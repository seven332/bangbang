# macOS Host Security Model

This document describes the current host security posture for bangbang. It is a
baseline for review and future work, not a claim that bangbang already provides
Firecracker's full production isolation model on macOS.

## Security Boundary

bangbang currently follows Firecracker's one-process-per-microVM model. One
`bangbang` process owns one API socket, one VMM controller, one HVF-backed
startup path, and the host resources configured for that microVM.

Normal startup uses non-clobbering fd-table preallocation as a Firecracker-style
performance guard. Failing to read the descriptor limit or duplicate a
descriptor is non-fatal; failing to close a successfully duplicated descriptor is
fatal. The setup does not overwrite inherited high-numbered descriptors.

The current trusted boundary is the host user account and the local filesystem
permissions around configured host paths. API clients, API request bodies,
guest-provided MMIO data, guest memory, and configured host paths must be treated
as untrusted input.

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

bangbang currently rejects Linux-specific Firecracker process options rather
than silently accepting them. There is no production sandboxed distribution,
resource broker, launcher process, or Firecracker-jailer replacement yet.

Apple App Sandbox is a supportable containment building block, not a direct
jailer port. The signed integration runner packages real binaries as minimal app
bundles with only App Sandbox and Hypervisor entitlements. The complete HVF
lifecycle suite runs inside that boundary. Process tests prove that a socket in
the app container is usable and cleaned on `SIGINT`, while the default `/tmp`
socket and a config path outside the container are denied without path leakage.
The shipped CLI remains an ordinary non-sandboxed executable; no production app
bundle or resource policy is provided.

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
- Operator-owned policy: socket-directory permissions,
  host-path ownership, and current resource-sharing rules are deployment
  assumptions until a launcher or resource broker exists. Document the
  assumption and test that one `bangbang` process does not clean up resources it
  no longer owns.

When a capability moves between these categories, update the compatibility docs,
validation matrix, tests, and related issue links in the same PR.

## Isolation Compatibility Checklist

Use this checklist when reviewing Firecracker-facing host isolation changes:

| Area | Current status | Review expectation |
| --- | --- | --- |
| Linux jailer, seccomp, namespaces, cgroups, chroot, and privilege dropping | Platform-limited unsupported | Reject matching Firecracker process options or document a concrete macOS replacement before accepting any no-op behavior. |
| API socket ownership | Implemented subset | Keep owner-only socket permissions, final-path ownership checks, and owner-only cleanup tests current when API socket behavior changes. |
| Host path policy | Operator-owned with per-resource validation | Redact sensitive path details in errors, avoid opening paths during pre-boot storage unless the resource explicitly requires it, and test cleanup for owned resources. |
| HVF entitlement and code signing | Implemented normal and App Sandbox validation paths | Keep real HVF tests in signed targets and keep unsupported CI hosts on explicit compile/sign-only validation, not silent skips. |
| Network and vmnet | Implemented virtio-MMIO/MMDS-only subset; direct vmnet conditional | Keep supported `host_dev_name` forms, startup validation, MMDS-only behavior, entitlement requirements, and non-goals documented when network behavior changes. |
| macOS App Sandbox | Signed feasibility and resource-denial boundary validated | Keep the integration app bundles minimal and do not claim that the ordinary CLI or a production distribution is sandboxed. |
| Launcher or resource broker | Unimplemented product architecture | Keep host-path and shared-resource authority as operator responsibility unless a separately approved broker design is implemented. |

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

Private capture holds paused-worker admission, block/entropy retry quiescence,
and all four runner operation domains through non-memory encoding and complete
memory streaming. Cancellation is checked between fixed stages and 1 MiB
chunks. Failure returns no binding or bundle, publishes no final state marker,
and drops the consumed writer and auxiliary guard before admission release.
Supervisor shutdown signals cancellation before joining, but Rust cannot
forcibly preempt an arbitrary blocking `write`; the public request path therefore
supplies only a publisher-owned regular staging file, never an arbitrary caller
writer. The capture writer names no path and the publisher owns cleanup of its
private staging entry. A partially written staging inode is never interpreted
as committed state.

