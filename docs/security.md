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
than silently accepting them. There is no macOS sandbox profile, resource broker,
launcher process, or Firecracker-jailer replacement yet.

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
| HVF entitlement and code signing | Implemented validation path | Keep real HVF tests in signed targets and keep unsupported CI hosts on explicit compile/sign-only validation, not silent skips. |
| vmnet networking | Deferred feasible macOS work | Treat live connectivity, host policy, and cleanup as open design work until a public vmnet integration proof exists. |
| macOS sandboxing | Deferred feasible macOS work | Do not claim production containment until a sandbox profile and resource policy are designed and tested. |
| Launcher or resource broker | Deferred feasible macOS work | Keep host-path and shared-resource isolation as operator responsibility until a separate broker owns privileged preparation and cleanup. |

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
  VM cannot wake or share limiter state with another VM.
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
- `/snapshot/create` and `/snapshot/load` currently parse Firecracker-shaped
  snapshot paths before returning unsupported faults, and they do not open or
  create snapshot state or memory files. Future snapshot support must treat
  snapshot paths, memory backend paths, restored guest memory, restored vCPU
  state, and restored device state as untrusted input, preserve path redaction,
  and prevent one process from cleaning up or overwriting another process's
  snapshot resources. The current implementation boundary is documented in
  [Snapshot Feasibility](snapshot-feasibility.md).
- `/vsock` stores the configured Unix socket path during configuration. Startup
  can attach a guest-visible virtio-vsock device whose internal MMIO handler
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
  the existing RX path. Host-stream EOF or read failures drop the retained
  stream and queue a guest-visible `VSOCK_OP_RST`.
  Startup also binds a nonblocking host Unix listener at `uds_path`,
  records the listener socket device and inode, and removes the path on normal
  shutdown only when it still refers to the socket created by this process. It
  does not route CIDs beyond current host/guest checks, dispatch real event
  payloads, implement Firecracker's full graceful-shutdown timeout/kill-queue
  behavior, or implement full virtio-vsock credit accounting yet.
- `/metrics` opens the output path during pre-boot configuration and keeps a
  per-process metrics sink. The `--metrics-path` startup CLI flag uses the same
  sink and host-path error redaction rules before the API socket is served.
  Runtime `FlushMetrics` and periodic runtime metrics flushes every 60 seconds
  can append minimal host observability lines to this sink while the VM is
  running.
- `/logger` opens `log_path` during pre-boot configuration when that field is
  present and keeps a per-process logger sink. Successful `InstanceStart` and
  `FlushMetrics` can append minimal action-event lines to that sink when the
  configured level allows `Info` and the optional module prefix matches the
  current minimal action log module. Logger startup CLI flags use the same sink
  and host-path error redaction rules before the API socket is served.
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
logger action events are host VMM events only and do not expose guest serial
output. Current explicit and periodic metrics lines can expose selected API
request counters, startup timing fields, logger and serial counters, a terse
boot run-loop status summary, and minimal device counters such as block
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
stateful RST policy, or handle session timeouts. Future
guest-visible MMDS work must continue validating device, packet, token, and
TCP/session inputs before expanding the guest-visible data path.

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

- a macOS sandbox profile
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
  lacks a macOS sandbox, host resource broker, connectivity policy, and live
  vmnet integration proof. The current
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
  `VSOCK_OP_CREDIT_UPDATE` headers on the existing RX path. Host-stream EOF or
  read failures drop the retained stream before queuing a guest-visible reset.
  Startup preparation
  creates a nonblocking host Unix listener at `uds_path` and cleans it up only
  while the path still matches the created socket inode. It can accept event
  queue notifications as no-op
  dispatch metadata, but it still does not route CIDs beyond current host/guest
  checks, dispatch real event payloads, implement Firecracker's full
  graceful-shutdown timeout/kill-queue behavior, or implement full virtio-vsock
  credit accounting.
- complete production logging or metrics policy
- public run-loop control or serial input, rate-limiting, and streaming policy

These are future security design and implementation topics. PRs that add new
host-facing resources should update this document and include resource-specific
validation, redaction, cleanup, concurrency, and multi-process tests where
practical.
