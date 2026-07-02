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
  `VSOCK_OP_RW` payload bytes to the retained host stream. Would-block, short,
  zero-byte, or failed host writes for non-empty payloads drop the retained
  stream and queue a guest-visible `VSOCK_OP_RST` instead of buffering
  unbounded data. Established host-initiated and guest-initiated connections
  can also retain a bounded per-connection backlog of host
  `VSOCK_OP_RW` payloads and deliver one queued payload at a time
  into validated guest RX buffers. Guest `VSOCK_OP_RST` packets drop matching
  retained host-initiated or guest-initiated streams without queuing guest-visible
  RX output. Full guest `VSOCK_OP_SHUTDOWN` packets drop matching retained
  streams, release the host local port when applicable, and queue a
  guest-visible `VSOCK_OP_RST`. Host-stream EOF or read failures drop the
  retained stream and queue a guest-visible `VSOCK_OP_RST`.
  Startup also binds a nonblocking host Unix listener at `uds_path`,
  records the listener socket device and inode, and removes the path on normal
  shutdown only when it still refers to the socket created by this process. It
  does not route CIDs beyond current host/guest checks, dispatch real event
  payloads, track graceful half-close state, retry buffered guest-to-host RW
  writes, or implement full virtio-vsock credit accounting yet.
- `/metrics` opens the output path during pre-boot configuration and keeps a
  per-process metrics sink.
- `/logger` opens `log_path` during pre-boot configuration when that field is
  present and keeps a per-process logger sink.
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
configuration, and are intentionally omitted from `GET /vm/config`. Future full
logging and metrics support must avoid leaking host paths or unexpected guest
data in error messages.

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
- network, MMDS, snapshot, or full vsock containment; the current network interface
  configuration path validates and stores configuration strings, and internal
  virtio-net notification dispatch can parse guest TX descriptor metadata and
  pass validated TX frame payloads to injected packet I/O selected per configured
  interface, and can copy injected RX packet bytes into validated guest RX
  buffers through the same boundary. On macOS, the process crate has internal
  vmnet descriptor, lifecycle, start owner, concrete system start/stop backend,
  packet descriptor, single-packet system read/write backend boundaries, a
  cleanup-owning packet backend for retaining stop-on-drop ownership while
  delegating packet I/O, and an internal virtio-net adapter that can move
  packets between vmnet and the runtime packet traits for future host
  networking, plus an internal provider that can select prebuilt adapters by
  configured interface ID and an internal `host_dev_name` mapping for
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
  bytes to retained host streams, and failed or incomplete writes drop the
  retained stream before queuing a guest-visible reset. Established
  host-initiated and guest-initiated connections can retain a bounded
  per-connection backlog of host `VSOCK_OP_RW` payloads and deliver one queued
  payload at a time into validated guest RX buffers,
  guest `VSOCK_OP_RST` packets drop matching retained host-initiated or
  guest-initiated streams without queuing guest-visible RX output, and
  full guest `VSOCK_OP_SHUTDOWN` packets drop matching retained streams before
  queuing a guest-visible reset. Host-stream EOF or read failures drop the
  retained stream before queuing a guest-visible reset. Startup preparation creates a nonblocking host Unix
  listener at `uds_path` and cleans it up only while the path still matches the
  created socket inode. It can accept event queue notifications as no-op
  dispatch metadata, but it still does not route CIDs beyond current host/guest
  checks, dispatch real event payloads, track graceful half-close state, retry
  buffered guest-to-host RW writes, or implement full virtio-vsock credit accounting
- complete production logging or metrics policy
- public run-loop control or public serial streaming policy

These are future security design and implementation topics. PRs that add new
host-facing resources should update this document and include resource-specific
validation, redaction, cleanup, concurrency, and multi-process tests where
practical.