PL031 RTC is represented by fixed MMIO metadata and an explicit fresh-device
policy. No mutable RTC register or alarm state is persisted, so no continuity
claim is permitted. Active SVE/SME or breakpoint/watchpoint state is rejected
rather than silently omitted, and optional devices remain outside the accepted
profile.

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

The current isolation boundary is one macOS process running as the invoking host
user. bangbang does not yet split privilege between a launcher, broker, and VMM
worker, and it does not install a sandbox profile. The host user account,
filesystem permissions, API socket directory, and per-resource validation are
therefore the only deployed host isolation controls.

Use the following boundaries when designing or reviewing macOS isolation work:

| Boundary or option | Current behavior | Future direction |
| --- | --- | --- |
| Operator-owned private directories | Required for API sockets, vsock sockets, observability sinks, and other configured paths that should not be shared. | A launcher or broker could create and own these directories before starting a VM process. |
| HVF entitlement and code signing | Required to execute Hypervisor.framework code paths; signed integration wrappers cover the validation path. | A production launcher may control which executable receives the entitlement, but the entitlement itself is not guest containment. |
| macOS App Sandbox | Integration-only app bundles run all signed HVF lifecycle tests and explicit process allow/deny checks. The ordinary CLI is not sandboxed. | A production bundle must define resource-specific container, bookmark, entitlement, and distribution policy before it is a claimed deployed boundary. |
| Launcher or resource broker | Not implemented. | A future process could prepare privileged or shared host resources, pass owned descriptors to the VMM, and centralize cleanup policy. |
| Firecracker Linux jailer model | Platform-limited unsupported as a direct port. | Keep Linux jailer, seccomp, namespaces, cgroups, chroot, and privilege-drop flags rejected or documented until macOS replacements exist. |

This document intentionally does not define a sandbox profile, broker protocol,
privilege-dropping flow, or new public API. PRs that add host resource types
should state which current boundary protects the resource and whether a future
launcher, broker, or sandbox profile would need to own it.

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
sandboxed bangbang would therefore need a container-only resource policy or a
separately designed launcher/broker that transfers authorized resources. This
repository does not claim or ship either architecture.

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
configured interfaces and retains stop-on-drop cleanup ownership inside the
process.

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
of at most 200 packets and 256 KB. The current bangbang system backend discards
vmnet's start-completion MAC, MTU, and maximum-packet-size values and does not
register the packet-available event callback. It retains synchronous
single-packet read/write adapters, injected backend tests, and stop-on-drop
cleanup, but its generic 16-interface configuration cap is not enforcement of
Apple's per-guest resource policy. No signed test uses the restricted
networking entitlement or proves direct-vmnet guest connectivity.

Configured RX/TX token buckets are implemented as device-local queue admission
with retained work and session-owned retry wakeups. They are not packet
filters, a host firewall, or a NAT policy, and current signed limiter evidence
uses MMDS-only packet I/O rather than direct vmnet. The boundary still lacks
packet filtering, production network isolation, a sandbox profile, a
launcher/resource broker, runtime network hotplug, limiter-specific metrics,
network snapshot state, and full Firecracker public packet-movement parity.

## API Socket Handling

The API socket is a local control interface with no protocol-level
authentication. Any process that can connect to the socket can send supported
API requests.

When binding the socket, bangbang refuses to overwrite an existing final socket
path. It first binds a temporary sibling socket, records the socket device and
inode, restricts that socket inode to owner-only permissions, publishes it to
the requested path, and verifies that the published path still refers to that
socket. It removes the path on shutdown only when it still refers to the socket
created by this process. Forced termination, such as `SIGKILL`, can still leave
a stale socket path that the operator must remove.

For multiple bangbang processes, use separate socket paths in directories whose
ownership and permissions match the intended control boundary. Do not share a
world-writable parent directory unless the sticky-bit and naming policy are
understood and acceptable for the deployment.

## Host File Paths

Host paths configured through the API are untrusted input. The current behavior
is resource-specific:

