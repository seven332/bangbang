# macOS Host Security Model

This document describes the current host security posture for bangbang. It is a
baseline for review and future work, not a claim that bangbang already provides
Firecracker's full production isolation model on macOS.

## Security Boundary

Direct mode follows Firecracker's one-VMM-process-per-microVM model. One
`bangbang` process owns one API socket, one VMM controller, one HVF-backed
startup path, and the host resources configured for that microVM. Production
bundle mode adds one outer supervisor and one authenticated private process
session while retaining exactly one sandbox worker and one VMM ownership domain
per invocation.

Direct startup uses non-clobbering fd-table preallocation as a Firecracker-style
performance guard. Failing to read the descriptor limit or duplicate a
descriptor is non-fatal; failing to close a successfully duplicated descriptor
is fatal. The setup does not overwrite inherited high-numbered descriptors.
Production launch instead uses Darwin's default-close spawn mode and explicitly
retains only open standard streams plus fixed private lifecycle, startup-grant,
vsock-broker, and vhost-user-broker descriptors.

Direct mode trusts the host user account and local filesystem permissions around
configured paths. Production bundle mode additionally trusts its outer launcher,
fixed metadata, and signed nested worker, while App Sandbox limits that worker
to container/sealed resources plus an explicitly prepared startup grant batch
and the fixed launcher-owned vsock and vhost-user connection facets.
API clients, API request bodies,
guest-provided MMIO data, guest memory, and configured host paths remain
untrusted input in both modes.

There is no authentication on the HTTP-over-Unix-socket API. bangbang restricts
the published socket inode to owner-only permissions, and access control still
depends on the socket path and parent-directory permissions. Operators should
place the socket in a private directory on multi-user hosts.

## Firecracker Differences

Firecracker's Linux production model relies on mechanisms that do not directly
map to the current macOS/HVF scaffold:

- the `jailer` launcher
- seccomp filters
- Linux namespaces
- cgroups
- chroot setup
- privilege dropping after privileged resource preparation

bangbang rejects Linux-specific Firecracker process options rather than
silently accepting them. The production bundle now supplies an unprivileged
macOS outcome for the jailer's fixed executable/current-user identity, private
working root, closed environment and descriptors, exact file/descriptor limits,
and foreground/detached lifecycle ownership. It uses a signed App Sandbox
launcher/worker boundary plus bounded per-VM lifecycle, startup-grant, daemon
handoff, and socket-broker channels. Contained startup config, startup metadata, kernel, and initrd inputs
adopt exact read-only grants; block and pmem devices adopt exact repeatable
read-only/read-write backing grants; logger, metrics, and serial adopt exact
singleton write-only sink grants; vhost-user endpoints require repeatable
connect-only directory grants and launcher-returned streams. Arbitrary uid/gid transition, configurable
chroot ownership, and complete distribution-signing policy remain absent. The
exact seccomp, cgroup, network-namespace, and PID-namespace mechanisms are now
certified public-macOS exclusions rather than unresolved or silently accepted
inputs.

Apple App Sandbox is a supportable containment building block, not a direct
jailer port. The lower-level signed target packages real binaries as minimal
apps and proves the complete HVF lifecycle plus container allow/deny behavior.
The production target separately proves the fixed outer app and nested worker,
exact entitlement split, static and dynamic code validation, descriptor closure,
bounded protocol rejection, signal cancellation, both surviving-process cleanup
directions, empty-namespace both-killed recovery, concurrent namespace isolation, owned socket
cleanup, typed startup grant allow/deny behavior, an outside-container granted
API socket, both real granted-vsock initiation directions, and a real sandboxed guest.
The direct CLI remains an ordinary non-sandboxed executable. Production can
commit typed startup authority, and its config, metadata, kernel, initrd,
block, pmem, logger, metrics, serial, snapshot input/output/root, API-socket,
and vsock-socket consumers use granted identities or exact retained anchors
without reopening their tagged path strings. General dynamic post-Ready
delivery and hard revocation still need dedicated designs.

## Certified Linux Runtime Isolation Exclusions

Firecracker v1.16 installs a nonempty classic-BPF program on each `vmm`, `api`,
and `vcpu` Linux thread after `PR_SET_NO_NEW_PRIVS`; `--no-seccomp` replaces the
default programs with empty programs, while `--seccomp-filter` loads the caller's
bounded map. Current public macOS exposes neither Linux `seccomp` installation
nor an equivalent caller-defined per-thread syscall-return policy. Direct
bangbang already has no Linux filter, so accepting `--no-seccomp` as a no-op
would falsely report a default-to-empty transition. App Sandbox is a fixed
signed resource boundary, private Seatbelt policy is unsupported, and Endpoint
Security is privileged event monitoring rather than an every-syscall in-process
filter.

Firecracker's jailer cgroup inputs select v1 or v2 controller hierarchies, write
arbitrary controller files, inherit/enable parents, and attach the process via
`tasks` or `cgroup.procs`. Darwin rlimits are inherited scalar process limits
(`RLIMIT_NPROC` is per user), not group identity, hierarchy, delegation,
controller files, or arbitrary PID placement. App Sandbox, launchd classes,
nice, and QoS do not supply that contract.

`--netns` opens a named Linux namespace handle without following its final
symlink and calls `setns(CLONE_NEWNET)` before later jail setup. Network
Extension packet tunnels are entitled VPN extensions, and vmnet provides guest
networking; neither joins the host launcher/worker to a caller-selected network
stack. `--new-pid-ns` uses `clone(CLONE_NEWPID)` so the first child is PID 1 in a
nested visibility domain. Darwin process groups, sessions, monitoring, and
supervisor ownership keep host PIDs and cannot reproduce that identity.

The executable rejects `--no-seccomp` and every shape of `--seccomp-filter`
before filter-path access, configuration-file access, VMM/backend construction,
readiness, or API socket publication. Its error contains only the first fixed
unsupported name. The production launch-policy parser similarly recognizes
only exact pre-delimiter `--cgroup`, `--cgroup-version`, `--parent-cgroup`,
`--netns`, and `--new-pid-ns` names, including attached forms, and returns a
closed typed error before grant parsing/preparation, bundle/profile selection,
private staging, session creation, spawn, publication, or worker execution. It
does not consume a following value or reinterpret post-delimiter worker argv.

Unit and direct process tests cover missing, separated, attached, duplicate,
conflicting, lookalike, and delimiter cases with fixed Debug/Display/stdout/
stderr redaction. The real separately signed production bundle combines each
jailer rejection with private invalid grant and socket inputs and proves empty
stdout, exact stderr, no socket, and an unchanged session directory. Those
tests certify rejection ordering; they do not turn App Sandbox, rlimits, vmnet,
sessions, or supervision into Linux-isolation substitutes.

## Platform-Limit Taxonomy

Use this taxonomy with the status vocabulary in
[Firecracker Validation Matrix](firecracker-validation-matrix.md) when a PR
changes Firecracker-facing behavior or security posture:

- Linux-only hardening: Firecracker behavior that depends on jailer, seccomp,
  namespaces, cgroups, chroot, or post-setup privilege dropping is
  `platform-limited` until bangbang has a macOS replacement. Matching CLI flags
  or API inputs should be rejected or documented as unsupported instead of
  accepted as no-ops.
- macOS/HVF host-facility limits: behavior blocked by Hypervisor.framework,
  code-signing, entitlement, vmnet, filesystem, or other host APIs is
  `platform-limited` only when the missing macOS/HVF facility is the blocker.
  Record the concrete macOS/HVF reason and any required external launcher,
  entitlement, or operator setup.
- Validation-environment limits: CI or developer hosts that cannot execute HVF
  change the validation layer, not the support status, unless the same limit
  applies to real macOS hosts. Use explicit compile/sign-only validation such as
  `--allow-unsupported` for those runners.
- Implementation deferrals: behavior that is feasible on macOS/HVF but not
  built yet is `deferred` or `partial`, not `platform-limited`. Keep a related
  issue for the missing implementation, tests, and documentation.
- Recognized unsupported shapes: parsed Firecracker endpoints, flags, or fields
  that intentionally return a Firecracker-shaped fault without mutating state
  are `recognized unsupported`. Add parser/state tests and process e2e coverage
  when the public process boundary is affected.
- Operator-owned policy: socket-directory permissions, host-path ownership, and
  current resource-sharing rules remain deployment assumptions. Startup grants
  now preauthorize closed roles. Config, metadata, kernel, initrd, block, pmem,
  logger, metrics, serial, API-socket, and vsock-socket consumers have explicit
  typed adoption; each remaining consumer still needs its own resource-specific
  mutation and cleanup policy.
  Document the assumption and test that one `bangbang` process does not clean
  up resources it no longer owns.

When a capability moves between these categories, update the compatibility docs,
validation matrix, tests, and related issue links in the same PR.

## Machine Memory and Exact 2M Boundary

Machine configuration is a transactional pre-boot security boundary. JSON
syntax and representation errors stay in the parser; representable semantic
values reach one runtime candidate validator. Numeric faults do not retain or
echo the submitted value, and a failed PUT/PATCH preserves both prior machine
configuration and balloon target.

Bangbang accepts `mem_size_mib` only through the 1022-GiB aarch64 DRAM maximum
and rejects larger values before storage. Every successful GET value is the
value used for balloon checks, guest-memory allocation, FDT, HVF mapping, and
native-v1 snapshot length. Startup keeps an independent defensive maximum for
unchecked state. This avoids silently accepting a configuration the guest does
not receive. It does not promise current or future host-free-memory
availability: anonymous no-reserve mappings and changing host pressure make
such a preflight unreliable, and normal allocation/mapping failures remain
failure-atomic startup outcomes.

Firecracker's `huge_pages = "2M"` requires exact Linux hugetlbfs backing.
Public XNU arm/SPTM and Hypervisor.framework do not provide that contract.
Bangbang rejects the known enum before allocation or HVF construction with a
fixed value-redacted platform fault. It does not substitute virtual alignment,
2-MiB batching, the 16-KiB host page/IPA granule, a private API, root-only host
configuration, a Linux sidecar, or a new entitlement. Ordinary allocation,
mapping, protection, alignment, balloon discard, and resource exhaustion are
not classified as impossible. The complete sources, local probe result,
alternatives, tests, and Challenge references are in the checked
[machine-memory contract](../compat/firecracker/v1.16.0/machine-memory-contract.md).

The internal descriptor-backed guest-memory profile is opt-in and the public
default remains private anonymous memory. Shared regions use exact-sized `0600`
files with unpredictable names, unlink the name before mapping/publication,
retain close-on-exec descriptors, and preflight `RLIMIT_FSIZE` and
`RLIMIT_NOFILE` before VM publication. Exports clone only checked
descriptor/offset/length metadata and redact descriptor numbers, host
addresses, and transient names. A dynamic-memory VM reserves its complete
validated virtio-mem aperture as one sparse shared object even when it starts
without vhost. That reservation is descriptor authority, not guest RAM
admission: offline bytes are absent from CPU/HVF mappings, FDT RAM, dirty
metadata, byte access, current-memory accounting, and public plugged size.
Online blocks are exact offset views and unplug reclaims the corresponding file
range only after the guest-visible transaction commits.

Darwin has no Linux `memfd` seal parity. Direct startup or runtime insertion
therefore treats an operator-selected vhost-user block backend as a trusted
confidentiality, integrity, and availability boundary: after bounded
negotiation, activation transfers boot-RAM descriptors plus, when configured,
the one complete hotplug aperture and only the queue call/kick descriptors
required by the protocol. This immutable arm64 table contains at most three
memory regions and no unrelated mapping. The backend can read or write
currently unplugged aperture bytes even though guest CPUs cannot access them,
and descriptor close is lifetime cleanup rather than hard revocation. Bangbang
does not bundle a production vhost backend or claim backend jailer, cache, rate, or security
policy; those remain operator-owned. Ordinary-only VMs keep anonymous RAM.
Contained workers reject ambient socket paths and instead require an exact
retained connect-only directory grant plus a session/sequence/grant/child-bound
stream from the launcher. The launcher validates the target relative to the
retained anchor without granting the worker ambient network authority.
Native-v1 capture still rejects vhost before artifact staging. Runtime direct
or contained insertion requires an already-shared live profile and validates
PCI, inventory, MMIO-region, interrupt, and metrics capacity before connecting
or claiming a child; dynamic-memory startup supplies that eligible profile.
Anonymous-profile, duplicate, root, disabled-PCI, and exhausted requests fail
before a direct connection or contained broker request. Backend death or
DELETE drops device-owned clones and leases without changing the VM-owned
aperture; shutdown releases it after all active views and devices are gone.

## Isolation Compatibility Checklist

Use this checklist when reviewing Firecracker-facing host isolation changes:

| Area | Current status | Review expectation |
| --- | --- | --- |
| Linux jailer, seccomp, namespaces, cgroups, chroot, and privilege dropping | Direct mechanisms unsupported; fixed-code/current-user/private-root/rlimit/daemon observable subset implemented | Preserve the exact macOS launch-policy contract and reject unsupported Linux controls. Do not equate current-user checks with arbitrary uid/gid transition or a private cwd with chroot. |
| API socket ownership | Implemented subset | Keep owner-only socket permissions, final-path ownership checks, and owner-only cleanup tests current when API socket behavior changes. |
| Host path policy | Operator-owned with per-resource validation | Redact sensitive path details in errors, avoid opening paths during pre-boot storage unless the resource explicitly requires it, and test cleanup for owned resources. |
| HVF entitlement and code signing | Implemented direct, App Sandbox, and production nested-worker validation paths | Keep real HVF tests in signed targets, inspect entitlement separation and nested signatures, and keep unsupported CI hosts on explicit compile/sign-only validation, not silent skips. |
| Network and vmnet | Implemented virtio-MMIO/all-PCI startup plus PCI-only runtime PUT/DELETE; direct vmnet conditional; contained lifecycle-v5 authority and closed networkless/vmnet signing profiles enforced | Keep supported `host_dev_name` forms, exact mode/bridge/actual-live-count admission, per-entry cleanup, MMDS-only behavior, entitlement/profile requirements, and non-goals documented when network behavior changes. |
| macOS App Sandbox | Production nested worker implemented for container/sealed resources plus granted config, metadata, kernel, initrd, block, pmem, logger, metrics, serial, API-socket, vsock-socket, and connect-only vhost-user-socket resources | Keep the ordinary CLI explicitly uncontained and prove package identity plus real ungranted denial and granted operation behavior without adding ambient network authority. |
| Launcher and resource broker | Authenticated lifecycle v5 credential/resource-limit/vmnet policy, closed exec environment and descriptor set, bounded atomic startup grants, signed daemon handoff, adopted file/directory/block-special consumers, and separate fixed session-bound vsock, vhost-user, and retained-descriptor block-control facets implemented; general-purpose brokerage missing | Require exact policy/profile/role/access/anchor/identity checks, retained session authority with per-device leases, closed session/sequence/rights framing, redaction, and cooperative lifetime. Do not describe sender close as revocation or let consumers fall back to ambient paths. |

## Native Snapshot Composite and Device Boundary

Every native snapshot layer remains untrusted even after its outer CRC passes.
The kind-2 `BANGCMT\0` record binds the complete memory image to a bounded
`BANGHVF\0` value with exactly five ordered components. Decode rejects unknown,
missing, duplicate, reordered, truncated, oversized, flagged, inconsistent, or
trailing data before constructing a bundle. The fixed cross-checks cover
machine memory and GPA layout, CPU/MPIDR and optional-feature policy, one
same-default-configuration cache manifest, GIC topology, PL031 mapping, and
nested device ranges. Hypervisor.framework's GIC blob is opaque and capped
before allocation; neither its embedded format nor acceptance after a host
update is trusted.

A complete bundle contains guest memory, general/system/SIMD register state,
pointer-authentication keys, device paths and backing identity, limiter time,
and opaque GIC bytes. Those values are confidential VM state. `Debug`, errors,
logs, and metrics may expose only stable categories, stages, and bounded byte
counts; they must never expose raw registers, keys, paths, guest addresses,
image IDs, checksums, guest bytes, or GIC contents. CRC-64/Jones and random
image identity detect accidental corruption or mismatched pairs, not malicious
rewriting, confidentiality, provenance, or authorization.

Native-v1 publication holds paused-worker admission, block/PMEM/network/entropy
retry quiescence, and all four runner operation domains through non-memory
encoding, complete memory streaming, artifact verification and synchronization,
exclusive memory-first/state-last commit, and the successful-publication hook.
Cancellation is checked between fixed stages and 1 MiB chunks and competes with
one atomic commit seal. Before the seal it returns no binding or bundle,
publishes no final state marker, and drops the consumed writer and auxiliary
guard before admission release. After the seal, signal-triggered shutdown stays
pending while publication finishes and retains its exact typed visibility and
cleanup result.

Rust cannot forcibly preempt an arbitrary blocking `write`; the public request
path therefore supplies only a publisher-owned regular staging file, never an
arbitrary caller writer. The capture writer names no path and the publisher owns
cleanup of its private staging entry. A partially written staging inode is never
interpreted as committed state. Signal handlers only update lock-free atomics
and the existing wakeup pipe; they do not allocate, lock, perform artifact I/O,
or run cleanup in signal context.

PL031 RTC is represented by fixed MMIO metadata and an explicit fresh-device
policy. No mutable RTC register or alarm state is persisted, so no continuity
claim is permitted. Active SVE/SME or breakpoint/watchpoint state is rejected
rather than silently omitted, and optional devices remain outside the accepted
profile.

The #1481 aggregate preflight traverses balloon, memory-hotplug, entropy,
serial, and time/identity state in one fixed order before the existing
optional-profile rejection. Injected failures stop at the named stage, preserve the
paused configuration, publish no artifact, and permit same-session retry after
complete cleanup. The captured values remain private validation objects: they
do not authorize host endpoints, encode optional devices, or establish restore,
clone, migration, or cross-host portability. Those responsibilities remain
with Wave 6 #1490.

The internal native-v1 device profile is untrusted input even when its outer
state file passed length and CRC checks; CRC detects accidental corruption and
is not authentication. The standalone `BANGDEV\0` decoder caps the complete
value at 16 KiB, bounds every string before allocation, requires exact schema
and EOF, and keeps paths, IDs, stat identity, guest addresses, features,
cursors, limiter values, and VMGenID bytes out of diagnostics.

The supported root disk remains an operator-managed external resource. Capture
and load open the final path read-only with nonblocking, close-on-exec, and
no-follow flags, require a regular file, derive identity from the opened
descriptor, and compare device/inode, length, mode, mtime, and ctime. Load
retains that exact descriptor after preflight, so a later pathname replacement
does not retarget the prepared device. This is a same-host compatibility check,
not content authentication: an actor allowed to mutate the already-open inode
can still alter guest-visible disk contents, and parent-directory symlink or
mount policy remains part of the operator-owned filesystem boundary.

