# macOS Host Security Model

This document describes the current host security posture for bangbang. It is a
baseline for review and future work, not a claim that bangbang already provides
Firecracker's full production isolation model on macOS.

## Security Boundary

bangbang currently follows Firecracker's one-process-per-microVM model. One
`bangbang` process owns one API socket, one VMM controller, one HVF-backed
startup path, and the host resources configured for that microVM.

The current trusted boundary is the host user account and the local filesystem
permissions around configured host paths. API clients, API request bodies,
guest-provided MMIO data, guest memory, and configured host paths must be treated
as untrusted input.

There is no authentication on the HTTP-over-Unix-socket API. Access control is
provided by the socket path and parent-directory permissions. Operators should
place the socket in a private directory and use restrictive permissions or
umask settings on multi-user hosts.

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

## API Socket Handling

The API socket is a local control interface with no protocol-level
authentication. Any process that can connect to the socket can send supported
API requests.

When binding the socket, bangbang refuses to overwrite an existing final socket
path. It first binds a temporary sibling socket, publishes it to the requested
path, records the socket device and inode, and removes the path on shutdown only
when it still refers to the socket created by this process. Forced termination,
such as `SIGKILL`, can still leave a stale socket path that the operator must
remove.

For multiple bangbang processes, use separate socket paths in directories whose
ownership and permissions match the intended control boundary. Do not share a
world-writable parent directory unless the sticky-bit and naming policy are
understood and acceptable for the deployment.

## Host File Paths

Host paths configured through the API are untrusted input. The current behavior
is resource-specific:

- `/boot-source` stores kernel and optional initrd paths during configuration.
  Files are opened later during `InstanceStart`.
- `/drives/{drive_id}` stores block backing paths during configuration. Backing
  files are opened later during `InstanceStart`.
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
  Established guest-initiated connections can forward bounded guest
  `VSOCK_OP_RW` payload bytes to the retained host stream, keep a bounded
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
  RX output. Full guest `VSOCK_OP_SHUTDOWN` packets drop matching retained
  streams, release the host local port when applicable, and queue a
  guest-visible `VSOCK_OP_RST`. Valid guest `VSOCK_OP_CREDIT_UPDATE` packets
  for established retained streams are consumed without queuing a reset, and
  valid guest `VSOCK_OP_CREDIT_REQUEST` packets queue zero-payload guest-visible
  `VSOCK_OP_CREDIT_UPDATE` headers on the existing RX path. Host-stream EOF or
  read failures drop the retained stream and queue a guest-visible
  `VSOCK_OP_RST`.
  Startup also binds a nonblocking host Unix listener at `uds_path`,
  records the listener socket device and inode, and removes the path on normal
  shutdown only when it still refers to the socket created by this process. It
  does not route CIDs beyond current host/guest checks, dispatch real event
  payloads, track graceful half-close state, or implement full virtio-vsock
  credit accounting yet.
- `/metrics` opens the output path during pre-boot configuration and keeps a
  per-process metrics sink. The `--metrics-path` startup CLI flag uses the same
  sink and host-path error redaction rules before the API socket is served.
- `/logger` opens `log_path` during pre-boot configuration when that field is
  present and keeps a per-process logger sink. Successful `InstanceStart` and
  `FlushMetrics` can append minimal action-event lines to that sink when the
  configured level allows `Info`. Logger startup CLI flags use the same sink and
  host-path error redaction rules before the API socket is served.
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
the HVF test binaries, creates a temporary entitlement plist, ad-hoc signs
copies, and runs the signed copies with one test thread. CI may use
`--allow-unsupported` only to compile and sign on runners that cannot execute
HVF; local HVF verification should fail when HVF is unavailable.

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

The current serial device is an internal TX-only MMIO output path with bounded
capture. Public serial output streaming is not implemented. Treat serial output
as guest data; future public exposure must document whether the host is expected
to observe it and how it is bounded.

Block devices can expose host file contents to the guest and can write to the
backing file when configured read-write. Operators should use dedicated disk
images per microVM and avoid sharing writable backing files between multiple
bangbang processes.