- `/boot-source` stores kernel and optional initrd paths during configuration.
  Files are opened later during `InstanceStart` with read-only nonblocking
  access. Startup rejects inaccessible, non-regular, or empty payload files,
  and API-facing startup errors must not echo the configured path.
- `/drives/{drive_id}` stores block backing paths during configuration. Backing
  files are opened later during `InstanceStart`. Runtime
  `PATCH /drives/{drive_id}` opens a replacement backing for an existing active
  drive before mutating stored configuration, refreshes only the matching
  virtio-block MMIO handler, and leaves the old backing and stored
  configuration in place if opening or handler lookup fails. Limiter-only
  runtime updates do not reopen host backing paths; configured limiter buckets
  update only process-local active device state and stored drive configuration.
  It does not implement block-device hotplug or removal. Block rate limiters are
  process-local runtime state created during startup preparation or runtime
  drive update. Exhausted limiters leave the descriptor pending for a later
  dispatch opportunity instead of sleeping, busy-waiting, writing request
  status, publishing a used-ring entry, or mutating the backing file. Active
  HVF boot sessions schedule block retry wakeups with per-session state so one
  VM cannot wake or share limiter state with another VM. Firecracker v1.16.0's
  optional runtime drive attach/remove instead requires PCI transport, a guest
  rescan after attach, and guest removal before host DELETE. bangbang's current
  MMIO path rejects runtime PUT and DELETE without using a proposed backing or
  mutating device state; a future PCI design must make that guest/operator
  coordination an explicit lifecycle boundary. Configured vhost-user sockets
  also remain rejected. A future vhost-user frontend would grant an external
  backend access to guest-memory mappings and queue notifications, so it needs
  separate shared-memory authorization, backend containment, lifecycle,
  cleanup, and failure policy rather than only accepting a socket path.
- `/pmem/{id}` stores Firecracker-shaped pmem backing paths during pre-boot
  configuration after rejecting empty paths, and reports them through
  `GET /vm/config`. Startup opens each configured path with nonblocking
  read/write access according to the configured read-only flag, verifies it is a
  non-zero regular file, mmaps it to a 2 MiB-aligned host range, and keeps the
  file handles and mappings with the boot resources. Startup also assigns
  deterministic non-overlapping 2 MiB-aligned guest physical ranges after the
  aarch64 MMIO64 gap, skipping current guest RAM, and records those ranges in
  the internal virtio-pmem config-space `start`/`size` fields. HVF startup
  creates the VM with the framework-reported maximum IPA size, copies each
  prepared pmem mapping into an HVF-compatible anonymous shadow, and registers
  that shadow at the guest physical range after DRAM mapping, using read-only
  HVF permissions for read-only pmem and read/write non-executable permissions
  for writable pmem. Writable shadows are copied back to the backing file with
  positional writes and a data sync for guest queue-driven flush requests and
  after clean HVF unmap; read-only shadows never write back, and failed unmap
  cleanup does not flush memory that HVF may still reference. Startup also
  attaches each prepared pmem device as a guest-visible virtio-mmio/FDT node
  whose config-space exposes the assigned `start` and `size` values. It does
  not normalize stored host paths, and shadow allocation, HVF registration,
  MMIO attachment, flush, or writeback errors
  identify the pmem ID and guest range without echoing `path_on_host`.
  Configured rate limiters are rejected without replacing stored pmem
  configuration. Root-device boot semantics, runtime updates, direct
  file-backed HVF mapping, dirty-range tracking, and hot-unplug remain
  deferred.
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
  value-free. Unsupported request/profile dimensions fail before artifact I/O;
  an admitted paused create opens only preflighted namespaces, temporarily
  closes ordinary boot-worker command admission, and acknowledges process-local
  block/entropy retry quiescence through complete capture and memory streaming.
  Load freshness uses
  successful configuration history plus current non-logger/metrics state, so
  explicit defaults and residual MMDS presence fail closed without treating a
  side-effect-free failed request as configuration. Snapshot execution treats
  paths and restored guest/vCPU/device state as
  untrusted, preserve redaction, and prevent one process from cleaning up or
  overwriting another process's resources. The current boundary is documented
  in [Snapshot Feasibility](snapshot-feasibility.md).