Device preparation is deliberately off-side and drop-safe. It may read loaded
guest memory and open the root backing, but it does not modify guest memory,
an MMIO dispatcher, an HVF VM, controller state, or retry schedulers. UART
output bytes, locks, metrics, files, and limiter clocks are never deserialized
as live handles; the supported serial policy creates a new empty buffer and
metrics owner. Source VMGenID bytes are not encoded as reusable identity and
must be replaced and signaled through the separate never-run restore stage.

The native-v1 loader completes bundle, platform, memory, cache, root,
and baseline-device validation before creating an HVF VM. Runtime installation
consumes the validated block and UART owners, creates a fresh RTC, and leaves
the loaded guest bytes untouched; it does not reload a kernel, rewrite an FDT,
or configure boot registers. Destination CPU identity, optional-state evidence,
MPIDR, and GIC metadata are exact local compatibility checks rather than a
cross-host portability or artifact-authentication boundary.

VM creation begins the nontransactional restore boundary. One never-run runner
command validates every destination-derived value before its first setter and
then applies architecture, opaque GIC, ICC, normalized timer, and pending
interrupt state in a fixed order. VMGenID replacement and edge notification
follow that state. Any failure tears down the scheduler, runner, mapped memory,
and VM; a same-process retry is reported only when explicit cleanup evidence is
complete, while uncertain cleanup latches the private process load path as
terminal. Errors expose stages and categories, not paths, register values,
opaque bytes, identities, or guest contents.

The restored session is handed to a worker whose pause gate is closed before it
can receive the session. Controller and process ownership commit only after
that handoff, always as `Paused`. Public `PUT /snapshot/load` reaches this
transaction only after pristine-request and committed-pair validation;
`resume_vm: true` then uses the ordinary resume path. Public create likewise
uses the production publisher/capture transaction only after paused-profile and
namespace preflight.

## macOS Isolation Design Boundaries

bangbang has two explicit execution modes. The direct CLI is one uncontained
macOS process running as the invoking host user; its controls are the host user
account, filesystem permissions, API socket directory, and per-resource
validation. The production bundle has an unsandboxed outer launcher and one
separately signed nested VMM worker constrained by App Sandbox. The launcher has
no Hypervisor or App Sandbox entitlement and the worker has exactly both. This
is a real deployed containment boundary, but it is not Firecracker's Linux
jailer. It now preauthorizes bounded startup resources; existing public path
consumers for startup config, startup metadata, kernel, initrd, block, pmem,
logger, metrics, serial, snapshots, API sockets, and vsock sockets adopt them.

Use the following boundaries when designing or reviewing macOS isolation work:

| Boundary or option | Current behavior | Future direction |
| --- | --- | --- |
| Operator-owned private directories | Required for API sockets, vsock sockets, vhost-user sockets, observability sinks, and other configured paths that should not be shared. Contained API/vsock use requires one exact preauthorized create-children directory and safe child; contained vhost-user use requires one exact preauthorized connect-only directory and safe child; direct paths remain operator-owned. | Cross-launcher name allocation and sharing policy remain operator responsibilities. |
| HVF entitlement and code signing | The production worker alone receives the Hypervisor entitlement; the outer launcher cannot enter HVF. Both code objects use Hardened Runtime and are separately inspectable. | Developer ID possession, team policy, launch constraints, and notarization still require deployment evidence. |
| macOS App Sandbox | The production worker is sandboxed; the ordinary direct CLI and outer launcher are not. Container/sealed resources plus granted config, metadata, kernel, initrd, block, pmem, logger, metrics, serial, snapshot, API-socket, vsock-socket, and connect-only vhost-user-socket authority form the current contained mode. Lifecycle v5 binds vmnet policy to exact networkless or caller-approved vmnet signature profiles. | The real restricted-entitlement credential and connectivity evidence remain operator-owned gates; general dynamic delivery still requires explicit design. |
| Launcher or resource broker | The production launcher validates fixed/live nested code, starts one closed-environment/default-close worker, authenticates lifecycle v5 credential/resource-limit/vmnet policy, applies worker-local limits before `Prepared`, owns cancellation/status, coordinates and enters the private namespace, atomically transfers a bounded typed startup batch, supports adopted file/directory/block-special consumers, offers signed daemon detach, and exposes separate fixed vsock, vhost-user, and retained-descriptor block-control facets. | Keep each private protocol fixed and redacted; separately challenge any broader dynamic broker and never infer hard revocation from closing a duplicate descriptor. |
| Firecracker Linux jailer model | Direct port unsupported; exact fixed executable/current-user/rlimit/version/daemon outcomes implemented through the versioned macOS policy envelope. | Keep arbitrary uid/gid, configurable chroot, seccomp, namespaces, cgroups, and parent-cgroup controls rejected until separately challenged macOS outcomes exist. |

This document intentionally does not define a sandbox profile, broker protocol,
privilege-dropping flow, or new public API. PRs that add host resource types
should state which current boundary protects the resource and whether a future
launcher, broker, or sandbox profile would need to own it.

## Startup Grant Authority

Only an exact argv-position-one grant envelope, or the same envelope immediately
after a `--bangbang-jailer-v1 ... --` delimiter, activates grants:
`--bangbang-grant-manifest MANIFEST -- FIRECRACKER_ARGS...`. Otherwise the
launcher preserves every non-policy worker argument byte. The strict JSON manifest is read
once from a no-follow regular-file descriptor and never accepts descriptors or
bookmark bytes from the operator. Its closed roles are read-only startup
config/metadata, kernel/initrd and snapshot inputs; read-only or read-write
repeatable drive/pmem backing; write-only logger/metrics/serial sinks; and
create-children API/vsock/snapshot-output directories. Unknown or duplicate
fields, IDs, singleton roles, invalid access pairs, nonabsolute or ambiguous
paths, aliases, missing objects, symlinks, and excessive input fail before spawn
without exposing paths or IDs. Every file role requires a regular file except
`DriveBacking`, which additionally accepts one exact macOS block-special node;
directories, FIFOs, sockets, character devices, and unsupported object kinds
remain rejected for that role.

Resource paths are walked from an owned root descriptor with one-component
`openat` calls, `O_NOFOLLOW`, no creation, and a temporary nonblocking probe so
a special file cannot stall preparation. Exact fstat type/device/inode and
F_GETFL access/status flags are recorded. The complete RAII batch is prepared
before spawn; any failure drops every opened descriptor. Current hard limits are
256 KiB manifest data, 64 grants, 64 identifier bytes, 255 UTF-8 bytes per
snapshot output child, 4096 source-path bytes, 512 records, 1024 encoded bytes
per datagram, 64 KiB per bookmark, and 256 KiB aggregate bookmark material.

Lifecycle protocol v5 uses descriptor 3 and carries one fixed reserved-zero
worker policy only after peer authentication. A separate connected unnamed Darwin
datagram socket at descriptor 4 carries BBG2 grant-channel records. A third
connected unnamed datagram socket at descriptor 5 carries the dormant closed
vsock broker protocol. Descriptor 6 carries the independent closed vhost-user
connection protocol; neither broker is a general grant channel. Every grant record
binds the random lifecycle SessionId, an independent random BatchId, exact
sequence, closed kind, payload length, reserved fields, and declared descriptor
count. `Begin` declares exact batch bounds; regular-file, block-special, and
directory records carry one SCM_RIGHTS descriptor; bookmark fragments are contiguous and bounded;
and `Commit` must reproduce the declaration. The launcher sends nonblocking in
the same kqueue that observes signals, lifecycle input, worker exit, and one
absolute five-second send-plus-acknowledgment deadline.

Descriptor 7 carries the independent fixed 256-byte `BBC1` block-control
protocol. It accepts only `Inspect` and `SynchronizeCache` for a block-special
`DriveBacking` already present in the immutable grant batch. Requests and
responses bind the lifecycle SessionId, a nonzero monotonic sequence, exact
grant ID, access, normalized status flags, device/inode/rdev, and adopted
geometry. The worker applies a two-second deadline and accepts no ancillary
rights; stale sequence/session/identity, malformed framing, unexpected rights,
timeout, disconnect, or ambiguous reply poisons the facet. This is not a path
service or generic ioctl broker.

The worker receives each datagram with fixed payload/control buffers, immediately
owns every delivered fd, rejects payload/control truncation and malformed or
unexpected cmsghdr data, restores FD_CLOEXEC, and independently verifies access,
flags, type, device, and inode. Missing, extra, reordered, replayed,
cross-session/batch, overlapping, partial, or late records poison the entire
staging batch and drop its authority. No lookup exists before exact `Commit`.
Only after all values move atomically into the session registry does the worker
send an exact redacted `GrantsAccepted`; `Proceed` is invalid before that ack,
including for an empty batch. Cancellation remains valid throughout staging and
the acknowledgment wait.

Regular-file grants expose descriptors only. A block-special drive grant uses
the BBG2 record to carry its exact rdev, access/status, logical block size,
block count, checked capacity, and descriptor; the worker independently rechecks
fstat/fcntl identity and accepts geometry only from the authenticated atomic
batch. A scoped-directory grant combines
a read-only anchor descriptor with a freshly minted ordinary implicit bookmark.
The worker resolves it without UI or mounting, explicitly starts scope, reopens
the directory without following symlinks, checks exact anchor identity and
role-specific access, and retains scope plus anchor as one registry value.
API/vsock/snapshot output roles require create/search access; the connect-only
vhost-user role requires search without write authority. The
platform stale bit is private and is not by itself rejection: concrete
resolution, scope acquisition, identity, and access must all succeed. Bookmark
material is never persisted, renewed, logged, or supplied by the operator.

Registry adoption is one-time and requires exact ID, role, and access; mismatch
never falls back to an ambient path. Unadopted values drop on cancellation,
terminal exit, disconnect, bootstrap failure, or process exit. SCM_RIGHTS creates
an independent descriptor reference, so closing the launcher's copy is cleanup,
not revocation. The initial grant batch remains immutable after acknowledgment.
General-purpose dynamic grants and hard revocation require a later broker
design. The fixed vsock and vhost-user facets derive only connected streams
from authority already fixed in the immutable startup batch.

Contained mode recognizes only the exact, case-sensitive
`bangbang-grant:<GrantId>` form. The direct CLI treats the same text as an
ordinary pathname. Config and metadata claims must match their singleton role
and read-only access, then use the existing bounded regular-file readers on the
adopted descriptors. Kernel and optional initrd claims are validated and removed
from the registry as one failure-atomic batch when boot-source configuration is
applied. A malformed, missing, mismatched, or already-consumed tagged claim
fails without changing public VM configuration and never falls back to the tag
as a pathname. Mixed boot sources claim only their tagged members; ordinary
members retain their prior deferred-path behavior.

Prepared kernel and initrd descriptors are stored beside the public boot-source
configuration and consumed once by `InstanceStart`; the loader never reopens the
tag strings. `GET /vm/config` may return those references as authorized
configuration output, while errors and logs remain redacted. Preflight failures
before descriptor consumption remain retryable. Once boot consumes a singleton
grant, a later boot failure requires a fresh contained launch and grant batch,
unless the boot source is successfully replaced with ordinary paths. The
session's file authority is synchronized across the control reader and API
worker so cancellation, terminal exit, or disconnect invalidates every pending
file claim. Already adopted descriptors are cooperatively owned resources, not
hard-revocable handles.

Repeatable block and pmem claims use the same exact tag grammar but bind
`DriveBacking` or `PmemBacking` plus access derived from the immutable device
configuration. Read-only devices require read-only descriptors; writable
devices require read-write descriptors. Complete request and lifecycle
validation precedes a claim. A successful pre-boot same-ID `PUT` atomically
replaces public configuration and the retained opened backing; an ordinary path
removes retained private authority and preserves the historical deferred-open
timing. Startup preflights every prepared/consumed entry before moving any file
into the VM resource bundle, then matches provided backings by exact device ID
and rechecks that their logical read-only mode matches the immutable device
configuration. Once moved, a later startup failure leaves that device's grant
consumed and a fresh same-ID `PUT` is required.

After start, only the existing path-changing block `PATCH` may claim another
preauthorized drive grant. The replacement is validated and opened before the
active handler swap; public configuration commits only after that swap. If a
later active-session step fails, the old device/configuration survives but the
new claim remains consumed. Path-free block limiter updates and pmem limiter
updates retain the active backing and claim nothing. Pmem has no live backing
replacement surface. Owner-authorized configuration responses may return tag
values; errors, faults, logs, and debug output redact tags, IDs, paths,
descriptor identities, and contents.

Direct drive preparation uses public macOS `DKIOCGETBLOCKSIZE` and
`DKIOCGETBLOCKCOUNT` on the exact opened block descriptor and
`DKIOCSYNCHRONIZECACHE` for Writeback flush. The signed App Sandbox worker can
pread/pwrite the transferred descriptor but receives `EPERM` for those disk
ioctls, so contained drive ownership delegates only fresh geometry inspection
and cache synchronization to the launcher's retained copy through descriptor
7. The launcher re-fstats and rechecks access/status before every operation,
acts only on the exact grant identity, and never reopens or enumerates a device
path. Capture performs a fresh broker inspection and compares the complete
adopted tuple; mismatch, timeout, peer failure, or ioctl failure is fail-closed.
Native-v1 remains regular-file-only and rejects block-special capture before
artifact publication.

Logger, metrics, and serial references use the same exact grammar but require
their singleton role and `WriteOnly` access. Complete input and lifecycle
validation precedes each claim. Adopted regular files retain kernel-enforced
write-only access while the consumer sets and verifies append/nonblocking
status without reopening a path. Logger and metrics retain their sinks at
successful configuration; path-free logger updates claim nothing, retain the
current sink, and commit requested filter/presentation fields together.
Metrics rejects repeat initialization before inspecting or consuming another
reference and preserves its existing initial, periodic, explicit, and terminal
transaction behavior.

Serial retains one prepared output beside its public pre-boot configuration.
Clear or replacement drops it; startup moves it through the one-attempt VM
resource aggregate and never opens the submitted tag. Once handed to a startup
attempt it is consumed even if later startup fails, and a validated serial
reconfiguration is required before retry. Direct logger/metrics paths still
open at configuration, direct serial paths still open at startup, and their
existing creation and FIFO-like behavior is unchanged. Pending, replaced, and
active sink files close through ordinary process/session ownership; descriptor
delegation remains cooperative rather than hard-revocable.

Snapshot describe/state/memory file references use the exact contained input
grammar and distinct singleton read-only roles. Description inspects a duplicate
of the exact descriptor. Load duplicates only state for bounded decode, learns
any persisted grant-tagged root selector, then atomically takes every tagged
state, memory, and read-only `DriveBacking` input. Wrong, missing, duplicate,
cancelled, or mismatched authority consumes nothing. After a successful take,
the prepared state, anonymous memory, and supplied root backing complete restore
without reopening any submitted or persisted tag. A later failure retains the
existing snapshot retryable/terminal classification but does not restore the
one-time file grants. The root's captured identity includes device, inode,
length, mode, modification time, and status-change time; a metadata-changing
rename or replacement is intentionally not treated as unchanged authority.

Snapshot outputs use the separate exact contained reference
`bangbang-grant:<GrantId>/<SnapshotOutputChild>`. The child is one 1–255 byte
UTF-8 component, contains no NUL or `/`, and is neither `.` nor `..`.
`SnapshotOutputDirectory` is repeatable, and a retained anchor can serve
distinct state/memory children and later create requests. Complete request and
profile validation precede adoption. Staging creation, verification, barriers,
and exclusive finals remain relative to the retained anchors. Security-scoped
authorization remains tied to the directory's granted pathname on macOS;
moving the directory after scope activation can cause descriptor-relative
access checks to fail rather than broadening authority.

Each active granted staging file has one strict private record containing only
artifact kind, directory identity, its bounded random component, and file
identity. The worker records after create/fstat and before content, then clears
after publication or conclusive identity-safe cleanup. After worker reap, the
launcher matches its retained exact anchor and removes only a current-user
regular `0600`, single-link device/inode match. Missing, changed, or replaced
entries survive; the record is then cleared so cleanup cannot be retried against
a later occupant. A SIGKILL between staging creation and durable record, or
simultaneous uncatchable death of launcher and worker, can leave residue because
Darwin provides no unlink conditioned on an already-open inode.

API and vsock socket directories use a distinct exact contained reference:
`bangbang-grant:<GrantId>/<SocketChild>`. `SocketChild` must be one 1–64 byte
ASCII `[A-Za-z0-9._-]` component other than `.` or `..`; separators, traversal,
controls, non-ASCII, empty, or longer values fail without ambient fallback.
Direct mode continues treating the identical bytes as an ordinary path. Claims
are singleton, require exact `CreateChildren` role/access, and consume only
after complete consumer validation. No-API mode never claims the API role.

The owner thread retains the resolved scope and exact directory anchor. A
short-lived default-close instance of the already signed worker authenticates
its parent, receives only its control endpoint at fd5 and the exact private
namespace anchor at fd6, enters that anchor, binds one fixed role-specific
staging name, validates the listener, transfers its descriptor, and is killed
and reaped on every failure. The main worker verifies the namespace inode and
listener identity, writes one fixed bounded record containing only role, safe
child, and socket device/inode, and publishes the live vnode exclusively with
fd-relative `renameatx_np(RENAME_EXCL)` to the grant anchor. Cross-filesystem
publication, an existing target, symlink or pathname replacement, role/identity
mismatch, and extra rights fail closed and value-redacted. The binder is always
reaped before API readiness or VM-start success.

The supplied API listener preserves owner-only mode, no-clobber publication,
readiness timing, and identity-aware cleanup. The supplied vsock main listener
is retained through the VM lifetime and serves host-initiated Firecracker-style
connections. For guest-initiated connections, contained vsock consumes the
fixed descriptor-5 broker endpoint exactly once. `Activate` binds lifecycle
SessionId, sequence 1, and the validated safe child; the launcher requires the
retained exact `VsockSocketDirectory` anchor, enters it with `fchdir`, rechecks
cwd identity, and cannot change the child afterward. Subsequent requests carry
only a monotonic sequence and `u32` port. The launcher constructs only relative
`<SocketChild>_<port>`, validates the target before and after a nonblocking Unix
connect, and returns at most one validated connected stream descriptor. Closed
framing, exact rights counts, peer PID, lifecycle state, shutdown, EOF, and
timeouts fail closed. The launcher receives no guest payload, grant ID,
bookmark, resolved path, arbitrary child, or general resource selector, and
the worker gains no `network.client` entitlement.

