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
- `/metrics` opens the output path during pre-boot configuration and keeps a
  per-process metrics sink.
- `/logger` opens `log_path` during pre-boot configuration when that field is
  present and keeps a per-process logger sink.
- `scripts/run-hvf-tests.sh` creates temporary files for signed HVF integration
  tests and removes them when the wrapper exits normally.
- `scripts/run-guest-boot-tests.sh` creates temporary files for the signed
  guest boot integration test and removes them when the wrapper exits normally.
  Its generated guest initrd is cached under `.tmp/guest-artifacts` by default.

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
tests must run through signed wrappers, currently `scripts/run-hvf-tests.sh`
and `scripts/run-guest-boot-tests.sh`. These wrappers build the HVF test binary,
create a temporary entitlement plist, ad-hoc sign a copy, and run the signed
copy with one test thread. CI may use `--allow-unsupported` only to compile and
sign on runners that cannot execute HVF; local HVF verification should fail
when HVF is unavailable.

## Guest Data Exposure

The guest is untrusted. vCPU execution, guest memory contents, virtqueue
descriptor chains, MMIO accesses, block requests, and future device inputs must
be validated before they affect host resources.
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
- network, vsock, MMDS, or snapshot containment; the current internal network
  interface model validates configuration strings only and does not open host
  networking resources
- complete production logging or metrics policy
- public run-loop control or public serial streaming policy

These are future security design and implementation topics. PRs that add new
host-facing resources should update this document and include resource-specific
validation, redaction, cleanup, concurrency, and multi-process tests where
practical.