- Native snapshot inspection treats the entire state file as untrusted binary
  input. The process opens it nonblocking, accepts only a regular file, caps the
  complete read at 16 MiB plus the 40-byte envelope overhead, and rechecks the
  cap while reading. The pure decoder uses checked length conversion and
  arithmetic, requires exact consumption, validates CRC before semantic
  compatibility, and publishes no payload or metadata until all checks pass.
  Command-path and payload debug output is redacted, and read errors retain only
  `ErrorKind`, not the host path. CRC-64/Jones detects accidental corruption;
  it is not authentication, and a party that can rewrite the file can recompute
  it. Future payload schemas must therefore stay memory-safe and fail closed
  even for checksum-valid attacker-controlled bytes.
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
  to retry under unchanged names.
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
  guest bytes remain redacted.
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
  snapshot restore: a higher layer still must validate it against an atomic
  destination cache manifest and define ISB/dependent-read synchronization and
  cache-maintenance policy. The separately queried default
  vCPU configuration's raw CTR_EL0, CLIDR_EL1, and DCZID_EL0 values and its
  independent eight-entry data/unified and instruction CCSIDR arrays are
  read-only metadata, not guest execution state, but can fingerprint the
  exposed virtual CPU model. They must not be logged or persisted without a
  defined need. The independent queries are not one atomic manifest, and even
  together they are not trusted topology or destination policy without cache-
  level interpretation, masks, and validation.
  Firecracker-shaped custom CPU-template values have a narrower control-plane
  boundary. The HTTP/config-file parser validates KVM capability identifiers,
  KVM vCPU-init feature indexes, arm register identifiers, and bitmaps, then
  discards every raw value and retains only a singleton or mixed category.
  Runtime actions, `Debug`, platform errors, logs, `GET /vm/config`, backend
  construction, and snapshots therefore receive no modifier values. Empty
  input stores nothing. Non-empty categories fail before mutation or VM
  construction because Hypervisor.framework exposes feature/cache queries but
  no equivalent contract for setting the created guest feature view. Existing
  live general/system-register setters are execution-state primitives and must
  not be repurposed as arbitrary CPU feature masks. Any future writable subset
  requires a separate Apple API, destination feature-view, atomicity,
  persistence, and snapshot-policy review.
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
- `/vsock` is an **implemented supported live virtio-MMIO/Unix-socket subset**
  that stores the configured Unix socket path during repeatable pre-boot
  configuration and stably rejects post-start replacement. Startup can attach a
  guest-visible virtio-vsock device whose internal MMIO handler
  retains active RX, TX, and event queue metadata after `DRIVER_OK`, and the
  runtime has an internal Firecracker-shaped packet header model plus TX
  descriptor packet parser. Startup-level dispatch can drain RX, TX, and no-op
  event queue notifications, complete descriptor heads, and signal the
  allocated vsock queue interrupt line when completed descriptors require it.
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
  tables each retain at most 256 connections.
  Startup also binds a nonblocking host Unix listener at `uds_path`,
  records the listener socket device and inode, and removes the path on normal
  shutdown only when it still refers to the socket created by this process. It
  never treats a configured path as globally unique, and transport failures and
  signed test diagnostics omit Unix paths and payload bytes. `EVENT_IDX` is
  implemented for RX/TX notification suppression; indirect descriptors are a
  supported bangbang extension, while the event queue otherwise remains a no-op
  live notification surface. Signed Apple Silicon cases incrementally verify at
  least 1 MiB in each direction for both initiation paths, write-half-close/EOF,
  terminal cleanup, and two-stream isolation. PATCH, DELETE, runtime hotplug,
  broader CID routing, general performance/artifact parity, and full event
  payload dispatch remain outside this boundary. Native-v1 snapshot UDS
  override, event-queue `TRANSPORT_RESET`, and post-restore RX gating are the
  stable #543 exclusions; the live subset is not a snapshot-containment claim.