Contained vhost-user block uses the separate descriptor-6 facet throughout the
session. Each fixed 256-byte `BBU1` request binds lifecycle SessionId, nonzero
monotonic sequence, retained grant ID, and one bounded child. The launcher finds
only an exact `VhostUserSocketDirectory + ConnectChildren` anchor, saves its
current cwd by descriptor, enters and verifies that anchor, rejects symlinks and
non-current-user/non-socket/multi-link targets, performs one bounded
nonblocking relative connect, rechecks vnode and peer address, and explicitly
restores/verifies cwd before replying. `Connected` carries exactly one stream;
`Failed` carries a stable redacted category and permits a later request.
Malformed frames/rights, stale correlation, peer change, timeout, lifecycle
cancellation, or cwd-integrity failure poison the facet. One directory is
adopted and retained by grant ID for the contained session; each drive owns only
an exact child lease, so startup retry, multiple children, and post-DELETE
reinsertion do not reopen the manifest tag. Before the first startup request,
the worker validates every boot/file/pmem/serial/vsock/vhost grant dependency
and broker health. Runtime owner preflight precedes grant reservation and broker
I/O; ID-only PATCH uses the existing frontend, duplicate PUT is zero-request,
and DELETE drops the device lease while retaining session authority. No helper
proxies backend traffic and the worker receives no ambient path or outgoing
network entitlement.

The scope, anchor, listener, broker endpoint, ownership record, and cleanup
guard close through one session. Worker shutdown removes only the still-matching
socket. After worker exit, the launcher reads at most the two fixed strict
records and checks both the retained matching role anchor's final child and the
fixed private staging name. It removes only a socket with the recorded
device/inode and exact owner, mode, and link count before clearing the record
and namespace. Launcher-first and worker-first failure retain the existing
cooperative cleanup ordering. If both processes die through uncatchable signals
at the same time, the external socket name and private ownership record can
remain stale because Darwin has no unlink-on-final-close Unix socket; automatic
later recovery removes only empty session namespaces.

General dynamic post-Ready delivery, hard revocation, and cross-filesystem
socket publication do not yet consume or extend their declared authority.

## App Sandbox Validation Boundary

Apple documents Hypervisor.framework for entitled sandboxed user-space
processes. `scripts/run-integration-tests.sh --test app_sandbox` validates that
contract with real app bundles rather than adding an entitlement to a naked
command-line binary. One bundle reruns every `hvf_lifecycle` test. The other
launches the real VMM executable and proves both sides of the filesystem
boundary:

- `GET /` succeeds through an owner-cleaned Unix socket under the app
  container's `Data/tmp` directory.
- The default `/tmp/bangbang.socket` is denied with process-failure exit status
  `1`, without publishing readiness or echoing the path.
- A config file outside the container is denied with bad-configuration exit
  status `152`, without publishing readiness or echoing the path.

The test entitlements contain only `com.apple.security.app-sandbox` and
`com.apple.security.hypervisor`. They do not grant vmnet, arbitrary files,
full-disk access, or a private sandbox profile. Apple requires user-selected
access or security-scoped URLs/bookmarks for many external files; a production
sandboxed VMM therefore needs either container/sealed resources or a separately
designed launcher/broker that transfers authorized resources. The production
bundle now includes the bounded startup transfer described above; the public
build wrapper still embeds no guest resources. The production worker adopts
grants for config, metadata, kernel, initrd, regular or block-special drives,
pmem, logger, metrics, and serial. The exact retained-descriptor block-control
facet above exists only because App Sandbox denies the required public disk
ioctls; remaining path consumers have not adopted the registry, and no general
dynamic broker or hard revocation is claimed.

## Production Bundle and Signed Worker Boundary

`scripts/build-production-bundle.sh` produces exactly one fixed topology:

```text
Bangbang.app                         identifier dev.bangbang
└── Contents
    ├── Info.plist
    ├── MacOS/bangbang              outer launcher
    └── Helpers/BangbangWorker.app  identifier dev.bangbang.worker
        └── Contents
            ├── Info.plist
            └── MacOS/bangbang-worker
```

The package tool accepts already built regular launcher and worker files, an
absent final `Bangbang.app`, and one signing identity. It assembles a private
mode-0700 sibling staging tree, signs the worker first with exactly App Sandbox
and Hypervisor entitlements, signs the outer app last without an entitlement
file, and requires Hardened Runtime on both. It inspects plist identity and
executable fields, signatures, entitlement separation, and strict recursive
validity before a same-volume exclusive rename publishes the final bundle.
Existing destinations are never replaced or merged. Failed assembly removes
only the private unpublished staging tree; tool output, identities, paths, and
worker data are omitted from product errors.

At runtime the launcher derives the worker from its own exact bundle location;
there is no working-directory or user-path override. It rejects symlinked,
missing, nonregular, wrongly identified, or invalidly signed code, any outer
entitlement, and any worker entitlement dictionary other than exactly App
Sandbox and Hypervisor set to Boolean true. Security.framework validates the
outer and worker static requirements with strict, all-architecture, nested, and
symlink-restriction checks and requires Hardened Runtime. It also validates the
spawned worker's dynamic code by PID while that process is suspended and again
after the resumed bootstrap has used its session endpoint. The requirements do
not anchor a certificate or Team ID, so they do not authenticate a wholly
replaced and separately validly signed package; Developer ID/team policy and
kernel launch constraints remain deployment work.

The launcher preserves non-policy worker argument bytes and supplies an exec
environment containing only one private bootstrap marker. Direct Darwin `posix_spawn` uses
`CLOEXEC_DEFAULT | START_SUSPENDED`; file actions retain each open standard
stream and duplicate exactly the lifecycle stream, startup-grant datagram,
dormant vsock-broker datagram, and vhost-user-broker datagram endpoints to fixed
internal descriptors 3, 4, 5, and 6. Unexpected inheritable
descriptors are closed in the worker image. Before `Start`, worker code can only
mark those descriptors close-on-exec, require
peer effective UID/GID to match, require `LOCAL_PEERPID == getppid()`, send one
no-payload `Hello` under the reserved all-zero identity, and block. The launcher
then checks the child-attributed peer PID/credentials and live code again before
sending a fresh random session identity and fixed redacted policy. The worker
removes the marker, checks real/effective identity and session state, installs
and reads back exact `RLIMIT_FSIZE`/`RLIMIT_NOFILE`, creates and descriptor-enters
the locked private namespace, and only then reports `Prepared`. App Sandbox denies the worker's
Security.framework lookup of its unsandboxed parent, so worker-to-launcher trust
is deliberately limited to the inherited endpoint, direct-parent PID,
credentials, exact sequences, and disconnect behavior; symmetric code-signing
authentication is not claimed.

Lifecycle protocol v5 uses a fixed endian-stable header, 256-bit session identity, exact
per-direction sequence, zero reserved fields, closed message kinds, fixed
payload shapes, and a 4096-byte frame cap. Wrong magic/version/reserved fields,
oversized or truncated input, unknown messages, replay/gap, cross-session data,
wrong sender, and invalid state fail with one redacted category before public or
VM work. `Hello`, `Start`, `Prepared`, exact `GrantsAccepted`, `Proceed`, `Starting`, optional committed
`Ready(Api|NoApi)`, at most one `Cancel(SIGINT|SIGTERM)`, and path-free
`Terminal(category, exit_code)` form the complete v5 lifecycle. `Start` alone
carries the current credential, exact limit, daemon state, and immutable
`VmnetAuthority`. The canonical network value is denied; a positive value has
closed host/shared bits, four zero-padded 15-byte ASCII bridge slots, and an
independent active maximum from 1 through 4. Unknown flags, duplicates,
inconsistent counts, malformed names, and nonzero unused bytes fail closed.
Structured exit
values must match the reaped public status; abrupt death may omit `Terminal`.
The initial `Hello`, `Start`, grant transaction, and `Proceed` reads use
absolute five-second deadlines, including across interrupted or fragmented
stream reads.

After `Start`, the worker creates and locks an empty mode-0700 directory named
only from the random identity beneath its fixed App Sandbox container temp root.
`Prepared` carries device/inode numbers, never a path. The launcher independently
derives the root from the current user's home and fixed worker identifier, opens
without following links, and checks exact name, type, effective owner, mode,
device, inode, emptiness, and the worker-held lock before sending `Proceed`.
The directory must be empty at this authorization gate. After `Proceed`, socket
publication may add only the fixed API/vsock ownership records described above;
they contain role, safe child, and socket identity, not an external path, grant
ID, bookmark, descriptor, payload, argument, or session value. Same-identifier
workers share the container, so the lock and identity checks preserve unrelated
or replaced cooperative sessions but do not defend against a malicious
same-bundle sibling with equivalent container authority.

Daemon mode introduces no new code identity or root authority. The original
launcher starts the same validated outer executable suspended with
`CLOEXEC_DEFAULT | SETSID`, `/dev/null` standard streams, one marker-only
environment, and only a fixed descriptor-6 handoff. A 40-byte reserved-zero
protocol authenticates parent/child code and kernel peer identity, transfers
timing, and publishes the supervisor PID only after worker readiness. Parent
EOF/signal before acknowledgment is cancellation authority; after exact PID
acknowledgment the handoff closes. The returned session-leading launcher stays
responsible for signals, worker reap, grants, sockets, and namespace cleanup.
No ambient PID-file path, orphaned worker, restart service, or privilege change
is introduced.

The launcher kqueue watches both graceful signals, the session stream, grant
socket writability, broker input, and the unreaped child. The first signal sends one bounded cancellation and starts a
five-second grace deadline; later signals are coalesced, and expiry kills only
the still-owned unreaped worker. A structured `Terminal` or session EOF starts
the same bounded process-exit grace, so a peer cannot report completion or
disconnect and then hold supervision indefinitely. Pending protocol bytes are
drained before a same-batch child reap, ordinary status is preserved, and
signaled status maps to `128 + signal`. Worker EOF cleanup handles
launcher-first death; launcher identity-checked cleanup handles worker-first
death; a later worker scans at most 128 names and removes only valid empty
unlocked identity-stable residue after both were killed. There is no automatic
restart or reconnect.

The outer launcher, fixed metadata, and signed nested code are trusted package
components. API requests, guest data, device input, host path arguments, and
HVF exits remain untrusted worker inputs. Container/sealed resources plus the
committed startup registry and fixed vsock connection facet are the current
contained authority. Granted config, metadata, kernel, initrd, block, pmem,
logger, metrics, serial, snapshot, API-socket, and vsock-socket resources are
consumed through their opened identities or exact retained anchors. vmnet is
not descriptor-brokered: contained acquisition is instead bounded by the
immutable lifecycle-v5 authority described below. General dynamic resources
remain unbrokered.

## vmnet Host Policy Boundary

bangbang's current live vmnet boundary is a direct macOS vmnet interface owned
by the VMM process. Network interface configuration stores the Firecracker
`host_dev_name` value before boot without opening host networking resources.
During `InstanceStart`, startup accepts only these vmnet-shaped names:

- `vmnet:host`, mapped to macOS vmnet host mode.
- `vmnet:shared`, mapped to macOS vmnet shared mode.
- `vmnet:bridged:<interface>`, mapped to macOS vmnet bridged mode. The
  interface suffix must be nonempty and must not contain NUL bytes or ASCII
  control characters.

Unsupported names fail before the VM reaches `Running`. When every configured
network interface is selected by MMDS config, startup still validates the same
vmnet-shaped names but can use process-local MMDS-only packet I/O without
opening vmnet resources. Otherwise, startup opens vmnet resources for the
configured interfaces, validates their returned profiles, and retains one
bounded lifecycle owner per handle inside the process. Drop stops an active
owner at most once; a failed or unconfirmed stop marks it uncertain and cannot
trigger another attempt or reuse.

Public PCI sessions retain those owners in a bounded per-interface registry.
Runtime PUT prepares a complete independent MMDS-only or vmnet entry, publishes
it immediately before the matching PCI endpoint on the VM owner thread, and
commits public configuration last. Existing entries are not rebuilt. Runtime
DELETE first makes the PCI endpoint reversibly unreachable, takes the exact
packet-I/O generation, and explicitly stops vmnet before endpoint commit.
Successful removal releases queue, callback/event, limiter retry, metrics,
MMDS detour or vmnet, and PCI ownership. An uncertain system vmnet stop, failed
owner restoration, or post-boundary failure is terminal; the process does not
claim a damaged network remains usable.

Contained mode adds a separate authenticated admission boundary without
changing direct mode. The outer jailer accepts repeated exact
`--vmnet-allow host|shared|bridged:<interface>` plus one required
`--vmnet-max-interfaces 1..=4`; the canonical default is deny. Host/shared may
appear once each, there are at most four unique bridge names, and production
bridge names are 1–15 ASCII bytes from `[A-Za-z0-9._-]`. This fixed value is
carried in lifecycle-v5 `Start`, retained immutably by `ContainedSession`, and
cannot be supplied through ordinary worker argv, environment, files,
descriptors, or a post-Ready message.

The policy is published to the VMM only after `Proceed`, paired with the exact
nonzero random lifecycle session identity. The process session and packet-I/O
registry retain that pair across startup and restore. Every entry, realized
backend profile, generation, callback/batch owner, and readiness bridge belongs
to the same pair. Debug and error output redact both identity and policy, and
the session identity is deliberately excluded from capture-ready state and
snapshot artifacts.

At final `InstanceStart`, after controller preflight but before grant
consumption or starter/backend construction, the worker parses every configured
mode/name. Complete MMDS coverage requires no authority and opens no vmnet.
Otherwise the complete configured set—not only non-MMDS interfaces—must fit the
active maximum and match the host/shared/bridge allowlist exactly. This timing
is deliberate: an interface must be configured before MMDS can name it, so a
PUT-time denial would make the same all-MMDS zero-resource configuration depend
on API ordering. Denial is a fieldless error that reveals no interface ID,
bridge name, count, limit, or session value.

After startup, contained runtime insertion applies the same immutable
session-and-authority pair on the owner thread. A different session is denied
before class selection even when it carries identical policy. MMDS-only entries
consume no vmnet capacity, but still require the exact lifecycle owner. A vmnet
entry must match the requested mode/bridge and fit the maximum after counting
actual live vmnet entries rather than all configured MMDS-only interfaces.
Paused capture checks the same owner before quiescing callbacks or traversing
metrics/backend state. Denial occurs before backend start or live publication
and leaves the configuration projection unchanged.

Static authority is a separate gate. Static and suspended/live code validation
classify only two closed shapes. `Networkless` is exactly Boolean App Sandbox
plus Hypervisor with no embedded provisioning profile and rejects every
nonempty authority. `Vmnet` is exactly those claims plus Boolean
`com.apple.vm.networking`, a bounded `<app-prefix>.dev.bangbang.worker`
application identifier, a bounded team identifier, and one nonempty bounded
regular `Contents/embedded.provisionprofile`; it rejects a denied authority.
Missing, false, malformed, developer-prefixed, or extra signature claims fail
before worker resume, and the prepared profile must remain identical across the
suspended and post-`Hello` checks.

Vmnet packaging is caller-credentialed and fail-closed. It opens the supplied
profile once without following its final symlink, captures at most 1 MiB, and
embeds only those bytes. Structural CMS decoding must yield single bounded App
ID-prefix and Team ID values, the fixed worker App ID, matching entitlement
team, a current validity window, one through 16 bounded developer
certificates, and Boolean `com.apple.vm.networking`. The sparse
`com.apple.developer.networking.vmnet` key is never treated as an alias or
copied into the code signature. The generated signature has exactly five keys,
and its actual leaf certificate must be one listed by the profile with the
expected team OU.

Neither CMS decoding nor ordinary `codesign --verify` proves contextual
restricted-entitlement authorization. Therefore vmnet build and the dedicated
nonpublishing preflight both sign a disposable copy of the already-running
package tool with the same fixed worker App ID, captured profile, five claims,
identity, and allowed leaf, inspect it, and execute only its immediate-success
private command with empty environment, null standard streams, neutral working
directory, and a five-second deadline. Publication is impossible before that
current-host AMFI gate succeeds. Networkless packaging creates no probe and
never executes the supplied worker; vmnet packaging also never executes that
worker.

The vmnet path requires the host to satisfy macOS vmnet authorization,
entitlement, and code-signing requirements. Apple's
[`com.apple.vm.networking`](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.vm.networking)
entitlement is restricted to virtualization developers. That authorization
allows a process to call vmnet APIs; it is not a guest containment boundary.
Operators remain responsible for host firewalling, routing, NAT exposure,
bridged-interface selection, and avoiding unintended sharing across multiple
`bangbang` processes. Use unique interface configurations and host resources
for separate VMs unless sharing is intentional and externally coordinated.

Apple's current [vmnet contract](https://developer.apple.com/documentation/vmnet)
returns the MAC address and MTU that the guest should use and documents limits
of 32 interfaces overall, four per guest operating system, and read/write calls
of at most 200 packets and 256 KB. The bangbang system backend copies, validates,
and retains the start-completion MAC, effective MTU, maximum packet size,
optional UUID, and optional batch limits before guest or runtime publication.
Configured and allocated realized MACs share one process-local reservation set;
requested API configuration remains distinct from the redacted realized
profile. Start and stop completions have finite deadlines. A returned handle is
either retained by one active owner, confirmed stopped once, or marked terminal
and uncertain; failed stop is never retried or reused. Diagnostics omit MAC,
UUID, bridge/interface names, returned bounds, XPC contents, raw handles, and
unknown framework values.

The backend registers only the packet-available event. Its serial callback
captures a restricted generation publisher, not guest memory, device queues,
packet bytes, configuration, limiters, or public metrics. It atomically retains
readiness and a bounded optional estimate, then uses a nonblocking capacity-one
signal; the estimate is only a hint and a full or disconnected signal path does
not clear readiness. A separate provider-owned bridge performs the potentially
locking vCPU wake outside Apple's callback queue. Owner-thread dispatch selects
the exact live generation, issues at most one realized host batch per pass, and
parks retained work while the guest has no RX buffer or a limiter is blocked.

RX and TX aggregate storage is allocated before interface publication and is
bounded by the validated profile, 200 packets, 256 KiB, per-packet size, and
virtqueue capacity. Read counts and packet lengths are treated as untrusted;
only a validated initialized prefix becomes visible. TX copies frames before
used-ring publication, commits them in descriptor order, preserves per-frame
MMDS effects, and maps a successful short write to an exact forwarded prefix;
the suffix fails without an ambiguous retry. An immutable backend profile
selects raw Ethernet or Apple's direct virtio-header envelope without using
that transport choice to authorize guest features. The portable packet layer
validates negotiated checksum/segmentation operations and normalizes every TX
frame before MMDS or vmnet sees it; direct RX accepts only the canonical
zero-offload header.

Teardown marks the exact generation retiring, disables its event, waits for a
bounded marker on the same serial callback queue, stops vmnet with its finite
completion wait, and only then drops callback, lease, interface, and wake
ownership. Disable, drain, or stop uncertainty is terminal and prevents
ID/MAC/PCI-slot reuse. The generic 16-interface configuration cap is not
enforcement of Apple's per-guest resource policy. Repository tests carry no
restricted credential fixture. They prove callback isolation, generation
retirement, batch/count validation, owner-loop routing, typed reconciliation,
and bounded lifecycle with injected backends; the positive packaging
transaction with synthetic tools; the real blocked preflight contract without
credentials; and signed MMDS-only/networkless runtime shapes. They do not prove
direct-vmnet guest connectivity.