Metrics and logger outputs are host observability state, not guest
configuration, and are intentionally omitted from `GET /vm/config`. Current
logger action events are host VMM events only and do not expose guest serial
output. Future full logging and metrics support must avoid leaking host paths
or unexpected guest data in error messages.

MMDS control-plane contents are process-local in-memory JSON state configured
through the unauthenticated local API socket. Treat metadata as sensitive host
control-plane data: any process that can use the API socket can read, replace,
or patch it. The current implementation bounds the serialized MMDS data store
to `51200` bytes, can format initialized metadata by path as JSON or
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
requires a valid generated token before returning metadata. The runtime can
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
SYN-ACK, the runtime can produce the same process-local HTTP response bytes as
the existing guest HTTP helper, including token PUT and MMDS v2 GET token
enforcement. The process vmnet TX path detours
MMDS ARP requests, pure empty-payload MMDS SYN packets, pure empty-payload MMDS
ACK-only packets that acknowledge bangbang's deterministic SYN-ACK, pure
empty-payload MMDS FIN close packets, unsupported empty-payload MMDS control
packets, guest-sent MMDS packets carrying RST, and non-empty candidates that
acknowledge bangbang's deterministic SYN-ACK only for interfaces listed in the
MMDS config, buffers split request headers in bounded per-interface process
state only when each fragment starts at the next expected TCP sequence number,
rejects non-contiguous buffered fragments before appending guest bytes,
synthesizes response frames from deterministic ARP context, deterministic
SYN-ACK context, minimal FIN close context, minimal RST context, or the first
TCP request fragment context, retains those frames in bounded per-interface
queues, delivers queued frames through the matching virtio-net RX source with a
bounded post-TX RX retry, and does not forward handled request payloads to
vmnet. This still does
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
- network, MMDS guest access, snapshot, or full vsock containment; the current network interface
  configuration path validates and stores configuration strings, and internal
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
  bounded post-TX retry, plus an internal provider that can select prebuilt
  adapters by configured interface ID and an internal `host_dev_name` mapping for
  `vmnet:host`, `vmnet:shared`, and `vmnet:bridged:<interface>`. The current
  model stores at most 16 configured network interfaces. Startup revalidates
  that limit before opening vmnet resources, opens them only when configured
  interfaces use the supported names, keeps no-network startup on a no-op TX
  sink plus empty RX source, and still lacks a macOS sandbox, host resource
  broker, connectivity policy, and live vmnet integration proof. The current
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
  guest-initiated connections can forward bounded guest `VSOCK_OP_RW` payload
  bytes to retained host streams, keep a bounded four-packet per-connection
  guest-to-host retry queue for partial or would-block nonblocking writes, and
  retry pending bytes on later notification dispatch before accepting more guest
  `RW` data for the same connection. Queue overflow or terminal write failures
  drop the retained stream before queuing a guest-visible reset. Established
  host-initiated and guest-initiated connections can retain a bounded
  four-packet per-connection backlog of host `VSOCK_OP_RW` payloads and deliver
  one queued payload at a time into validated guest RX buffers,
  guest `VSOCK_OP_RST` packets drop matching retained host-initiated or
  guest-initiated streams without queuing guest-visible RX output, and
  full guest `VSOCK_OP_SHUTDOWN` packets drop matching retained streams before
  queuing a guest-visible reset. Valid guest `VSOCK_OP_CREDIT_UPDATE` packets
  for established retained streams are consumed without queuing a reset, and
  valid guest `VSOCK_OP_CREDIT_REQUEST` packets queue zero-payload guest-visible
  `VSOCK_OP_CREDIT_UPDATE` headers on the existing RX path. Host-stream EOF or
  read failures drop the retained stream before queuing a guest-visible reset.
  Startup preparation
  creates a nonblocking host Unix listener at `uds_path` and cleans it up only
  while the path still matches the created socket inode. It can accept event
  queue notifications as no-op
  dispatch metadata, but it still does not route CIDs beyond current host/guest
  checks, dispatch real event payloads, track graceful half-close state, or
  implement full virtio-vsock credit accounting.
- complete production logging or metrics policy
- public run-loop control or public serial streaming policy

These are future security design and implementation topics. PRs that add new
host-facing resources should update this document and include resource-specific
validation, redaction, cleanup, concurrency, and multi-process tests where
practical.