- `/metrics` opens the output path during pre-boot configuration and keeps a
  per-process metrics sink. The `--metrics-path` startup CLI flag uses the same
  sink and host-path error redaction rules before the API socket is served.
  Runtime `FlushMetrics` and periodic runtime metrics flushes every 60 seconds
  can append minimal host observability lines to this sink while the VM is
  running.
- `/logger` opens `log_path` during pre-boot configuration when that field is
  present and keeps a per-process logger sink. Successfully parsed API requests
  can append method/path lines before dispatch, and successful `InstanceStart`
  and `FlushMetrics` can append minimal action-event lines when the configured
  level allows `Info` and the optional module prefix matches the event module.
  API request log lines intentionally omit request bodies, including MMDS
  payloads. Logger startup CLI flags use the same sink and host-path error
  redaction rules before the API socket is served.
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

## HVF Entitlements

Real Hypervisor.framework execution requires macOS support, Apple Silicon, and
the `com.apple.security.hypervisor` entitlement on binaries that enter HVF.

The unsigned Rust test path runs only non-HVF unit tests. Real HVF integration
tests must run through `scripts/run-integration-tests.sh`. This wrapper builds
the selected HVF test binaries or executable e2e artifacts, creates a temporary
entitlement plist when signing is needed, ad-hoc signs copies, and runs signed
targets with one test thread. CI may use `--allow-unsupported` only to compile
and sign on runners that cannot execute HVF; local HVF verification should fail
when HVF is unavailable.

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

Online peers may hold the shared MMIO dispatcher while the boot worker handles
another member's completed step. Runtime notification dispatch therefore waits
for that short owner critical section under the existing guest-memory then
dispatcher lock order. Snapshot capture, preflight, and control-plane mutation
retain the nonblocking busy policy, so this does not turn their admission checks
into unbounded waits.

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

The current virtio-balloon foundation derives a startup-attached virtio-mmio/FDT
shell from stored control-plane configuration. It exposes guest-visible
identity, feature, queue, and config-space registers, but does not map guest
memory or change host memory accounting. Guest config-space writes update only
local device register state. The backend-neutral inflate notification dispatcher
can read bounded PFN descriptor payloads, compact them into page ranges, and
acknowledge descriptor heads with zero-length used-ring entries. The deflate
notification dispatcher follows the same bounded PFN parsing and mapped guest
memory validation path before acknowledging descriptor heads. Completed
descriptors update only internal inflated-page accounting on the owning balloon
device; they do not release, remap, or otherwise alter host memory. The HVF boot
loop can drain these balloon notifications and signal the allocated balloon
interrupt line. Bounded statistics queue reports can update only optional
statistics fields, and a runtime policy trigger can complete the pending
statistics descriptor with a zero-length used-ring entry plus queue-interrupt
intent. Parsed PFNs, statistics descriptors, free-page hinting range
descriptors, and reporting queue data remain untrusted guest input and must not
change host memory accounting or reclaim behavior until those host-side paths
are implemented and reviewed. Free-page hinting command descriptors are limited
to 4-byte command identifiers stored in active device state.
Runtime balloon target-size updates change only the stored target and active
virtio-balloon `num_pages` config-space value, then signal a config interrupt;
they do not map, unmap, reclaim, or release host memory. Balloon statistics
queries read the stored target and internal inflated-page count only; they do
not process guest statistics descriptors or change host memory accounting.
Balloon hinting start and stop commands update only host-owned command state,
mirror that state into active config space, and signal a config interrupt.
Balloon hinting status queries read only the active device's internal host
command identifier and latest 4-byte guest command identifier observed on the
hinting queue. These paths do not trust guest config-space writes as host
commands, parse hinting range descriptors, or reclaim host memory.