Configured RX/TX token buckets are implemented as device-local queue admission
with retained work and session-owned retry wakeups. They are not packet
filters, a host firewall, or a NAT policy, and current signed limiter evidence
uses MMDS-only packet I/O rather than direct vmnet. The boundary still lacks
packet filtering, production network isolation, a repository-owned approved
credential and real contained vmnet evidence, network/MMDS snapshot encoding
and restoration, and full Firecracker public packet-movement parity. The
checked
[network and MMDS closure contract](../compat/firecracker/v1.16.0/network-mmds-contract.md)
separately pins the implemented deterministic capture-ready state and the
#1378/#1490/#1491 handoffs without broadening this security claim.

## API Socket Handling

The API socket is a local control interface with no protocol-level
authentication. Any process that can connect to the socket can send supported
API requests.

In direct mode, bangbang refuses to overwrite an existing final socket path. It
first binds a temporary sibling socket, records the socket device and inode,
restricts that socket inode to owner-only permissions, publishes it to the
requested path, and verifies that the published path still refers to that
socket. In contained mode the exact directory grant and safe child grammar
replace ambient path traversal: the transient binder creates the owner-only
socket in the private namespace, and the main worker publishes it exclusively
between exact directory anchors as described above. Both modes remove the path
on shutdown only when it still refers to the socket they created. Forced
termination can leave a stale path; in contained mode the surviving launcher
can clean an exact ownership record, but simultaneous uncatchable death of both
processes remains the documented stale-name and private-record window.

For multiple bangbang processes, use separate socket paths in directories whose
ownership and permissions match the intended control boundary. Do not share a
world-writable parent directory unless the sticky-bit and naming policy are
understood and acceptable for the deployment.

## Host File Paths

Host paths configured through the API are untrusted input. The current behavior
is resource-specific:

- `/boot-source` stores kernel and optional initrd paths or contained grant tags
  during configuration. Direct paths are opened later during `InstanceStart`
  with read-only nonblocking access. In contained mode exact tags instead claim
  matching read-only kernel/initrd descriptors during the successful `PUT` and
  move those files into startup without reopening the tags. Both paths reject
  inaccessible, non-regular, or empty payload files, and API-facing errors must
  not echo the configured path or tag.
- `/drives/{drive_id}` stores block backing paths during configuration. Direct
  regular files or macOS block-special nodes are opened later during
  `InstanceStart`. In contained mode an exact
  drive grant tag is claimed during a successful pre-boot `PUT` with access
  matching `is_read_only`, retained by drive ID, and handed to startup without
  reopening the tag. Same-ID replacement is failure-atomic. Runtime
  `PATCH /drives/{drive_id}` opens a replacement backing for an existing active
  drive before mutating stored configuration; in contained mode that backing
  may be an exact still-unused startup-batch drive grant. The update refreshes
  only the matching virtio-block MMIO handler and leaves the old backing and
  stored configuration in place if opening or handler lookup fails, although a
  successfully claimed replacement grant remains consumed. Limiter-only
  runtime updates do not reopen host backing paths; configured limiter buckets
  update only process-local active device state and stored drive configuration.
  Accepted opened objects are only regular files or exact block-special drive
  descriptors; final-component symlinks and every other object kind fail
  closed. Regular capacity is metadata length. Block capacity is checked
  `logical_block_size * block_count`, refreshed on replacement, and never
  inferred from `st_size`. PCI-mode post-start PUT prepares a non-root backing before entering the
  owner-thread command and commits public configuration only after the live
  endpoint publishes. Direct mode opens the proposed regular file on the API
  thread. Contained mode instead requires an exact still-unused initial
  `DriveBacking` grant with matching access, duplicates its already-opened file
  for the candidate, and retains the original grant in a rollback claim until
  publication commits. Invalid backing, admission, capacity, or publication
  failure therefore restores usable authority and never falls back to ambient
  path opening. Successful rollback leaves the worker live; incomplete
  publication cleanup closes admission and makes the worker terminal. A
  successful insertion consumes the grant once.

  File-backed `GET_ID` is derived from metadata on the exact opened descriptor,
  never from a second pathname lookup: decimal `st_dev`, `st_rdev`, and
  `st_ino`, truncated or NUL-padded to 20 bytes. A successful backing
  replacement commits this guest-visible identity with the backing, while a
  limiter-only update retains it. The value intentionally follows Firecracker,
  but it can disclose host filesystem identity components, may be truncated or
  ambiguous, and is not an authentication or authorization token. Native-v1
  load accepts only the exact current metadata-derived value or the exact
  legacy drive-ID-derived value from older bangbang artifacts.

  Bodyless PCI-mode DELETE first removes MMIO and ECAM visibility, closes work
  and message admission, and drains admitted operations while retaining exact
  generation-bound leases. Recoverable failure republishes the same usable
  endpoint. Only then does the irreversible phase release device, MSI-X, BAR,
  PCI function, dispatcher, backing, metrics, and configuration ownership;
  a failed preparation rollback or corruption after that boundary is terminal.
  Linux must rescan PCI after PUT and remove the function through guest sysfs
  before host DELETE. The API does not automate or attest that guest
  coordination. Default MMIO rejects runtime PUT and DELETE before using the
  proposed backing or mutating live/public state. Root insertion/removal
  remains rejected. Successful removal closes the active backing but does not
  recreate already-consumed contained grant authority.

  Block rate limiters remain process-local runtime state. Exhausted limiters
  leave the descriptor pending for a later dispatch opportunity instead of
  sleeping, busy-waiting, writing request status, publishing a used-ring entry,
  or mutating the backing file. Active HVF boot sessions schedule block retry
  wakeups with per-session state so one VM cannot wake or share limiter state
  with another VM. Direct or launcher-authorized contained pre-boot vhost-user
  sockets use a bounded redacted connector and the closed Firecracker v1.16
  frontend request set. The frontend
  explicitly zero-encodes native
  endian frames, attaches borrowed rights only until the first header byte is
  transferred, owns/CLOEXECs every received right before rejecting it, bounds
  one operation by one absolute deadline, and terminally closes a stream after
  synchronization loss. Directional nonblocking pipe types prevent swapping
  backend call writers with kick readers; Darwin descriptors suppress SIGPIPE,
  and errors/debug output omit paths, raw descriptors, addresses, and peer
  payload. Before VM construction, discovery validates mandatory virtio and
  CONFIG support and exact config length; failure drops every prepared stream
  without publishing a VM. Guest activation validates complete ring extents
  inside exported mappings before transferring memory/queue descriptors.
  Backend notification failure terminalizes that device, drops its pollable
  endpoint, increments redacted per-drive failure metrics, and leaves the API
  process responsive; it never falls back to local storage. The strict
  regular-file backend used for signed MMIO/PCI evidence is test-only and is
  not shipped. For a direct or contained all-PCI VM that already owns shared
  RAM, a new non-root runtime vhost device is discovered on the API thread only
  after the owner-side no-effect preflight, then materialized against the exact
  live memory and published atomically on the owner thread. Fresh contained
  directory adoption remains reversible until publication; committed
  directories remain session authority across DELETE. For any active MMIO or
  PCI device, ID-only PATCH fetches and validates all 60 config bytes
  before replacing guest-visible config and delivering one configuration
  interrupt; confirmed pre-delivery failure keeps the old generation, while
  delivery ambiguity is terminal. Caller-coordinated PCI DELETE drops the
  frontend, notifier pipes, cloned RAM descriptors, metrics lease,
  BAR/MMIO/MSI-X state, and PCI slot. Contained authorization remains a separate
  exact grant/broker layer and no live same-ID reconnect API is invented.
- `/pmem/{id}` stores Firecracker-shaped pmem backing paths during pre-boot
  configuration after rejecting empty paths, and reports them through
  `GET /vm/config`. In contained mode an exact pmem grant tag is claimed during
  successful `PUT` with access matching `read_only`, retained by pmem ID, and
  handed to startup without reopening the tag; same-ID replacement is
  failure-atomic. Ordinary mode opens each configured path with nonblocking
  access according to the configured read-only flag. Both paths verify a
  non-zero regular file, mmap it to one 2 MiB-rounded retained host range, and
  keep the file handles and mapping leases with the boot resources. The host
  pointer is host-page aligned; the assigned guest physical address and mapped
  length are 2 MiB aligned. Startup skips current guest RAM and records the
  deterministic range in the internal virtio-pmem config-space `start`/`size`
  fields. HVF registers that exact mapping after DRAM with no anonymous copy.
  Writable backings use a shared read/write host mapping and read/write,
  non-executable guest permissions. Because HVF requires a write-capable host
  mapping even for a read-only guest slot, read-only backings use a private
  read/write host mapping from the retained read-only descriptor while the
  guest mapping remains read-only and non-executable; accidental host writes
  are copy-on-write and cannot modify that backing. A real signed guest-write
  test retains the unchanged-backing boundary. Startup also
  attaches each prepared pmem device over the selected virtio-MMIO/FDT or modern PCI transport
  whose config-space exposes the assigned `start` and `size` values. It does
  not normalize stored host paths, and mapping, HVF registration, MMIO
  attachment, or flush errors
  identify the pmem ID and guest range without echoing `path_on_host`.
  Per-device bandwidth and operation rate limiters are validated before
  startup, reported through `GET /vm/config`, and charged once per non-empty
  coalesced flush event before the queue cursor advances. Throttled work stays
  pending behind a dedicated per-session wakeup, and runtime
  `PATCH /pmem/{id}` replaces the exact active device limiter before stored
  configuration is committed. Flush selection is lazy and scoped to the
  notified device: empty or malformed-only events perform no backing sync, one
  valid request caches only that device's event result, and peer pmem mappings
  are not traversed. Diagnostics expose only counters and device IDs, not
  bucket values or backing paths. An operator can deliberately configure the
  same external file for multiple devices or processes; that alias is outside
  bangbang's isolation guarantee and requires operator-owned access and
  coordination. A same-user operator can also truncate a direct writable
  backing after validation and cause later mapped access to fault; Darwin has
  no file-seal equivalent, so this is an explicit availability boundary rather
  than a racy size-check promise. Queue flush and graceful removal call
  `msync(mapping_start, file_len, MS_SYNC)` over only the persistent prefix;
  the private alignment tail is volatile and no stronger power-loss guarantee
  is claimed. Public PCI runtime PUT opens or reserves one exact direct or
  contained backing before owner publication, allocates a non-overlapping
  aligned guest range, and registers a cloned lease on that mapping. DELETE
  first removes endpoint reachability, then synchronizes and unregisters that
  exact mapping and releases the backing, range, and metrics generation;
  failed unmap retains every lease HVF may still reference. Pre-commit failure
  restores reachability or mapping, while incomplete restoration is terminal.
  Consumed contained authority is never recreated. Pre-boot configuration
  permits one root across ordinary block and pmem devices; a pmem root boots as
  its stable `/dev/pmem<i>` index with `ro` or `rw`, while runtime root mutation
  remains rejected. Pmem remains outside ordinary RAM dirty epochs. Its exact
  capture-ready live state is retained in memory, while native-v1 serialization
  and restore remain deferred.
- `/entropy` accepts Firecracker-shaped bandwidth and ops rate-limiter buckets.
  The limiter is process-local runtime state, is applied before host entropy is
  read or guest memory is written, and must not sleep or busy-wait while budget
  is exhausted. Throttled descriptors remain pending for a later dispatch
  opportunity instead of completing with zero bytes or exposing host entropy
  source details. Runtime dispatch may report a process-local retry delay for
  the pending descriptor. Active HVF boot sessions use that delay to schedule a
  per-session entropy retry wakeup without sharing limiter state with another
  VM. Metrics may count throttling and limiter retry events, but must not
  include random bytes, descriptor contents, host RNG errors, or host paths.
- `/snapshot/create` and `/snapshot/load` retain complete normalized
  Firecracker-shaped inputs in typed API/runtime values before capability
  preflight or execution. Manual `Debug` implementations redact state/memory paths,
  interface IDs, host device names, and vsock paths even through enclosing
  request/action enums; action names, errors, logs, and metrics remain
  value-free. Unsupported request dimensions fail before storage work. Before
  applying native-v1 profile exclusions, an admitted paused create traverses
  every configured startup/runtime block and pmem owner across MMIO/PCI. It
  rejects live vhost-user first; otherwise it closes and drains every Async
  generation, publishes all completions, delivers their MMIO SPI or PCI MSI-X
  interrupts, captures the detached state, and reopens every generation before
  returning a redacted in-memory handoff. The later network traversal
  generation-binds configuration, packet I/O, metrics, optional MMDS identity,
  and deterministic MMIO/PCI owners while callback publication is quiesced.
  It validates exact queue/feature/limiter state and one reconstructible TX
  retry, but excludes raw handles, callbacks, cached peer RX, active TCP/ARP,
  retained MMDS output, token secrets, borrows, and absolute clock values. MMIO
  publication releases the
  dispatcher before GIC delivery so the normal dispatcher/GIC lock order is
  preserved. This occurs before contained output claims, staging, or
  native-state capture. Unsupported profiles then fail before artifact I/O. An
  accepted create opens only preflighted namespaces, temporarily
  closes ordinary boot-worker command admission, and acknowledges process-local
  block/PMEM/network/entropy retry quiescence through complete capture,
  publication, and the post-publication hook. API/MMDS/controller mutation and
  periodic callbacks cannot re-enter the synchronously borrowed process during
  that interval. External vmnet/vsock peers and their host/kernel buffers are
  neither frozen nor persisted; the admitted profile excludes those devices.
  Load freshness uses
  successful configuration history plus current non-logger/metrics state, so
  explicit defaults and residual MMDS presence fail closed without treating a
  side-effect-free failed request as configuration. Snapshot execution treats
  paths and restored guest/vCPU/device state as
  untrusted, preserve redaction, and prevent one process from cleaning up or
  overwriting another process's resources. In contained mode, state
  preinspection is non-consuming and the eventual state/memory/persisted-root
  claim is atomic; create uses only exact retained output anchors and validated
  children. Direct mode retains ordinary path adapters. The current boundary is
  documented in [Snapshot Feasibility](snapshot-feasibility.md).
- Native snapshot inspection treats the entire state file as untrusted binary
  input. The process opens it nonblocking, accepts only a regular file, caps the
  complete read at 16 MiB plus the 40-byte envelope overhead, and rechecks the
  cap while reading. The pure decoder uses checked length conversion and
  arithmetic, requires exact consumption, validates CRC before semantic
  compatibility, and publishes no payload or metadata until all checks pass.
  Command-path and payload debug output is redacted, and read errors retain only
  `ErrorKind`, not the host path. Contained description applies the same reader
  to an exact granted descriptor and never falls back from a malformed or
  mismatched tag to ambient pathname access. CRC-64/Jones detects accidental corruption;
  it is not authentication, and a party that can rewrite the file can recompute
  it. Future payload schemas must therefore stay memory-safe and fail closed
  even for checksum-valid attacker-controlled bytes.
- Native-v2 state is currently a library-only, incomplete VM-state format. Its
  first pass treats all bytes as hostile, caps the complete file at 16 MiB,
  caps feature and component counts before table traversal, uses checked
  conversions and arithmetic, requires canonical packed ranges and exact EOF,
  and validates the whole-state CRC before publishing a borrowed view. The pass
  performs no count-proportional allocation. The immutable `2.0.0` catalogs are
  empty; current `2.1.0` adds only semantic memory kind 1, instance 0. Unknown
  mandatory behavior fails closed, while explicitly nonsemantic extensions can
  survive complete structural validation. Errors and `Debug` omit identifiers,
  payloads, format magic, guest contents, and raw state. The Firecracker prefix
  classifier proves only an incompatible family and never deserializes or
  translates upstream bitcode. No public create, describe, load, or VM action
  consumes v2 in this slice.
- Native-v2 lazy memory validates the state binding before opening or adopting
  a source, then requires a read-only close-on-exec regular descriptor, exact
  canonical length, stable descriptor identity/facts, an exact repeated header,
  and zero fixed 64-KiB padding. Direct opens are anchored at the parent and
  reject a final symlink; contained callers supply the exact `File`. Validation
  reads only that fixed metadata area. All extents are checked for topology,
  host-page compatibility, conversion, offset, and file bounds before the first
  mapping. Every extent then retains the same descriptor through a writable
  `MAP_PRIVATE`/no-reserve mapping; no path is reopened, no guest bytes are
  eagerly copied, and all partial owners drop normally on failure. Descriptor
  facts are rechecked after metadata and after mapping to reject substitution or
  mutation during setup. The arbitrary external inode cannot be sealed on
  macOS, so it must remain immutable for the entire mapping lifetime.
- V2 guest writes and discard never write or punch the retained source. COW
  writes enter the ordinary dirty epoch; discard replaces an aligned private
  subrange with anonymous zero pages and marks it dirty. The binding and state
  CRCs cover metadata only: guest bytes intentionally have no digest, trailer,
  authentication, or encryption. An actor able to rewrite the artifacts can
  recompute CRCs or alter lazy guest pages after validation. Deployment must
  authenticate the complete state/memory pair and apply encryption when guest
  memory confidentiality is required. Signed demand-fault/COW evidence is a
  lifecycle proof, not an artifact trust mechanism.
- Native guest-memory bindings and images are also untrusted. The binding caps
  metadata at 4,096 exact GPA ranges / 98,376 encoded bytes and memory data at
  the current 1,022-GiB arm64 policy, checks every conversion, alignment,
  overlap, sum, absolute offset, and file length, and allocates no guest memory
  until a seekable input reports the state-bound exact length and header
  identity. Streaming uses one fallible 1 MiB buffer; partially initialized
  anonymous memory is never returned. Handle errors retain only a fixed stage
  and `ErrorKind`; image IDs, checksums, guest bytes, and host paths stay out of
  diagnostics. The random image ID and CRC detect mismatched or accidentally
  corrupt pairs but do not authenticate an actor able to rewrite both files.
  The handle-level codec itself opens no path. The internal artifact layer can
  compose it with either memory-only or composite commit kind. The private
  process create seam composes complete capture with final publication, and the
  admitted public snapshot paths invoke the production create/load transactions.
  Contained load supplies the exact state/memory handles after one atomic claim;
  state decode is reused rather than rereading a selector.
- Internal native snapshot publication treats both final paths and all existing
  directory entries as untrusted. It opens each parent once, anchors later
  operations to that descriptor, rejects exact aliases, preflights final names
  without following them, and creates unreported 128-bit-random staging names
  with exclusive `0600` regular files. Both private contents and file barriers
  complete before memory is published; memory uses `RENAME_EXCL` and a directory
  barrier before state is published the same way as the only commit marker.
  Existing files, directories, FIFOs, sockets, and symlinks are never opened for
  write, truncated, or replaced. A failure after memory publication leaves a
  typed orphan rather than unlinking a final name. A state-directory sync error
  after state rename is a committed, durability-uncertain result and is not safe
  to retry under unchanged names. Granted destinations enter through already
  retained directory anchors and validated children rather than parent-path
  reopening; each staging inode is recorded for strict launcher recovery.
- The generic content producer receives only a non-cloneable, pathless staging
  writer. Writer destruction closes its descriptor before publishing a close
  proof; retention or `mem::forget` fails without waiting and before any file
  barrier or rename. Producer failures retain a typed source only through a
  trusted accessor while formatted diagnostics redact it. Before sync, a
  fixed-size verifier matches the actual memory header identity, data/file
  lengths, EOF, and stored checksum trailer to the returned codec binding. This
  is mismatch detection for a trusted producer, not full validation: only the
  loader recomputes CRC and validates GPA ranges.
- Destination directories are trusted security boundaries. Darwin has no
  public rename or unlink conditioned on the identity of an already-open file,
  so the immediate staging inode check is best-effort and has a residual race.
  Random names and `0600` protect against accidental collision and actors
  lacking directory authority; they do not protect against an uncooperative
  writer with mutation rights in that directory. Such a writer can also replace
  final names after publication. `RENAME_EXCL` authoritatively prevents this
  operation from overwriting a target present at the rename instant. CRCs and
  image IDs detect accidental corruption or mismatched pairs but do not
  authenticate either artifact. Diagnostics retain only typed stages, byte
  counts, and `ErrorKind`; paths, staging names, IDs, checksums, state bytes, and
  guest bytes remain redacted. App Sandbox security scope is still associated
  with the authorized directory pathname, so moving that directory after scope
  activation may deny later descriptor-relative writes. Worker-first recovery
  narrows crash residue after a record is durable, but the create-before-record
  interval and simultaneous uncatchable launcher/worker death remain.
- Detached vCPU general-register values, raw SP_EL0, SP_EL1, ELR_EL1, and
  SPSR_EL1 values, raw EL1 AFSR0/AFSR1/ESR/FAR/PAR/VBAR values, raw
  ACTLR_EL1/CPACR_EL1 execution controls, raw CSSELR_EL1 cache selection, raw
  hardware-breakpoint and hardware-watchpoint value/control pairs, raw
  MDCCINT_EL1/MDSCR_EL1 debug controls, raw Hypervisor.framework debug-trap
  policy, raw pointer-authentication keys, raw TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1
  values, raw SME SMCR_EL1/SMPRI_EL1/TPIDR2_EL0 values, raw
  SCXTNUM_EL0/SCXTNUM_EL1 software context numbers, raw
  Q0-Q31/FPCR/FPSR values, raw
  physical-timer CNTKCTL/control/CVAL/TVAL values, raw virtual-timer
  mask/offset/control/CVAL values, raw EL1
  SCTLR/TTBR0/TTBR1/TCR/MAIR/AMAIR/CONTEXTIDR values, CPU IRQ/FIQ pending
  levels, opaque GIC device-state bytes, and raw EL1 GIC ICC CPU-interface
  values are sensitive guest/VMM execution state.
  The general-register owner-thread restore primitive accepts only the detached
  typed X0-X30/PC/CPSR value, but that value remains untrusted guest execution
  state rather than validated snapshot input. Its 33 Hypervisor.framework
  writes are ordered and nontransactional. A typed error reports only the
  failed register identifier, completed-write count, and backend source—not
  register contents. After failure, callers must retry the complete retained
  value or discard the vCPU before execution; running a partially updated vCPU
  is outside the supported boundary. Public snapshot load uses the validated
  aggregate restore command rather than invoking this standalone primitive.
  The paired core system-register restore has the same trust and partial-write
  boundary for raw `SP_EL0`, `SP_EL1`, `ELR_EL1`, and `SPSR_EL1`. It accepts
  only the complete typed capture today, writes the four fields in capture
  order, and reports the failed `HvfSystemRegister`, completed prefix, and
  backend source without values. Stack and exception-return fields must not be
  treated as validated addresses or legal return state. After failure, callers
  must retry the complete retained value or discard the vCPU before execution.
  The paired EL1 exception-register restore extends that boundary to raw
  `AFSR0_EL1`, `AFSR1_EL1`, `ESR_EL1`, `FAR_EL1`, `PAR_EL1`, and `VBAR_EL1`.
  It writes only a complete typed capture in capture order and reports the
  exact failed register, completed prefix, and backend source without values.
  AFSR contents can be implementation-defined, report fields are not a
  validated coherent exception, and address/vector fields are not validated
  against guest memory. After failure, retry the complete retained value or
  discard the vCPU before execution.
  The paired execution-control restore applies the same boundary to raw
  `ACTLR_EL1` and `CPACR_EL1`. It accepts only the complete typed capture,
  writes ACTLR then CPACR, and reports the exact failed register, completed
  prefix, and backend source without values. EnTSO changes the guest memory
  model, while CPACR can expose optional feature controls; neither is validated
  against a destination or accompanied by a guest ISB transition. After
  failure, retry the complete retained value or discard the vCPU before
  execution.
  The paired thread-context restore extends the same boundary to raw
  `TPIDR_EL0`, `TPIDRRO_EL0`, and `TPIDR_EL1`. It accepts only the complete
  typed capture, writes the three fields in capture order, and reports the
  exact failed register, completed prefix, and backend source without values.
  TPIDR fields can contain guest TLS or kernel pointers and are not validated
  against destination memory or coordinated with separately captured TPIDR2,
  SCXTNUM, or CONTEXTIDR state. After failure, retry the complete retained
  value or discard the vCPU before execution.
  The paired EL1 translation-register restore extends the boundary to raw
  `SCTLR_EL1`, `TTBR0_EL1`, `TTBR1_EL1`, `TCR_EL1`, `MAIR_EL1`, `AMAIR_EL1`,
  and `CONTEXTIDR_EL1`. It accepts only the complete typed capture, writes the
  seven fields in capture order, and reports the exact failed register,
  completed prefix, and backend source without values. TTBR fields and
  CONTEXTIDR can expose sensitive guest addresses and identities, and every raw
  control remains untrusted. The primitive supplies no translation-table
  memory, feature or destination validation, barriers, TLB/cache maintenance,
  safe MMU transition sequence, rollback, or wider restore ordering. It must
  preserve actual implementation-defined AMAIR readback; after failure, retry
  the complete retained value or discard the vCPU before execution.
  The paired system-context restore extends the same boundary to raw
  `SCXTNUM_EL0` and `SCXTNUM_EL1`. It accepts only the complete redacted typed
  capture, writes EL0 then EL1, and reports the exact failed register,
  completed prefix, and backend source without either software context number.
  Those values can identify guest execution contexts and are not interpreted,
  destination-validated, or coordinated with separately captured TPIDR and
  `CONTEXTIDR_EL1` state. After failure, retry the complete retained value or
  discard the vCPU before execution.
  The paired cache-selection restore extends the boundary to raw
  `CSSELR_EL1`. It accepts only the complete typed capture, performs the one
  owner-thread write, and reports the exact register, zero completed writes,
  and backend source without the selector value. CSSELR is not topology, and
  this primitive neither validates an encoding or destination cache manifest
  nor supplies ISB/dependent CCSIDR visibility or cache maintenance. After
  failure, retry the complete retained value or discard the vCPU before
  execution.
  The paired debug-control restore extends the boundary to raw `MDCCINT_EL1`
  and `MDSCR_EL1`. It accepts only the complete typed capture, writes MDCCINT
  then MDSCR, and reports the exact failed register, completed prefix, and
  backend source without either raw value. A retained value can request monitor
  debug, stepping, or DCC behavior; after partial failure, retry the complete
  value or discard the vCPU before execution. The primitive provides no
  feature/writable-bit or destination validation, comparator or host trap-policy
  coordination, protected persistence, rollback, schema, or safe complete debug
  restore.
  The paired debug-trap restore extends the boundary to Hypervisor.framework's
  host debug-exception and debug-register-access policies. It accepts only the
  complete two-Boolean typed capture, writes exception policy then register-
  access policy, and reports the exact failed operation, completed prefix, and
  backend source without either value. A partial apply can change whether guest
  debug behavior exits to the VMM; after failure, retry the complete retained
  value or discard the vCPU before execution. The primitive provides no wider
  ordering with guest MDCCINT/MDSCR or comparator state, feature/destination
  policy, protected persistence, rollback, schema, or public snapshot load.
  The paired pending-interrupt restore applies the same boundary to CPU-level
  IRQ and FIQ injection state. It accepts only the complete typed capture,
  writes IRQ then FIQ, and reports the exact failed interrupt type, completed
  prefix, and backend source without either Boolean value. The fields affect
  guest control flow but do not include GIC/device state, routing, delivery, or
  EOI policy. HVF clears both levels after a vCPU run returns, so the primitive
  neither persists delivery state nor defines automatic per-run reassertion.
  After failure, retry the complete retained value or discard the vCPU before
  execution.
  The paired pointer-authentication restore extends the same boundary to APIA,
  APIB, APDA, APDB, and APGA. It accepts only the complete redacted typed
  capture, writes each low then high half in capture order, and reports the
  exact failed register, completed prefix, and backend source without key
  material. The borrowed API clones the non-`Copy` state once into command
  ownership; neither that restriction nor redacted `Debug` provides memory
  zeroization. This primitive supplies no algorithm/feature or destination
  validation, protected persistence, safe keys-before-SCTLR-enable ordering,
  rollback, or schema. After failure, retry the complete retained value or
  discard the vCPU before execution.
  The paired baseline SIMD/FP restore extends the boundary to Q0-Q31, FPCR, and
  FPSR. It accepts only the complete typed capture, writes all 34 fields in
  capture order, and reports the exact SIMD/FP or scalar register space,
  completed prefix, and backend source without values. Q bytes can contain guest
  application or cryptographic working data. The target-gated C shim receives
  only a transient 16-byte pointer, copies it into the SDK vector, and retains
  nothing. In streaming mode Q writes alias the low 128 bits of Z registers;
  this primitive provides no wider Z/P/ZA/ZT0 ordering, feature or destination
  validation, FPCR/FPSR writable-bit policy, protected persistence,
  zeroization, rollback, or schema. After failure, retry the complete retained
  value or discard the vCPU before execution. Public snapshot load reaches the
  validated aggregate restore command only after exact native-v1 compatibility
  checks; it does not call these standalone primitives independently.
  TTBR fields expose guest physical table addresses, while CONTEXTIDR can
  expose guest process or kernel context identifiers.
  FAR and PAR can expose guest fault or translation-result addresses, VBAR can
  expose a guest kernel vector address, and syndrome/fault fields can reveal
  guest execution details.
  CSSELR records the guest's current cache-size query selector but does not
  contain cache topology. The internal capture-order apply treats the selector
  as raw untrusted state and never queries CCSIDR, but it is not a validated
  snapshot restore: a higher layer must still define selector validation,
  ISB/dependent-read synchronization, and cache-maintenance policy. The public
  standalone default-configuration feature and geometry queries remain
  independent read-only diagnostics; they do not form one atomic manifest and
  can fingerprint the exposed virtual CPU model.
  Ordinary arm64 startup instead owns a distinct same-configuration source
  containing MMFR2, CTR/CLIDR/DCZID, and both CCSIDR arrays. It interprets only
  active levels and accepts the result only when exactly one public macOS
  performance-level description independently confirms sizes and supplies
  valid nested sharing factors. Missing, malformed, mismatched, or ambiguous
  evidence fails before VM construction. It uses neither scheduler affinity,
  private Apple interfaces, nor model-specific tables. Errors and `Debug`
  output expose no raw HVF values, sysctl values, selector names, or underlying
  host diagnostics. The normalized geometry and sharing are deliberately guest
  observable through the FDT, but remain a same-host presentation rather than
  a physical-host identity or cross-host portability promise.
  Native-v1 capture reuses the retained startup manifest after an MMFR2
  cross-check instead of querying mutable external state. Restore reconstructs
  only that already-validated compatibility source; it does not fabricate a
  retained FDT hierarchy or change the snapshot schema. These internal needs do
  not authorize logging the raw source or treating it as a safe live CSSELR
  restore policy.
  Firecracker-shaped custom CPU-template values have a narrower untrusted
  control-plane boundary. The HTTP/config-file parser retains bounded ordered
  KVM capability, KVM vCPU-init feature, and 32/64/128-bit arm one-register
  values, but every aggregate has manual value-redacted `Debug`. Executable
  state is limited to eleven reviewed U64 identification registers,
  ACTLR.EnTSO, reviewed U64 X/core fields, U128 Q0-Q31, and U32 FPCR/FPSR.
  ZFR0/SMFR0 require a public macOS 15.2 pre-VM check, and ACTLR filters may
  select only bit 1. X1-X3 are boot-reserved; AArch32 banked state, topology
  identity, boot/dependency controls, translation/exception/thread/context,
  cache, pointer-authentication, debug/trap, timer, GIC/ICC, mutable SME, and
  disabled EL2 state each fail with a distinct value-free policy category.
  KVM-only classes, aliases, invalid fields, and unnamed encodings also fail
  closed. There is no generic raw system-register constructor. Q values cross
  the HVF boundary only through explicit little-endian conversion, and nonzero
  FP transport bits above U32 fail before mutation. All requested typed
  baselines are read on every vCPU before the first write; targets are computed
  once, then every owner writes and immediately rereads each one. Boot setup
  subsequently overwrites the admitted X0/PC/PSTATE targets. Any mapping,
  availability, read, write, or mismatch failure destroys the complete
  unpublished VM because live register mutation is not rollback-safe.
  The allowlist and exact readback do not make an arbitrary mask safe: a custom
  view can still crash a guest or create an incoherent/insecure instruction
  contract. Raw capability numbers, indexes, register identities, masks,
  baselines, targets, and readbacks must not enter product `Debug`, `Display`,
  HTTP faults, logs, metrics, or serial output. Custom contents remain omitted
  from GET and excluded from native-v1 snapshots. KVM capability/feature
  namespaces have fixed platform faults. Pending static `V1N1` fails before
  executor/backend construction because live writes cannot establish its
  documented Neoverse V1 source model on Apple Silicon. The complete boundary
  is checked in `compat/firecracker/v1.16.0/cpu-template-contract.md`.
  Breakpoint value registers can expose guest virtual addresses, Context IDs,
  or VMIDs. Watchpoint value registers expose guest data virtual addresses, and
  their controls can encode access type, byte selection, linking, and enabled
  debug behavior. Each capture reads only its DFR0-reported implemented prefix
  and does not log, persist, write, enable, or change trap policy. Future export
  must protect confidentiality, and future restore must validate each
  destination count, features, control bits, ordering, and host trap policy.
  MDCCINT and MDSCR can reveal security-sensitive guest debugging controls and
  status. The bounded pair apply can reapply a complete capture, but raw writes
  can activate monitor debug, software stepping, or debug communications
  behavior and are not a validated safe restore model. Breakpoint/watchpoint
  comparators and host trap policy use separate values and operations.
  Hypervisor.framework's separate debug-exception and debug-register-access
  booleans reveal whether guest debug exceptions and documented debug-register
  accesses exit to the host. The bounded pair apply can reapply a complete
  capture but remains separate from guest debug-register contents. Future
  composite restore must treat both host trap settings and guest debug controls
  as untrusted, validate features and writable/status bits, and coordinate
  policy and ordering before executing the guest.
  Pointer-authentication keys are cryptographic secrets. Their detached value
  uses a custom `Debug` implementation that exposes only a redacted marker.
  Current capture-order apply never formats the value, but does not zero the
  caller or queued copies, validate a destination, or define SCTLR enable
  ordering. Future persistence must protect key confidentiality and integrity.
  The opaque GIC byte value uses a custom `Debug` implementation that reports
  only its length rather than formatting its contents. Its borrowed pre-run
  apply clones the complete value into command ownership and passes only the
  exact pointer and length to Hypervisor.framework; neither copy is zeroized.
  Empty input is rejected without an FFI call, and backend failures contain no
  bytes. Because Apple documents neither transactional rollback nor a distinct
  compatibility status, a failed apply must not be relabelled as corruption or
  followed by guest execution: discard the destination or use a future explicit
  recovery policy. The isolated command does not protect the later ICC, timer,
  pending-interrupt, vCPU, and device restore sequence from an intervening run.
  The separate EL1 GIC ICC restore accepts the complete untrusted ten-register
  capture, writes its nine architecturally mutable fields, and validates the
  derived read-only RPR at the original capture position. Getter and setter
  capabilities are both loaded before mutation. A typed error exposes only the
  failed register, write-or-validation operation, completed-write count, and
  backend source—not the captured or observed values. The writes are
  nontransactional, so any failure requires complete retry or vCPU discard
  before execution. The command enforces the sticky never-run gate, but trusts
  the caller to apply a compatible opaque GIC blob first and releases admission
  on return; it is not destination validation or a lease across the wider
  restore sequence. Raw ICC values remain sensitive and must not be logged.
  The separate MIDR, MPIDR, PFR, DFR, ISAR, and MMFR baseline plus optional
  ZFR0/SMFR0 capture are read-only virtual-CPU/HVF compatibility metadata rather
  than mutable execution state, but they can fingerprint the exposed processor
  feature model. They must not be logged or persisted without a defined need
  and must not be mistaken for physical-host identity or a trusted destination
  compatibility decision. The optional IDs do not contain or protect streaming
  SVE/SME execution state.
  The configuration-wide maximum SME streaming vector length is a read-only
  HVF host capability, not guest data or mutable execution state. It can still
  contribute to fingerprinting the exposed host capability and must not be
  logged or persisted without a defined need. The scalar is only a
  buffer-sizing bound for conditional Z, P, and ZA captures: it neither proves
  that a particular vCPU exposes SME nor
  defines its effective `SMCR_EL1.LEN`, and it must not be trusted as a feature
  or destination-compatibility decision.
  The separately captured SME `PSTATE.SM` and `PSTATE.ZA` flags are mutable
  guest execution-mode state. They reveal whether streaming mode and ZA storage
  are active, but contain none of the Z/P/ZA/ZT0 data. The getter is resolved at
  runtime for the macOS 15.2 boundary, never calls the setter, and preserves
  raw `HV_UNSUPPORTED` on SME-incapable hardware. The flags must not be logged,
  persisted, trusted, or restored without feature validation and ordering with
  Q/Z/P/FPSR and conditional ZA/ZT0 contents.
  The conditionally captured streaming Z0-Z31 bytes are sensitive guest
  execution and potentially cryptographic state. Capture preflights
  `PSTATE.SM`, uses the configuration-wide maximum SVL only as an allocation
  width, and publishes no partial buffer after a getter failure. The detached
  value redacts all bytes from `Debug`; callers with bounded raw access remain
  trusted internal code and must not log the contents. Persistence requires
  confidentiality and integrity protection, effective-SVL and feature policy,
  coordinated P/ZA/ZT0 and FPSR handling, destination validation, schema,
  zeroization, and safe transition/restore ordering, none of which exists yet.
  The conditionally captured streaming P0-P15 predicate bytes are likewise
  sensitive guest execution state. Capture preflights `PSTATE.SM`, requires a
  non-zero maximum SVL divisible by eight, uses one eighth of that maximum as
  each predicate width, and publishes no partial buffer after a getter failure.
  The detached value redacts all bytes from `Debug`; bounded raw access remains
  restricted to trusted internal composition. Persistence requires the same
  confidentiality, integrity, zeroization, feature/destination, effective-SVL,
  schema, and transition/restore policies coordinated with Z/FPSR and
  conditional ZA/ZT0 contents, none of which exists yet.
  The conditionally captured ZA matrix is sensitive guest execution and
  potentially cryptographic state. Capture preflights `PSTATE.ZA` without
  requiring `PSTATE.SM`, checked-squares the non-zero maximum SVL, fallibly
  allocates that exact byte count, and publishes no value after a getter
  failure. The detached value redacts bytes and dimensions from `Debug`; raw
  access remains restricted to trusted internal composition. Persistence
  requires confidentiality, integrity, zeroization, layout and effective-SVL
  policy, feature/destination validation, schema, and transition/restore
  ordering coordinated with Z/P/FPSR and conditional ZT0; none of those
  persistence or restore policies exists.
  The conditionally captured SME2 ZT0 register is sensitive guest execution and
  potentially cryptographic state. Capture preflights `PSTATE.ZA` without
  requiring `PSTATE.SM` or querying maximum SVL, then writes exactly 64 bytes
  through a private 16-byte-aligned SDK value and publishes only after success.
  The detached value redacts every byte from `Debug`; fixed-size raw access
  remains restricted to trusted internal composition. Persistence requires
  confidentiality, integrity, zeroization, SME2 feature and destination policy,
  lane interpretation, schema, and transition/restore ordering coordinated with
  Z/P/ZA/FPSR, none of which exists yet.
  The separately captured raw `SMCR_EL1`, `SMPRI_EL1`, and `TPIDR2_EL0` values
  are mutable SME and thread-context state; `TPIDR2_EL0` can contain sensitive
  guest pointers. Their detached value redacts every register from `Debug`, and
  capture performs no writes, but raw accessors remain restricted to trusted
  internal composition. The values must not be logged, persisted, trusted, or
  restored without feature and writable-bit validation, maximum-SVL policy,
  and ordering with PSTATE plus conditional Z/P/ZA/ZT0 contents.
  The separate raw `SCXTNUM_EL0` and `SCXTNUM_EL1` values can identify guest
  software execution contexts. Their detached value redacts both registers from
  `Debug`, and raw accessors remain restricted to trusted internal composition.
  The internal capture-order apply never formats either value, but it is not a
  snapshot restore policy: the values must not be logged, persisted, or trusted
  without feature, interpretation, destination, and ordering policy coordinated
  with TPIDR and `CONTEXTIDR_EL1` state.
  Current internal capture and raw-apply commands keep these values in process memory and do
  not write them to logs, metrics, error strings, or persistence. The raw
  virtual-timer offset is tied to HVF's host-time relation, the physical-timer
  CVAL is an absolute comparator against a continuing count, and the
  architecturally signed 32-bit relative TVAL is returned as raw `u64` and
  changes as that count advances. CVAL and TVAL are read sequentially rather
  than simultaneously, and control ISTATUS bits are time-sensitive observations
  rather than writable configuration. These raw values remain observation-only
  and are not the native restore form. The separate normalized timer policy
  removes the source counter epoch, strips ISTATUS, ignores TVAL, retains only
  ENABLE/IMASK, and rejects unknown control bits. Its state and failure `Debug`
  output redact values, and restore preflights all capabilities before an
  ordered nontransactional write sequence. A failed apply may have changed the
  never-run destination; callers must retry the complete value with a fresh
  sample or discard it. Individual command admission is not a cross-step
  restore lease. Public snapshot create/load use the aggregate native-v1
  capture/restore commands rather than these raw standalone operations.
  The retained virtual-timer wait foundation reads the same owner-local raw
  mask/offset/control/comparator state, converts only the comparator distance
  through the host Mach timebase, and publishes the configured timer PPI only
  after an enabled, guest-unmasked due recheck wins exact cancellation
  arbitration. It does not log timer values, execute a guest trampoline, infer
  a GIC route, or by itself expose PSCI `CPU_SUSPEND`. The guest-facing
  `CPU_SUSPEND32/64` layer binds only the exact deferred PSCI token to that
  wait, keeps affinity `ON`, ignores guest power-state/entry/context values,
  and writes `SUCCESS` only after the selected PPI is pending. Invalid timebase
  data, duration overflow, owner read failure, PPI failure, stale tokens, and
  dispatch-mode mismatches remain typed fail-closed errors; lifecycle
  cancellation retains the transaction and never fabricates guest timer wake.
  Stop, shutdown, and terminal drains deliberately abandon completion. No FDT
  idle states or SGI/SPI/direct IRQ/FIQ wake path is exposed.
  Native-v1 optional-state classification also fails closed when CPACR enables
  SVE/SME access, PSTATE.SM/ZA is active, or an implemented breakpoint or
  watchpoint is enabled. Category-only rejections expose no register value,
  address, feature value, or comparator index. An accepted inactive capture is
  not authorization to persist or restore other raw optional-state subsets.
  Prepared-session VMGenID replacement keeps the random candidate local, writes
  all 16 guest bytes before committing retained metadata, and formats neither
  old nor new generation bytes. GIC capability and line checks precede the
  write. The edge-rising SPI is asserted only afterward; signal failure means
  the new value is already committed and requires another complete replacement
  and notification or session discard, never a claimed rollback.