The current serial device is a TX-only MMIO output path. By default, guest
serial bytes go to a bounded internal capture buffer; when `/serial` configures
`serial_out_path`, startup opens that host path with nonblocking output
semantics and routes guest TX bytes there. A configured serial `rate_limiter`
must remain nonblocking: exhausted guest TX bytes are dropped instead of
sleeping the VM thread or propagating a host-output backpressure error. Metrics
may report the number of rate-limited dropped bytes, but must not include the
dropped guest byte values. Treat serial output as untrusted guest data. Reviews
for serial-output changes must preserve explicit host-observation behavior,
bounded internal buffering where used, path redaction, limiter state scoped to
one process output, and per-process ownership.

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
logger API request and action events are host VMM events only; they can expose
API method/path metadata but not request bodies or guest serial output. Current
explicit and periodic metrics lines can expose selected API request counters,
startup timing fields, logger and serial counters, a terse boot run-loop status
summary, and minimal device counters such as block
queue/update/throttling activity, virtio-pmem queue activity, virtio-net packet
counters, and virtio-vsock queue, packet, byte, and connection cleanup counters,
plus virtio-rng request, byte, host-randomness failure, and event-failure
counters, plus PL031 RTC invalid read/write and error counters. Serial, vsock,
block, pmem, entropy, and RTC metrics are counters only and must not expose Unix
socket paths, guest payload bytes, host stream data, worker error strings, host
paths, guest serial bytes, randomness bytes, host
entropy-source details, guest descriptors, guest memory addresses, or unexpected
guest data.
Future full logging and metrics support must preserve those redaction
boundaries.

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
arbitrary version tokens. It can synthesize deterministic
Ethernet/ARP replies, Ethernet/IPv4/TCP SYN-ACK frames, and Ethernet/IPv4/TCP
response frames carrying those bytes, expose queued response frames through the
matching virtio-net RX source, and schedule one bounded post-TX RX retry when
that source reports a queued response. It also has a process-local
opaque token authority with a default `1024`-entry active-token store and can
model process-local guest `PUT /latest/api/token` exchanges that return
generated tokens. When MMDS v2 is configured, process-local guest GET handling
requires a valid generated token before returning metadata. The signed
executable e2e coverage includes a direct-rootfs v2 token flow that requests a
guest token and uses it for metadata access, while the guest init script emits
only static success or failure markers and must not log generated tokens or
metadata values. The runtime can
classify ARP requests for the configured MMDS IPv4 address and raw
Ethernet/IPv4/TCP guest packet bytes as MMDS candidates only when they target
the configured MMDS IPv4 address and TCP port `80`; malformed, truncated,
fragmented, non-TCP, and non-MMDS packets are ignored as non-candidates. For
pure empty-payload TCP SYN candidates, the runtime can synthesize deterministic
SYN-ACK frames, and pure empty-payload TCP ACK-only candidates that acknowledge
that deterministic SYN-ACK are consumed without queueing a response. Pure
empty-payload TCP FIN close candidates queue
deterministic ACK and FIN-ACK frames without touching MMDS data or token state.
Unsupported empty-payload TCP control candidates queue deterministic RST frames
without touching MMDS data or token state, and guest-sent packets carrying RST
are consumed without response even when they also carry payload bytes.
For non-empty candidate TCP payloads that acknowledge that deterministic
SYN-ACK and do not carry unsupported SYN or FIN payload control flags, the
runtime can produce the same process-local HTTP response bytes as the existing
guest HTTP helper, including token PUT and MMDS v2 GET token enforcement.
Non-empty candidates carrying SYN or FIN are not interpreted as process-local
MMDS HTTP requests. The process vmnet TX path detours
MMDS ARP requests, pure empty-payload MMDS SYN packets, pure empty-payload MMDS
ACK-only packets that acknowledge bangbang's deterministic SYN-ACK, pure
empty-payload MMDS FIN close packets, unsupported empty-payload MMDS control
packets, guest-sent MMDS packets carrying RST, and non-empty candidates on
interfaces listed in the MMDS config when they acknowledge bangbang's
deterministic SYN-ACK and do not carry unsupported SYN or FIN payload control
flags,
buffers split request headers in bounded per-interface process
state only when each fragment starts at the next expected TCP sequence number,
rejects non-contiguous buffered fragments before appending guest bytes,
synthesizes response frames from deterministic ARP context, deterministic
SYN-ACK context, minimal FIN close context, minimal RST context, or the first
TCP request fragment context, retains those frames in bounded per-interface
queues, delivers queued frames through the matching virtio-net RX source with a
bounded post-TX RX retry, and does not forward handled request payloads to
vmnet. When every configured network interface is listed in MMDS config,
startup can use a process-local MMDS-only packet path that reuses the same
detour and response-queue logic, drops non-MMDS TX frames, and does not open
vmnet resources. This still does
not manage a full ARP cache, emit gratuitous ARP, implement ARP
timeouts/retries, validate broader TCP ACK numbers beyond the narrow ACK-only
and non-empty payload SYN-ACK acknowledgement paths, reassemble out-of-order TCP
data, track TCP state, implement retransmission policy, implement a full
stateful RST policy, or handle session timeouts. Consistent with Firecracker
v1.16.0's
[MMDS security considerations](https://github.com/firecracker-microvm/firecracker/blob/v1.16.0/docs/mmds/mmds-design.md#security-considerations),
this detour is not an outbound firewall: guest traffic remains untrusted, and
host policy must block access to restricted host addresses. Future guest-visible
MMDS work must continue validating device, packet, token, and TCP/session inputs
before expanding the guest-visible data path.

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
Path isolation is therefore an operator responsibility until a future launcher
or resource broker exists.

## Current Non-Goals

The current scaffold does not implement:

- a production macOS App Sandbox distribution or resource policy
- a Firecracker-jailer replacement
- privilege dropping
- host resource brokering
- full containment for network, guest-visible MMDS, snapshots, or vsock; the
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
  packets between vmnet and the runtime packet traits, detour configured MMDS
  ARP requests, pure empty-payload MMDS SYN packets, pure empty-payload MMDS
  ACK-only packets that acknowledge bangbang's deterministic SYN-ACK, pure
  empty-payload MMDS FIN close packets, unsupported empty-payload MMDS control
  packets, guest-sent MMDS packets carrying RST, and non-empty MMDS TX payloads
  that acknowledge bangbang's deterministic SYN-ACK before vmnet forwarding,
  buffer contiguous split MMDS request headers,
  synthesize deterministic ARP replies, MMDS SYN-ACK frames, minimal MMDS RST
  frames, and MMDS TCP response frames, retain bounded per-interface MMDS
  response queues, and expose queued responses through virtio-net RX with
  bounded post-TX retry, plus an MMDS-only adapter that can reuse those queues
  without opening vmnet when every configured interface is listed in MMDS
  config, plus internal providers that can select prebuilt adapters by
  configured interface ID and an internal `host_dev_name` mapping for
  `vmnet:host`, `vmnet:shared`, and `vmnet:bridged:<interface>`. The current
  model stores at most 16 configured network interfaces. Startup revalidates
  that limit before selecting packet I/O, opens vmnet resources only for
  non-MMDS-only startup when configured interfaces use the supported names,
  keeps no-network startup on a no-op TX sink plus empty RX source, and still
  lacks a macOS sandbox, host resource broker, production connectivity policy,
  and full public vmnet packet-movement proof beyond the documented
  operator-owned vmnet boundary. The current
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
  queue a reset. Request and shutdown cleanup are bounded to two seconds, and
  each initiation direction retains at most 256 connections.
  Startup preparation
  creates a nonblocking host Unix listener at `uds_path` and cleans it up only
  while the path still matches the created socket inode. `EVENT_IDX` is active
  on RX/TX, indirect descriptors are a supported bangbang extension, and event
  queue notifications otherwise remain no-op dispatch metadata. This
  **implemented supported live virtio-MMIO/Unix-socket subset** still is not full
  containment: there is no global host-path broker, PATCH/DELETE/runtime
  hotplug, broader CID routing, or full event payload dispatch. Native-v1
  snapshot UDS override, event-queue `TRANSPORT_RESET`, and post-restore RX
  gating remain #543 exclusions.
- complete production logging or metrics policy
- public run-loop control or serial input, rate-limiting, and streaming policy

These are future security design and implementation topics. PRs that add new
host-facing resources should update this document and include resource-specific
validation, redaction, cleanup, concurrency, and multi-process tests where
practical.