- `/vsock` is an **implemented supported live MMIO-or-PCI startup/Unix-socket subset**
  that stores the configured Unix socket path during repeatable pre-boot
  configuration and stably rejects post-start replacement. Startup can attach a
  guest-visible virtio-vsock device whose internal MMIO handler
  retains active RX, TX, and event queue metadata after `DRIVER_OK`, and the
  runtime has an internal Firecracker-shaped packet header model plus TX
  descriptor packet parser. Startup-level dispatch can drain RX, TX, and event
  queue acknowledgements, complete descriptor heads, and signal the allocated
  vsock queue interrupt line when completed descriptors require it.
  The runtime can
  also parse host `CONNECT <PORT>` requests, allocate Firecracker-shaped host
  local ports, retain host-initiated accepted streams in an internal table,
  expose one-shot guest-facing `VSOCK_OP_REQUEST` packet headers for retained
  host connections, dispatch those request headers into validated writable
  guest RX descriptors, accept one pending host connection per dispatch pass
  into an owned nonblocking stream, retain bounded accepted streams across
  partial handshakes and retained connection records, drop invalid
  accepted-stream handshakes without exposing host paths, retry RX delivery
  when pending host requests exist, and acknowledge guest `VSOCK_OP_RESPONSE`
  packets for delivered host requests by writing `OK <local_port>\n` to the
  retained host stream. Short or failed acknowledgement writes drop the retained
  connection and release its host local port. Unsupported or orphan
  host-destined guest TX packets can queue bounded guest-visible
  `VSOCK_OP_RST` headers. Supported guest `VSOCK_OP_REQUEST` packets attempt
  nonblocking connects to Firecracker-shaped `uds_path_<PORT>` sockets, retain
  successful streams in a bounded guest-initiated connection table, and deliver
  guest-visible `VSOCK_OP_RESPONSE` headers. Connect, duplicate, or retention
  failures deliver guest-visible `VSOCK_OP_RST` headers and retain no stream.
  Established host-initiated or guest-initiated connections can forward bounded
  guest `VSOCK_OP_RW` payload bytes to the retained host stream, keep a bounded
  four-packet per-connection guest-to-host retry queue for partial or
  would-block nonblocking writes, and retry pending bytes on later notification
  dispatch before accepting more guest `RW` data for the same connection.
  Zero-byte writes, queue overflow, or failed host writes for non-empty payloads
  drop the retained stream and queue a guest-visible `VSOCK_OP_RST` instead of
  buffering unbounded data. Established host-initiated and guest-initiated
  connections can also retain a bounded four-packet per-connection backlog of host
  `VSOCK_OP_RW` payloads and deliver one queued payload at a time
  into validated guest RX buffers. Guest `VSOCK_OP_RST` packets drop matching
  retained host-initiated or guest-initiated streams without queuing guest-visible
  RX output. Partial guest `VSOCK_OP_SHUTDOWN` packets record receive/send
  closure state, suppress later data movement in the closed direction, apply TX
  shutdown control before same-window RX host-payload delivery, and keep
  the retained stream until both directions are closed. Full guest
  `VSOCK_OP_SHUTDOWN` packets drop matching retained streams, release the host
  local port when applicable, and queue a guest-visible `VSOCK_OP_RST`. Valid
  guest `VSOCK_OP_CREDIT_UPDATE` packets for established retained streams are
  consumed without queuing a reset, and valid guest `VSOCK_OP_CREDIT_REQUEST`
  packets queue zero-payload guest-visible `VSOCK_OP_CREDIT_UPDATE` headers on
  the existing RX path. Each direction uses a dynamic 64-KiB credit window with
  wrapping counters; queued data reserves credit before publication, forwarded
  bytes release local credit, and exhausted peer credit requests an update
  without unbounded buffering. Host-stream clean EOF queues a guest-visible
  shutdown and a two-second terminal cleanup deadline after queued payloads
  drain; terminal read/write failures queue a reset. Incomplete host requests
  use the same two-second bounded-cleanup policy. Host- and guest-initiated
  tables share one 1023-connection active budget. Incomplete accepted host
  handshakes are bounded separately to 256, and host-local ports advance from a
  detached last-used cursor in Firecracker's round-robin range even when a
  completed handshake loses the final active slot.
  Startup also binds a nonblocking host Unix listener at `uds_path`,
  records the listener socket device and inode, and removes the path on normal
  shutdown only when it still refers to the socket created by this process. It
  never treats a configured path as globally unique, and transport failures and
  signed test diagnostics omit Unix paths and payload bytes. `EVENT_IDX` is
  implemented for RX/TX notification suppression; indirect descriptors are a
  supported bangbang extension. The event queue validates available, descriptor,
  payload, and used-ring memory before publishing the four-byte
  `TRANSPORT_RESET` value. Publication is committed before its mandatory MMIO
  queue intent or PCI queue-2 signal, arms a runtime-only acknowledgement gate,
  and leaves paths and packet contents out of diagnostics. Restored-origin
  signaling arms the same gate without mutating queue state; while gated, TX
  stays live and every eligible RX source remains buffered without consuming
  guest descriptors until the first valid event-queue kick. Signed Apple
  Silicon cases incrementally verify at least 1 MiB in each direction for both
  initiation paths, write-half-close/EOF, terminal cleanup, and two-stream
  isolation. PATCH, DELETE, runtime hotplug, broader CID routing, general
  performance/artifact parity, and broader event types remain outside this
  boundary. Internal MMIO/PCI capture values contain only redacted logical
  identity, feature/activation state, queue cursors, and the range-checked
  host-local cursor; live sockets, connections, accepts, payloads, deadlines,
  guest-memory borrows, and the ACK gate are excluded. Full state/ring/resource
  validation precedes consumption of the caller-supplied listener/connector,
  and a reconstructed destination starts with empty connection work and an armed
  snapshot-origin gate. The paused source producer now validates one exact
  MMIO-or-PCI owner, publishes reset, captures, and normalizes connection work
  under one lease before the unchanged optional-device rejection. Internal
  destination preparation resolves the captured selector and override before
  authority access. Direct publication is owner-only, stale-safe, atomic, and
  identity-cleaned; contained publication reserves the exact directory grant
  and session-bound broker endpoint, rolls them back before activation, and has
  no ambient-path fallback. A single-use process transaction keeps cleanup
  ownership through runtime adoption. Public native-v1 records still omit
  vsock encoding and placement and public load rejects overrides. The checked
  vsock ledger certifies the eight API/live records; the six aggregate
  encoding, invocation, restored-guest, clone/version, and portability outcomes
  remain #1490 work. The internal producer is not a public
  snapshot-containment claim.
- `/metrics` opens the output path during pre-boot configuration and keeps a
  per-process metrics sink. The `--metrics-path` startup CLI flag uses the same
  sink and host-path error redaction rules before the API socket is served.
  Configuration writes nothing. A retained session makes one best-effort
  initial attempt, Running and Paused sessions make 60-second best-effort
  attempts, explicit runtime `FlushMetrics` propagates a configured-sink write
  error to its caller, and normal convergence makes one best-effort final
  attempt without replacing the process result. Ordinary handle drop closes
  the sink.
- `/logger` opens `log_path` during pre-boot configuration when that field is
  present and keeps a per-process logger sink. Successfully parsed API requests
  can append method/path lines before dispatch, and successful `InstanceStart`
  and explicit `FlushMetrics` can append action-event lines when the configured
  level allows `Info` and the optional module prefix matches the event module.
  API request log lines intentionally omit request bodies, including MMDS
  payloads. These host records are unrestricted; guest boot-timer records use a
  ten-per-five-second limiter and one unrestricted recovery warning. Sink lock,
  write, or flush failure increments missed-delivery accounting but cannot
  change the API, action, startup, or guest-MMIO result. Logger startup CLI
  flags use the same sink and host-path error redaction rules before the API
  socket is served.
- `scripts/run-integration-tests.sh` creates temporary files for signed
  integration tests and removes them when the wrapper exits normally. Its
  generated guest initrd is cached under `.tmp/guest-artifacts` by default.

Metrics and logger outputs are opened with append/create semantics and
`O_NONBLOCK` to avoid blocking on FIFO-like paths during configuration. Block
backing code rejects unsupported file types such as directories, FIFOs, and Unix
sockets for block devices instead of treating every path-like object as a disk
image.

Error messages for host file open failures should not echo configured host
paths. Tests already cover this for several path surfaces, and new host path
features should add resource-specific redaction and file-type tests.

## Aggregate Storage Trust and Capacity Boundary

The #1471 direct and normal-production signed profiles compose Sync, portable
Async, vhost-user, pmem, and virtio-mem in one product-PCI VM. This does not
widen host authority. Direct mode retains the existing operator-owned files and
Unix sockets. Contained mode can consume only exact startup-batch file grants
and exact children below a connect-only vhost-user directory grant; replacing
the source pathname cannot redirect an already-opened backing. Failed
prepublication candidates restore reusable authority, successful publication
consumes it once, and final shutdown must release device, mapping, stream,
broker, session, and helper ownership without adding an entitlement.

Runtime pmem insertion is fail-closed before host-side effects. Root and
duplicate checks plus the shared 31-endpoint budget, pmem inventory, PCI
function, BAR aperture, MSI-X demand, dispatcher region, and metrics generation
are preflighted on the VM owner before a contained grant is claimed or a direct
file is opened and mapped. Mapping and endpoint publication precede public
configuration and grant-consumption commit. A preflight failure therefore
opens no path, maps no bytes, sends no broker request, consumes no grant, and
changes neither live nor public state.

An operator-selected vhost-user backend remains trusted for confidentiality,
integrity, and availability of the complete immutable memory table, including
the offline virtio-mem aperture. Darwin supplies no Linux memfd-seal equivalent,
and bangbang does not ship, jail, monitor, or define caching and rate-limiting
policy for that backend. Backend death terminalizes only its frontend/session
path and never falls back to ambient local storage.

Pmem is one direct file/private-tail mapping, not anonymous guest RAM. The
exact file prefix is persistent and flushable; alignment tail bytes are
private and volatile. Operators must treat DAX as a guest/filesystem choice and
profile page faults, page-cache/RSS accounting, huge-page realization,
eviction, same-backing physical-page sharing, side channels, and throughput on
the deployed macOS/HVF system. Linux Firecracker measurements are not portable
security or performance promises. The live aggregate certification adds no
snapshot authority: exactly the two checked Wave 6 pmem composites retain
optional-device serialization/restore, external-backing identity, artifact,
migration, portability, and signed-restore work.

## Offline Seccompiler Artifact Boundary

`seccompiler-bin` is an offline host utility, not part of the production
launcher/worker boundary. It does not expose a filter-install API, call Linux
seccomp, or change macOS process containment. The policy and paths are
untrusted inputs even though the resulting artifact is intended for a Linux
Firecracker consumer.

The input is opened once with close-on-exec, nonblocking, and final-component
no-follow flags. It must be a regular file and is read through a 1 MiB plus one
byte bound before UTF-8/schema validation. Errors retain only static categories;
they never embed a path, policy value, syscall name, target string, or raw OS
error.

For output, the tool opens and retains the selected parent directory with
no-follow semantics. It accepts only absent or regular final entries and never
opens a final target for truncation. Complete bytes are written to unique
owner-only same-directory staging files and synced before publication. A
private preflight proves the filesystem's no-replace and exchange rename flags;
unsupported filesystems fail before final mutation. Each rename and cleanup is
checked against recorded device/inode identities so an observed racing
replacement is preserved rather than deliberately deleted. This is not a claim
of atomic source identity against a hostile writer that controls the directory.

Observed failures before all outputs publish reverse committed renames in
reverse order when current identities prove that operation safe. A failed proof
reports rollback uncertainty and retains recovery objects. Once every final
name is visible, directory-sync failure is reported as committed with durability
uncertain; later old-inode cleanup or cleanup-sync failure is reported as
committed with cleanup uncertain. No claim is made that three split filenames
are one crash-atomic transaction. Normal success leaves only complete final
files and no private stages.

## Public-HVF GICv2m Capability Boundary

The macOS 15+ GICv2m path is an explicit startup opt-in selected by the public
`--enable-pci` flag. MSI-specific Hypervisor.framework symbols are probed before
API/no-API readiness and loaded for VM construction only after that opt-in; the
default backend configuration and guest FDT expose no MSI controller.
The configured frame and interrupt range are validated before publication; the
legacy and MSI allocators are disjoint, INTID 1019 is kept outside the pinned
Linux driver's usable domain, and the send address is derived internally from
the frame's `SETSPI` register. Startup atomically reserves the complete checked
VM vector pool and builds one exact address/data route per opaque,
generation-bound interrupt capability. Linux chooses MSI messages from the
vectors each driver actually requests, not from each device's maximum table
size, so every function receives an independently revocable registry over that
same exact pool. Ambiguous duplicate routes, foreign allocators, out-of-range
messages, and stale generations fail closed; this grants no host interrupt
primitive outside the configured VM GIC. Registry and signaler diagnostics
redact message values. Quiesce closes new admission and drains in-flight sends
before revocation; the final registry owner returns the complete pool under the
allocation lock, so a stale capability cannot target a later VM after reuse.

Hypervisor.framework does not make message delivery transactional. A returned
error cannot prove that the guest did not observe the interrupt, so a future
device owner must define its own retry and teardown policy. The modern
virtio-pci transport does not blindly retry an ambiguous host send; its
spec-defined masking path retains pending state for later unmask delivery.
Current signed coverage separately proves raw host-to-vCPU delivery,
pinned-Linux GICv2m discovery, focused identity/virtio-rng/data-device
conformance, and the signed product process booting every configured virtio
class with positive queue/configuration MSI-X and real I/O. Separate direct
and contained signed block, pmem, and all-MMDS network gates prove the retained
PCI manager's manual rescan/removal lifecycle and capacity reuse; pmem adds
exact direct-mapping/range reuse, while network adds packet-I/O/metrics teardown and
real MMDS exchange without vmnet authority. This evidence does not prove
interrupt remapping, external vmnet connectivity, or Firecracker's KVM ITS
behavior. MSI-bearing GIC metadata is rejected by the native-v1 snapshot
profile rather than silently omitted.

## PCI Ownership Boundary

The production process selects PCI only through exact `--enable-pci` syntax on
the supported macOS arm64/HVF path. Unsupported targets or missing GIC/MSI
symbols fail before readiness. Configuration-dependent endpoint, slot,
512-KiB BAR, and dispatcher-region demand plus exact fixed vector demand and
worst-case three-vector headroom for every remaining runtime slot is checked
before any endpoint is guest-visible. The path publishes one 1 MiB ECAM handler
only after the Firecracker-shaped configuration and 32/64-bit BAR windows
validate against guest RAM, GIC/GICv2m, and platform devices. The FDT binds the
host to the validated GICv2m phandle and advertises neither an ITS nor an
`msi-map`; every virtio legacy SPI/MMIO/FDT node is structurally suppressed.

MMIO publication and PCI slot/BAR allocation use opaque owner provenance plus
monotonic generations. Registration builds a complete candidate bus before
committing its handler and regions; failure leaves the live dispatcher
unchanged. Release checks the originating dispatcher or allocator, owner,
generation, and exact registered state before mutation, so a stale capability
cannot remove a later occupant after reuse. Ownership and address details are
redacted from capability `Debug` output.

The identity-only `[0042:0000]`, deterministic entropy source, and hidden data
selectors remain focused conformance tools. Product mode instead publishes
balloon, block, network, pmem, vsock, entropy, and virtio-mem in deterministic
Firecracker order and allows no mixed virtio transport. Platform devices remain
MMIO, while default process startup remains wholly virtio-MMIO and retains
`pci=off`.

All fixed and runtime data devices consume one fail-closed 31-endpoint budget;
removing block, pmem, or network returns that class's exact generation-bound
slot to the same shared pool. Runtime IDs are scoped by device type, so equal
block, pmem, and network IDs can coexist, while duplicate IDs within one type
and duplicate network MAC addresses are rejected before owner mutation. Mixed
commands submitted through concurrent handles execute exactly once on the
single VM owner thread, and the committed `/vm/config` projection follows the
same successful insertion/removal order. This aggregate contract composes the
class-specific signed guest gates; it does not replace their backing, mapping,
packet-I/O, teardown, or reuse evidence.

PCI configuration, BAR publication, MSI-X routing, and each virtio device share
one ordered endpoint lifecycle: reverse teardown first removes the exact
MMIO/function registrations, then closes and drains device work, revokes
message registries, and releases BAR and finally shared vector leases. Signed
teardown plus the
lower-level reuse gate prevent an old endpoint from signaling or unpublishing
its successor. The PCI MMDS proof uses only process-local runtime packet state
and opens no vmnet resource or extra host authority.

The hidden selectors and redacted diagnostic views grant no arbitrary host
resource authority. Public PCI supports the documented block, pmem, and network
runtime transactions, but guest rescan/removal remains an explicit operator
step and there is no automatic notification. Native-v1 create first captures
the complete live storage handoff, then rejects the immutable PCI profile before
native-state capture or artifact work. Load retains its pre-file/grant/
controller/VM-mutation rejection. Neither path persists or silently drops PCI
state.

## HVF Entitlements

Real Hypervisor.framework execution requires macOS support, Apple Silicon, and
the `com.apple.security.hypervisor` entitlement on binaries that enter HVF.

In the production bundle only `dev.bangbang.worker` receives that entitlement,
paired with `com.apple.security.app-sandbox`; `dev.bangbang` receives neither.
The two code objects are independently signed with one identity and Hardened
Runtime, then recursively verified before publication and again through the
launcher at execution. The default ad-hoc identity is local validation, not
Developer ID or notarization evidence.

The unsigned Rust test path runs only non-HVF unit tests. Real HVF integration
tests must run through `scripts/run-integration-tests.sh`. This wrapper builds
the selected HVF test binaries or executable e2e artifacts, creates a temporary
entitlement plist when signing is needed, ad-hoc signs copies, and runs signed
targets with one test thread. CI may use `--allow-unsupported` only to compile
and sign on runners that cannot execute HVF; local HVF verification should fail
when HVF is unavailable.

`hv_vm_protect` dirty-write tracking is an observation mechanism, not a guest
memory security boundary. It removes WRITE only from mapped guest RAM owned by
one active tracker and accepts an exit only when the lower-EL write has
CM/S1PTW clear, a physical address resolving to a tracker-owned currently
protected RAM page, and one of two signed-observed encodings: level-three
translation DFSC `0x07` for initial protection or level-three permission DFSC
`0x0f` after re-protection. These values are empirical Apple Silicon evidence,
not public Apple promises. Drift is fail-closed through ordinary MMIO/error
handling. MMIO, host-backed pmem payload mappings, readonly mappings, and IPAs
outside the current tracker set cannot become dirty through this path.

`GuestMemory` owns the authoritative atomic bitmap. Its bounded write API marks
boot-loader and every current VMM/virtio guest-RAM write after whole-range
validation; discard marks the aligned attempted interior before host advice so
partial zero/free failure cannot create a false negative. HVF restores one
protected page before marking that same bitmap. The protection bit is separate:
a host-dirty page remains protected and still takes one guest fault. One peer
may already have exited on the same page, so stale admission is bounded once per
member; a repeat by the same member/page is a typed no-progress failure.

The tracker is installed before normal boot population or after snapshot image
population. Live writable RAM additions are mapped without WRITE and enter the
current epoch wholly dirty; exact unmap removes both bitmap and protection
metadata under the fault lock. Host-backed pmem payload storage is not guest RAM
in the native-v1 image, while its guest-RAM queues and status writes still pass
through `GuestMemory`.

Only the paused snapshot-ready transaction may advance an epoch. After the
complete Full pair becomes visible, it re-protects coalesced restored-WRITE
ranges before clearing bits and incrementing the generation. A failed protect
reverses completed calls and preserves the old conservative epoch. If that
rollback is incomplete, the tracker is poisoned, command admission closes,
resume is impossible, and teardown owns cleanup even though the already-visible
artifact outcome remains accurately reported. Every pre-commit capture, I/O,
flush, rename, cancellation, or cleanup failure leaves the old epoch unchanged.
Errors and automatic `Debug` output expose typed stages and operation indexes,
not guest values, host pointers, memory contents, or public configuration
values. Exact page queries remain an authorized snapshot-internal/test surface.

An internal multi-vCPU topology does not relax HVF ownership rules. Each vCPU
is created, configured, queried, run, and destroyed only on its permanent owner
thread. The backend requires VM then GIC creation before topology allocation,
checks both the portable count ceiling and the host-reported maximum before the
first owner, reserves aggregate metadata first, and writes plus reads back every
ordered MPIDR before returning the complete topology. A partial construction is
never published: retained owners are shut down in reverse order, the original
failure remains primary, and indexed cleanup failures remain observable. MPIDR
values are topology metadata, but unrelated guest registers and memory are not
included in these diagnostics.

The legacy topology-wide `cancel` prerequisite still attempts each singular
runner and is suitable only for teardown and its signed pre-run cancellation
proof: asking HVF to exit an idle vCPU can affect that vCPU's next run. The
concurrent coordinator does not use that primitive. Under one aggregate state
lock it snapshots only online members with an active identified generation,
locks their runner state while raw ids are borrowed, and submits exactly one
slice-level `hv_vcpus_exit` request. Offline and idle members are excluded, raw
ids never leave the internal cancellation boundary, and diagnostics expose only
stable topology index/MPIDR and typed stages—not guest registers, memory, or host
pointers.

Successful batch exit records cancellation debt per member. If an ordinary
completion wins the race, a later matching `Canceled` generation is absorbed
instead of being reported as guest progress; members with active work or debt
cannot be moved offline. Wakeup, pause, stop, shutdown, and terminal outcomes
publish only after the exact active snapshot drains. A batch failure produces
no false barrier acknowledgement and leaves the coordinator fail-closed;
shutdown still reports cleanup separately rather than claiming quiescence.

An internal boot session consumes the topology into that coordinator, so no
second runner owner or raw-id authority survives beside it. The same ordered
MPIDR metadata drives FDT CPU nodes, PSCI targets, run identities, and PPI
routing. `CPU_ON` diagnostics expose only index, MPIDR, and transaction stage;
the guest entry, context, registers, memory contents, and host pointers remain
redacted. An entry is accepted only when four-byte aligned and contained in the
already mapped guest RAM. `CPU_OFF` uses the same indexed transaction boundary:
the exact pending token is required, success writes no return register, the last
committed online CPU is denied, and scheduler removal completes before the
power model publishes `OFF`. Later re-entry reuses the fixed owner and shared
GIC, writes the retained `SCTLR_EL1` to zero for the Linux warm-entry contract,
and does not claim a full architectural reset.

`CPU_SUSPEND` never transfers owner authority or marks the caller offline. Its
power token and runner token are both exact, and coordinator mode transitions
are accepted only while that online member is idle. A suspended generation can
read timer state and publish only its configured PPI; it cannot run guest code.
Timer wake restores runnable scheduling before the original owner writes X0,
and any pending raw HVF cancellation is absorbed by a later identified run
before guest execution resumes. Diagnostics retain only index, MPIDR, and
transaction stage, not ignored guest arguments or timer values.

PSCI/SMCCC discovery is a fixed guest ABI, not a probe of Apple host firmware
or vulnerability state. PSCI 1.0 feature results come from one reviewed static
table and expose only delivered function IDs plus zero CPU_SUSPEND mode/format
flags. SMCCC 1.1 architecture discovery returns success only for VERSION and
the discovery function itself; workaround, SoC ID, KVM PV/vendor, and TRNG
queries return `NOT_SUPPORTED`. No call reads host mitigation policy, logs a
guest query, mutates vCPU power state, or creates deferred coordinator work.
Unknown IDs and nonzero HVC immediates retain the same zero-extended,
fail-closed `NOT_SUPPORTED` result.

Public `InstanceStart` now exposes the same topology for counts through
`min(32, host_max)`; it does not add another owner or capacity authority. A
capacity or later construction failure publishes no partial session and cannot
commit `Running`. Public pause returns only after the aggregate active snapshot
drains, and process shutdown, guest off/reset, SIGINT/SIGTERM, or a runner fault
all converge on the same aggregate stop/join and reverse owner teardown. Signed
dual-process tests keep sockets, serial paths, and lifecycle controls isolated;
faults continue to redact host paths and guest/HVF values.

Same-state public pause/resume acknowledgements are controller no-ops, not
ownership bypasses: the process must still retain its started session, no second
backend command or generation is issued, and state remains unchanged. Their
latency field measures the successful API request, so recording it does not
claim that a backend transition occurred.

Online peers may hold the shared MMIO dispatcher while the boot worker handles
another member's completed step. Runtime notification dispatch therefore waits
for that short owner critical section under the existing guest-memory then
dispatcher lock order. Snapshot capture, preflight, and control-plane mutation
retain the nonblocking busy policy, so this does not turn their admission checks
into unbounded waits.

## UFFD-Equivalent Pager Authority Boundary

The checked
[snapshot paging contract](../compat/firecracker/v1.16.0/snapshot-paging-contract.md)
records positive public-macOS feasibility, not a shipped pager. Native-v1
`Uffd` still rejects before path, socket, artifact, or backend access.

The accepted later boundary uses two in-worker protection planes: public Mach
task exceptions mediate host accesses to owned absent guest pages, and HVF
stage-two permissions mediate guest accesses. Task and thread ports remain
inside the worker because an exception receiver has whole-task authority, not
UFFD's registered-range authority. Unrelated exceptions must be forwarded and
the prior task configuration conditionally restored.

The external content owner receives neither task ports nor host virtual
addresses. The launcher reduces the configured Unix path to one connected
stream, and the worker exposes only versioned, bounded, offset-based
`bangbang-pager-v1` requests. This adds no ambient network entitlement,
dynamic Mach service, root requirement, private API, entitlement weakening, or
host-wide setting.

Handler failure while a host instruction is suspended is a mandatory
fail-closed supervision gate. Later implementation must bound I/O and take one
documented terminal path; it may not fabricate zero or stale contents, fall
through accidentally to `SIGBUS`, swallow a genuine crash, or wait
indefinitely. External/shared mappings that bypass the task-local bridge remain
pre-resource rejections until independently certified.

## Guest Data Exposure

The guest is untrusted. vCPU execution, guest memory contents, virtqueue
descriptor chains, MMIO accesses, block requests, virtio-net TX descriptor
metadata and payload bytes, virtio-net RX buffer descriptors, virtio-vsock
packet headers, virtio-vsock TX available-ring heads, virtio-vsock TX payload
descriptor ranges, virtio-vsock TX used-ring completion writes, virtio-vsock
RX available-ring heads, virtio-vsock RX buffer descriptor ranges,
virtio-vsock RX used-ring completion writes, virtio-vsock queue notifications,
and future device inputs must be validated before they affect host resources.
Trapped system-register exits are guest-visible CPU behavior and must stay
explicit. The current HVF runner emulates only the early-boot `OSDLR_EL1` and
`OSLAR_EL1` OS lock RAZ/WI behavior needed by the pinned Firecracker kernel;
unsupported trapped system registers fail closed instead of being treated as
generic no-ops.

The current virtio-mem device treats every guest request as untrusted dynamic
memory ownership input. It validates block alignment, count, overflow, usable
range, requested-size capacity, and prior block state before invoking the HVF
owner. Plug work creates exact guest-memory regions in the VM's selected
anonymous or shared profile and maps them into the active VM; unplug work can
split or combine block-owned ranges while
removing only complete owned mappings. Device block state and `plugged_size`
commit only after backend mutation and guest-visible response publication both
succeed. If a later subrange or used-ring publication fails, already-applied
subranges roll back in reverse order; rollback failure is reported as a
fail-closed error, not simulated success. Session shutdown retains reverse
owner teardown, and no
mapping is shared across VMs. Lowering requested size asks the guest to
cooperate; it is not host-forced device deletion, snapshot admission, or a
promise that untrusted guest progress will release memory.

The current virtio-balloon foundation derives a startup-attached selected-transport virtio
shell from stored control-plane configuration. It exposes guest-visible
identity, feature, queue, and config-space registers without changing mapping
ownership. Guest config-space writes update only local device register state.
The backend-neutral inflate notification dispatcher reads bounded PFN payloads,
compacts them into ranges, validates every completed range against owned guest
memory, and acknowledges descriptor heads with zero-length used-ring entries.
The deflate path follows the same validation/publication boundary. Completed
inflate descriptors update owning-device inflated-page accounting and, before
runtime dispatch returns, pass their byte ranges to the bounded discard owner.

Discard validates the whole guest range before its first host operation, splits
work at every independently owned mmap, and aligns each segment inward to the
host page size. Partial host-page edges are skipped rather than zeroing adjacent
live guest data; in particular, a 4-KiB guest range within one 16-KiB Darwin
page produces no host advice. On Darwin, isolated checked wrappers issue
`MADV_ZERO` and only then `MADV_FREE`. A zero failure suppresses free, later
independent segments remain best effort, and diagnostics expose requested,
actual advised, skipped, and failed bytes plus stage classes without host
pointers. Unsupported targets report failure instead of substituting a
different operation or simulating success. Each validated inflate or deflate
descriptor fallibly prepares the next compact paired PFN-accounting value
before publishing its used entry, then commits that value by move only after
publication succeeds. Preparation or publication failure preserves the prior
accounting value, while a later descriptor failure preserves only the already
committed prefix. Deflate removes overlapping ranges and reset clears the
ledger. This logical guest-cooperation state does not prove that host advice
succeeded, and no synchronous RSS or footprint reduction is promised.

Paused balloon capture validates the negotiated feature/layout relationship,
active queue cursors, pending statistics head, hinting state, and compact PFN
ranges against mapped guest memory. Guest config `actual_pages` remains a
separate captured fact from host paired accounting so an untrusted or transient
guest mismatch is not promoted to host truth. The detached value retains no
guest-memory borrow, lock, endpoint, host handle, or wall-clock value. Capture
readiness does not yet define a serialization or restore contract.

Free-page hinting command descriptors remain limited to 4-byte command
identifiers stored in active device state. Range descriptors are accepted for
discard only while the host command is active and the guest command matches it;
missing, stale, STOP, and DONE ranges cannot trigger advice. Host advice failure
does not rewrite used-ring publication, queue-interrupt intent, inflated-page
accounting, or hint command state. The HVF boot loop owns mutable mapped memory
through this synchronous dispatch and resumes only after it returns. Bounded
statistics reports can update optional statistics fields.

Free-page reporting descriptors are direct guest-memory range declarations, not
trusted payloads. Dispatch accepts at most the queue's bounded 256-entry chain
limit and requires every reporting descriptor to be device-writable. Empty
ranges, wrong-direction descriptors, checked address-overflow failures, unmapped
ranges, and platform advice failures are recorded as failed best-effort attempts
without preventing later available chains from running. Each valid range is
fully mapping-validated and passes through the same per-owner, inward host-page-
aligned zero/free boundary as inflate and hinting. Discard completes before the
descriptor is published used; if used-ring publication then fails, diagnostics
retain the discard outcome but do not claim descriptor completion or interrupt
intent. Reset retains no reporting ledger. Requested reporting bytes are
observability input counts and must never be presented as advised or reclaimed
bytes.
Runtime balloon target-size updates change only the stored target and active
virtio-balloon `num_pages` config-space value, then signal a config interrupt;
they do not map, unmap, reclaim, or release host memory. Balloon statistics
queries read the stored target and internal inflated-page count only; they do
not process guest statistics descriptors or change host memory accounting.
Balloon hinting start and stop commands update only host-owned command state,
mirror that state into active config space, and signal a config interrupt.
Balloon hinting status queries read only the active device's internal host
command identifier and latest 4-byte guest command identifier observed on the
hinting queue. These control-plane paths do not trust guest config-space writes
as host commands or themselves perform host advice; only accepted runtime queue
ranges reach the bounded discard owner.

The serial device has process-owned TX and RX boundaries. With no configured
`serial_out_path`, guest TX uses nonblocking process stdout and a terminal or
FIFO/pipe process stdin becomes guest RX. A configured direct or contained
output instead disables stdin, so one submitted path cannot silently retain an
unrelated ambient input authority. Closed, invalid, regular-file, socket, and
other nonpollable stdin kinds are ignored. Production daemon mode supplies
`/dev/null`, so default TX is discarded and RX is absent there by explicit
launcher policy.

Preparing default stdio duplicates close-on-exec owners but changes status
flags on the shared open-file descriptions. Terminal stdin is also made raw for
byte-exact input. The original input/output flags and terminal attributes are
therefore retained as sensitive process state and restored only after the final
split endpoint owner drops. Failures and debug output identify neither paths
nor descriptors. Reviews must preserve final-owner restoration, exact
per-process ownership, and cleanup on ordinary shutdown, terminal backend
failure, partial startup, and launcher/worker teardown.

Host stdin bytes are untrusted guest-control input. The owner run loop reads at
most the current capacity of the 64-byte UART FIFO, unregisters a full FIFO,
rearms only after guest drain, consumes no input while Paused, and detaches on
EOF or error. It uses the existing readiness owner rather than a side thread.
Anyone who can write the supplying terminal/FIFO can influence guest console
input; Bangbang adds no authentication above the operating-system descriptor
authority. Guest serial output is likewise untrusted data. A serial
`rate_limiter` stays nonblocking: exhausted TX bytes are dropped instead of
sleeping the VM thread or propagating host backpressure. Metrics may expose
counts, including input, errors, overruns, and dropped bytes, but never serial
byte values.

Capture-ready traversal pairs reconstructible serial configuration with the
complete guest-visible UART state while excluding stdout/stdin descriptors,
terminal settings, pipe buffers, TX bytes, metrics, locks, and wakeup handles.
Bangbang-native v1 still accepts only its representable baseline and encodes
the legacy six mutable register bytes; restore constructs fresh default output
with empty metrics. It does not preserve a public output path, limiter budget,
RX bytes/intents, or any host endpoint. This prevents silent inheritance of
source-process authority and is not a Firecracker artifact-compatibility claim.
The aggregate production gate additionally holds two
launcher/App-Sandbox-worker sessions at once and proves that pausing, feeding, closing, or terminating
one default-stdio session cannot advance or tear down the other. Each launcher
retains exactly one worker, and both socket/session roots disappear only with
their owning session; no steady helper or cross-session descriptor authority is
introduced.

Block devices can expose host file contents to the guest and can write to the
backing file when configured read-write. Operators should use dedicated disk
images per microVM and avoid sharing writable backing files between multiple
bangbang processes. The default `cache_type=Unsafe` mode does not advertise
guest flush support. When `cache_type=Writeback` is configured, the block device
advertises guest flush support and handles flush requests through the backing
file `sync_all()` path. Configured block rate limiters must not create shared
global state between processes; each active device owns its limiter budget, and
throttled descriptors remain pending without writing request status, publishing
used-ring entries, or mutating the backing file. Runtime block dispatch may
report a process-local retry delay for the pending descriptor. The HVF boot run
loop uses that delay to schedule a per-session retry wakeup through the vCPU
cancel path; the backend-neutral dispatch path itself still does not sleep or
busy-wait.

Metrics and logger outputs are host observability state, not guest
configuration, and are intentionally omitted from `GET /vm/config`. Current
logger API request and action events are unrestricted host VMM records; they can
expose API method/path metadata but not request bodies or guest serial output.
The guest-triggerable boot-timer callsite alone uses a bounded limiter and emits
one unrestricted warning when delivery recovers. Logger filtering and sink
failures never change the API, action, or guest outcome; rate-limited records
and delivery failures are observable only through process-local counters.
Current session-initial, periodic, explicit, and normal-terminal metrics lines
can expose selected API request counters, startup timing fields, logger and
serial counters, a terse boot run-loop status summary, and minimal device
fields such as block
queue/update/throttling activity, virtio-pmem queue activity, virtio-net packet
counters, and virtio-vsock queue, packet, byte, and connection cleanup counters,
plus virtio-rng request, byte, host-randomness failure, and event-failure
counters, PL031 RTC invalid read/write and error counters, and balloon
inflate/hint/report discard attempts, reporting-requested bytes, actual advised
bytes, skipped-edge bytes, failed attempts, and block latency samples. Counters
and accumulated durations are emitted as increments; startup timing, latest
lifecycle latency, status, and block latency minima/maxima/sample counts are
stores. The typed previous-success baseline advances only after the complete
line is written, so failed or ambiguous writes retain data for an at-least-once
retry. A lower generation emits a full current sample, and absent device
families remain absent rather than being synthesized. None of these fields may
expose Unix socket paths, guest payload bytes, host stream data, worker error
strings, host paths or pointers, guest serial bytes, randomness bytes, host
entropy-source details, guest descriptors, guest memory addresses, or
unexpected guest data. Future observability changes must preserve these
redaction, transaction, and failure-isolation boundaries.

MMDS control-plane contents are process-local in-memory JSON state configured
through the unauthenticated local API socket. Treat metadata as sensitive host
control-plane data: any process that can use the API socket can read, replace,
or patch it. The current implementation bounds the serialized MMDS data store
to the effective `--mmds-size-limit` value, inherited from the HTTP API payload
limit when omitted, can format initialized metadata by path as JSON or
Firecracker-shaped IMDS text, and can model process-local guest GET response
status/content-type/body values, parse complete process-local guest HTTP `GET`
request bytes, map parse failures to deterministic process-local error
responses without echoing malformed request bytes, and serialize process-local
HTTP response bytes for guest delivery while preserving only accepted
`HTTP/1.0` or `HTTP/1.1` status-line versions. Malformed request lines and
unsupported versions use the default safe parse-error response without echoing
arbitrary version tokens. One bounded interface-local network stack serializes
ARP and TCP output around those HTTP bytes and retains exactly one generated
frame until guest RX publication commits. It also has a stateless process-local
MMDS v2 token authority matching Firecracker v1.16.0's AES-256-GCM envelope:
a 12-byte random nonce, encrypted 8-byte little-endian monotonic expiry, and
16-byte authentication tag encoded as exactly 48 standard-Base64 characters.
The immutable `microvmid=<instance-id>` additional-authentication data binds
each token to the controller instance that created it. Keys and AAD are
zeroizing and debug-redacted. Instance/startup configuration and internal HTTP,
session, normalized-packet, staged-frame, and RX-packet debug views redact
identities and packet contents while retaining only safe
shape, count, and length diagnostics. The authority rotates before encrypting
beyond `u32::MAX` tokens under one key, and failed first-use or rotation
attempts do not replace the current key or advance its counter. Validation
applies the 70-byte input gate before decoding, accepts only the current key,
does not mutate authority state, and exposes all malformed, modified,
foreign-instance, stale-key, and expired values through the same invalid-token
result. When MMDS
v2 is configured, process-local guest GET handling requires a valid generated
token before returning metadata. Signed executable e2e coverage requests and
uses a guest token without logging it. Its concurrent two-process case also
moves each opaque token through fixed scratch sectors while both VMs are
paused, requires `401 Unauthorized` when each guest presents its peer's token,
then proves each guest's own token remains valid. The host and guest emit only
static coordination markers, and dynamic token bytes are checked absent from
stdout, stderr, and failure diagnostics.

The runtime first performs Firecracker's speculative target test over a valid
Ethernet header: an ARP target protocol address or IPv4 destination must equal
the configured MMDS address. VLAN-tagged and non-target traffic are not owned;
a target-classified frame is consumed before external egress even if its full
ARP, IPv4, or TCP parse later fails. Exact Ethernet/IPv4 ARP requests update one
pending reply and the interface's last remote MAC. IPv4 parsing requires an
exact total length, deliberately tolerates an unverified header checksum for
offload compatibility, treats non-TCP as unusual consumed traffic, and parses
each fragment independently without reassembly. A parseable first fragment may
reach TCP while a later fragment normally fails TCP parsing; neither is
forwarded to vmnet.

Every MMDS-selected interface owns a separate TCP handler bound to port `80`,
with at most 30 connections, 100 pending resets, a 2,500-byte request buffer per
endpoint, and one response per endpoint. The handler validates MSS, sequence and
ACK progress, receive and remote windows, in-order data, duplicate/out-of-window
traffic, FIN/RST state, and 40-second bounded eviction. Responses are segmented
to the negotiated MSS and remote window; unacknowledged output is retransmitted
after 1.2 seconds and the fifteenth timeout resets and removes the connection.
ARP replies precede reset and connection output. Exactly one serialized frame
is retained across repeated peeks and limiter/no-buffer outcomes until the
guest used-ring publication consumes it, so delivery metrics cannot double
count a retry.

Only future protocol timeouts enter the existing generation-owned network
scheduler; immediate or retained output remains packet readiness. The earliest
protocol and limiter deadline is rearmed across ordinary pause/resume, canceled
on terminal shutdown, and recomputed after PCI DELETE so a removed generation
cannot wake a same-ID replacement. No MMDS timer thread or callback-side guest
mutation exists. All interfaces share the process-local metadata/token state
and top-level metrics but not ARP, TCP, reset, response, or retained-frame state.
All-MMDS configurations use the same stack through MMDS-only packet I/O, drop
non-MMDS TX, and open no vmnet resource. Signed direct-rootfs MMIO and PCI
coverage renews a v2 token, receives a segmented 49,152-byte response, drops one
ACK, and observes retransmission before completion.

The stack still does not implement a general ARP cache, gratuitous ARP, ARP
timeouts/retries, or IPv4 fragment reassembly; capture/restore deliberately
starts fresh network and MMDS sessions under the owning snapshot work.
Consistent with Firecracker v1.16.0's
[MMDS security considerations](https://github.com/firecracker-microvm/firecracker/blob/v1.16.0/docs/mmds/mmds-design.md#security-considerations),
this detour is not an outbound firewall: guest traffic remains untrusted, and
host policy must block access to restricted host addresses.

## Multi-Process Operation

Multiple bangbang processes can run on one host, but they must not share mutable
host resources unless sharing is intentional and externally synchronized.

Use unique paths for:

- API sockets
- metrics files or FIFOs
- logger files or FIFOs
- writable block backing files
- writable pmem backing files
- configured vsock socket paths
- future host network devices or sockets
- temporary test files

Each process owns its own VMM controller state and observability sinks. There is
no global registry that prevents two processes from using the same host path.
Path isolation across sessions therefore remains an operator responsibility.
Startup grants reject aliases within one batch but do not coordinate two
independent launchers or provide hard revocation. Granted API/vsock publication
also refuses to replace an existing child and cleans only an identity-matching
socket; callers must still allocate nonconflicting children across launchers.

Each production launcher owns exactly one sandbox worker and does not share
that child across invocations. Every invocation has a random protocol identity
and locked private namespace, and signed tests prove that one crashed session
does not terminate or clean a concurrent peer. This does not allocate unique
external resources or coordinate caller-supplied paths across launchers.

## Current Non-Goals

The current scaffold does not implement:

- dynamic post-Ready grants or a complete hard-revocation broker policy
- Developer ID possession, notarization, kernel launch constraints, or an
  automatic restart/reconnect policy
- a Firecracker-jailer replacement
- privilege dropping
- general-purpose host resource brokering beyond the fixed granted-vsock
  port-only and contained vhost-user exact-child connection facets
- broader snapshot profiles or Firecracker artifact compatibility beyond the
  exact contained native-v1 describe/create/load resource boundary
- full containment for network, guest-visible MMDS, or vsock beyond the exact
  granted Unix-socket subset; the
  current network interface configuration path validates and stores
  configuration strings, and internal
  virtio-net notification dispatch can parse guest TX descriptor metadata and
  pass validated TX frame payloads to injected packet I/O selected per configured
  interface, and can copy injected RX packet bytes into validated guest RX
  buffers through the same boundary. On macOS, the process crate has internal
  vmnet descriptor, lifecycle, start owner, concrete system start/stop backend,
  packet descriptor, single-packet system read/write backend boundaries, a
  cleanup-owning packet backend for retaining stop-on-drop ownership while
  delegating packet I/O, and an internal virtio-net adapter that can move
  packets between vmnet and the runtime packet traits, detour speculatively
  targeted MMDS traffic before external egress, and share one bounded
  interface-local ARP/IPv4/TCP stack between TX classification and RX delivery.
  The stack owns 30 connections, 100 resets, fixed 2,500-byte receive buffers,
  one response per endpoint, segmentation/flow control/ACK/FIN/RST state,
  retransmission and eviction deadlines, and one commit-retained output frame.
  Protocol deadlines merge into the existing generation-safe owner scheduler.
  An MMDS-only adapter reuses the same stack without opening vmnet when every
  configured interface is listed in MMDS config, plus a bounded per-interface registry that owns independent adapters,
  explicit vmnet stop/drop, and exact generation take/restore, and an internal `host_dev_name` mapping for
  `vmnet:host`, `vmnet:shared`, and `vmnet:bridged:<interface>`. The current
  model stores at most 16 configured network interfaces. Startup revalidates
  that limit before selecting packet I/O, opens vmnet resources only for
  non-MMDS-only startup when configured interfaces use the supported names,
  keeps no-network startup on an empty hotplug-capable registry, and enforces
  bounded lifecycle-v5 session-and-vmnet-authority owner for contained startup,
  restore, runtime insertion, and capture. Public PCI PUT/DELETE coordinates
  that registry with exact PCI, metrics, retry, and live-config ownership;
  MMDS-only runtime entries consume no vmnet capacity but still require the
  exact session owner, while actual live vmnet entries are charged to the bound.
  The
  default networkless code-sign profile rejects every positive authority
  before worker spawn but supports the signed all-MMDS hotplug path. Explicit vmnet packaging can bind a caller-approved
  profile to a positive authority only after exact inspection and current-host
  authorization, while real production connectivity policy and full public
  vmnet packet-movement proof beyond the documented operator-owned boundary
  remain missing. The current
  vsock API path validates and stores `guest_cid` plus `uds_path` before boot.
  The runtime crate has an internal virtio-vsock prepared resource, MMIO
  registration helper, config-space, packet header model, TX descriptor packet
  parser, TX available-ring drain helper with used-ring descriptor completion,
  MMIO handler skeleton with active queue metadata retention and RX/TX
  notification dispatch, startup FDT attachment, startup-level RX/TX
  notification dispatch, and HVF queue interrupt signaling that expose only the
  configured guest CID through bounded config reads. The runtime can also parse
  host `CONNECT <PORT>` requests, allocate Firecracker-shaped host local ports,
  retain host-initiated accepted streams in an internal table, expose one-shot
  guest-facing `VSOCK_OP_REQUEST` packet headers for retained host connections,
  dispatch those request headers into validated writable guest RX descriptors,
  accept one pending host connection per dispatch pass into an owned
  nonblocking stream, retain bounded accepted streams across partial handshakes
  and retained connection records, drop invalid accepted-stream handshakes
  without exposing host paths, retry RX delivery when pending host requests
  exist, and acknowledge guest `VSOCK_OP_RESPONSE` packets for delivered host
  requests by writing `OK <local_port>\n` to the retained host stream. Short or
  failed acknowledgement writes drop the retained connection and release its
  host local port. Unsupported or orphan host-destined guest TX packets can
  queue bounded guest-visible `VSOCK_OP_RST` headers. Supported guest
  `VSOCK_OP_REQUEST` packets attempt nonblocking connects to Firecracker-shaped
  `uds_path_<PORT>` sockets, retain successful streams in a bounded
  guest-initiated connection table, and deliver guest-visible
  `VSOCK_OP_RESPONSE` headers; connect or retention failures deliver
  guest-visible `VSOCK_OP_RST` headers and retain no stream. Established
  host-initiated or guest-initiated connections can forward bounded guest
  `VSOCK_OP_RW` payload bytes to retained host streams, keep a bounded
  four-packet per-connection guest-to-host retry queue for partial or
  would-block nonblocking writes, and retry pending bytes on later notification
  dispatch before accepting more guest `RW` data for the same connection. Queue
  overflow or terminal write failures drop the retained stream before queuing a
  guest-visible reset. Established host-initiated and guest-initiated
  connections can retain a bounded
  four-packet per-connection backlog of host `VSOCK_OP_RW` payloads and deliver
  one queued payload at a time into validated guest RX buffers,
  guest `VSOCK_OP_RST` packets drop matching retained host-initiated or
  guest-initiated streams without queuing guest-visible RX output, partial guest
  `VSOCK_OP_SHUTDOWN` packets record receive/send closure state, suppress
  later data movement in the closed direction, and apply TX shutdown control
  before same-window RX host-payload delivery, while full guest
  `VSOCK_OP_SHUTDOWN` packets drop matching retained streams before queuing a
  guest-visible reset. Valid guest `VSOCK_OP_CREDIT_UPDATE` packets for
  established retained streams are consumed without queuing a reset, and valid
  guest `VSOCK_OP_CREDIT_REQUEST` packets queue zero-payload guest-visible
  `VSOCK_OP_CREDIT_UPDATE` headers on the existing RX path. Dynamic 64-KiB
  credit windows use wrapping counters and bounded reservations; clean host EOF
  queues shutdown after pending payloads, while terminal read/write failures
  queue a reset. Request and shutdown cleanup are bounded to two seconds. Both
  initiation directions share one 1023-connection active budget, incomplete
  accepted host handshakes are bounded separately to 256, and host-local ports
  use a detached round-robin last-used cursor.
  Startup preparation
  creates a nonblocking host Unix listener at `uds_path` and cleans it up only
  while the path still matches the created socket inode. `EVENT_IDX` is active
  on RX/TX, indirect descriptors are a supported bangbang extension, and the
  event queue supports validated `TRANSPORT_RESET` publication plus guest
  acknowledgement of the runtime-only restored-origin RX gate. This
  **implemented supported live MMIO-or-PCI startup/Unix-socket subset** still is not full
  containment: there is no global host-path broker, PATCH/DELETE/runtime
  hotplug, broader CID routing, or broader event type. The private redacted
  MMIO/PCI capture and listener/connector-parameterized empty-state reconstruction layer
  grants no path authority and persists no live peer work. Production quiesced
  capture now validates one exact source owner, publishes reset, and detaches
  connection work while retaining listener/connector authority for fresh
  traffic. Internal destination preparation now validates the captured and
  optional override selectors before resource access, uses owner-only
  stale-safe direct publication or exact transactional contained authority,
  and transfers cleanup ownership through one single-use runtime adoption.
  The checked ledger certifies all eight API/live records. Public native-v1
  encoding/placement and invocation, restored-guest acknowledgement/reconnect/
  override proof, clone/versioning, and portability remain #1490 work.
- log rotation, syslog, journald, tracing, remote telemetry, or process-global
  panic/fatal observability durability
- a public serial streaming API, generalized serial artifact encoding/restore,
  and destination-authorized endpoint reconstruction/portability policy

These are future security design and implementation topics. PRs that add new
host-facing resources should update this document and include resource-specific
validation, redaction, cleanup, concurrency, and multi-process tests where
practical.
