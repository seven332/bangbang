# Firecracker Compatibility Scope

This document describes bangbang's intended Firecracker compatibility scope. It
is a planning reference for future API, VMM, and backend work; it does not mean
the current scaffold implements all listed API behavior.

The current repository defines crate boundaries, endpoint names, a minimal
HTTP-over-Unix-socket API server for `GET /`, `GET /version`,
`GET /vm/config`, `GET /machine-config`, pre-boot `PUT /machine-config`
configuration storage, pre-boot `PUT /boot-source` configuration storage, pre-boot `PUT /drives/{drive_id}`
configuration storage, pre-boot `PUT /network-interfaces/{iface_id}` configuration storage, pre-boot `PUT /metrics` output configuration, pre-boot `PUT /logger` output configuration, process-owned `PUT /actions` startup with an internal boot run-loop worker across bounded step windows, runtime `FlushMetrics` with a minimal per-process metrics sink, a backend-neutral VM trait, a minimal VMM action/data model with internal
`InstanceStart` preflight, transactional startup executor, and successful-start state transition helpers, backend-neutral guest
physical address and aarch64 DRAM layout/access primitives, arm64 boot
placement helpers, internal boot-source validation and arm64 kernel/initrd
payload loading, an internal Firecracker-shaped drive configuration validation
model, a Firecracker-shaped network interface configuration storage and
validation model, internal virtio-net config-space, activation, TX frame parser, RX buffer parser, and prepared device-resource skeletons, a host-file backing access layer, internal configured block-device
preparation and MMIO registration helpers, an internal virtio-block
config-space capacity model, an internal virtio-block request parser, single-request
executor, queue dispatcher, MMIO queue-state bridge, resettable activation
state, notification/interrupt-status dispatch helper, an internal TX-only
serial MMIO output device model with shared bounded capture support, and a minimal
Hypervisor.framework VM create/destroy wrapper, a current-thread HVF vCPU
create/destroy wrapper, typed HVF exit surface with MMIO data-abort decoding,
registry resolution, vCPU exit classification, single resolved HVF MMIO
exit dispatch/completion through runtime handlers, explicit runner-thread MMIO
handling commands, narrow vCPU register wrappers, internal macOS 15+ HVF GIC v3 boot metadata without MSI/ITS, HVF SPI interrupt-line allocation and signaling, minimal internal
arm64 FDT generation with optional serial and virtio-mmio device-node descriptors and guest-memory writes, anonymous guest memory allocation
for validated runtime layouts, HVF guest memory map/unmap ownership and
controlled mapped-memory access for allocated regions, an internal MMIO region ownership registry and operation/data
model plus handler dispatch boundary, an internal TX-only serial MMIO output
handler that captures transmit bytes without global state, an internal virtio-mmio register/access
decoder, feature/status, queue, queue notification, and interrupt
status/acknowledgement register state, a composed runtime handler that routes
common register accesses through those state models and exposes drained queue
notifications, delegated device-configuration accesses, and a `DRIVER_OK`
activation hook with reset callback, plus virtqueue descriptor-chain validator,
available-ring read model, used-ring write model, and internal virtio-block
queue construction, drain, resettable active queue ownership, and active queue
notification dispatch helper with virtio-mmio queue interrupt-status updates
for future device handlers, internal boot-resource assembly from stored VM
configuration with optional serial and block MMIO registration plus boot-runtime
block notification dispatch with per-device metadata, an internal backend-neutral
interrupt line/status/trigger model, single-vCPU arm64 HVF
boot-register setup, internal HVF single-vCPU arm64 boot-session preparation
with a runner-compatible shared MMIO dispatcher, controlled mapped guest-memory
access, one-step runner-thread MMIO handling, a run-cancellation boundary, a
virtual-timer-mask control boundary, a bounded internal boot-session run-loop
pump, owned internal boot-session handle, process-level owned startup-session
wiring with optional serial capture and boot run-loop supervision across bounded step windows with retained internal worker status, boot block queue interrupt signaling,
virtual timer PPI assertion, per-controller metrics and logger output state, and an initial process startup argument model.
There is no broader API request body model beyond the initial boot-source,
drive configuration, machine-configuration, metrics, logger, and actions bodies, public guest
execution beyond internal startup execution across bounded step windows, public run-loop control, complete interrupt
delivery, including timer EOI/deactivation-driven unmasking,
HVF runner-loop notification scheduling, public serial output streaming,
serial/backend interrupt wiring beyond the internal boot block notification
and retained serial capture paths,
device-backed feature negotiation, indirect descriptor support,
device-backed runner-loop MMIO scheduling, complete device emulation,
full Firecracker metrics counters, periodic metrics flushing, full logger integration,
multi-vCPU setup, full PSCI behavior, or successful actions beyond owned `InstanceStart`
startup with an internal boot run loop across bounded step windows and runtime
`FlushMetrics` yet. Public drive configuration is
recorded as pre-boot VM state and applied only during startup preparation; separate internal runtime helpers can
prepare owned block-device resources from that stored configuration and
register prepared resources in an internal MMIO dispatcher, but public
block-device attachment, boot selection, and runtime hotplug remain deferred.

## Firecracker Model Alignment

bangbang should follow Firecracker's process model: one `bangbang` process
manages one microVM. Future API work should keep the control plane outside the
guest execution fast path.

The macOS host security baseline is documented separately in
[macOS Host Security Model](security.md). That document records the current
socket, host-path, HVF entitlement, guest-data, and multi-process boundaries, and
also records Linux Firecracker hardening features that are not implemented by the
current macOS/HVF scaffold.

The intended public control plane is Firecracker-style HTTP over a Unix domain
socket. The implemented `GET /`, `GET /version`, `GET /vm/config`,
`GET /machine-config`, pre-boot `PUT /machine-config`, pre-boot
`PUT /boot-source`, pre-boot `PUT /drives/{drive_id}`, pre-boot
`PUT /metrics`, pre-boot `PUT /logger`, and parsed `PUT /actions` requests
already map through a minimal internal VMM action/data boundary. Validation
rejects malformed boot-source and actions requests before VMM state mutation.
Successful `InstanceStart` startup, the `Running` transition, and an internal boot run-loop worker across bounded step windows are implemented with an internal serial MMIO
console capture path and retained internal active, terminal-outcome, or error worker status. `FlushMetrics` is implemented as a runtime-only minimal JSON-line flush through per-process metrics state. `PUT /logger` is implemented as pre-boot per-process observability configuration; public run-loop control, public serial
streaming, full Firecracker metrics counters, periodic flush, and full logger integration remain deferred.

## Process Startup CLI

The current `bangbang` executable parses only the first process-lifecycle
arguments and starts the first API socket surface. It binds a Unix socket and
serves `GET /`, `GET /version`, `GET /vm/config`, `GET /machine-config`,
pre-boot `PUT /machine-config`, pre-boot `PUT /boot-source` configuration storage, and
pre-boot `PUT /drives/{drive_id}` configuration storage, pre-boot `PUT /metrics`
output configuration, pre-boot `PUT /logger` output configuration, plus process-routed `PUT /actions` startup and metrics
flush with an internal boot run-loop worker across bounded step windows or
state/configuration faults, but does not load a configuration file or provide
public run-loop control.

| Argument | Current behavior | Compatibility notes |
| --- | --- | --- |
| `--api-sock <PATH>` | binds the API Unix socket | Firecracker defaults to `/run/firecracker.socket`; bangbang defaults to `/tmp/bangbang.socket` because macOS does not normally provide `/run`. This is an intentional host-platform difference. |
| `--id <ID>` | parsed and stored | Defaults to Firecracker's `anonymous-instance`. IDs must be 1 to 64 bytes and contain only ASCII alphanumeric characters or `-`. |
| `--help`, `-h` | prints help | Help describes the current API socket scope. |
| `--version`, `-V` | prints version | `-V` is retained from the existing bangbang scaffold. |
| `--config-file`, `--no-api` | rejected | Deferred until VM configuration models and no-API startup behavior exist. |
| seccomp, logger and metrics CLI, snapshot, MMDS, boot timer, payload-size, and PCI process flags | rejected | These Firecracker options are Linux-specific, observability-related, or tied to later capability work. The API-level `PUT /metrics` and `PUT /logger` subsets are supported separately; CLI observability flags remain deferred. |

bangbang intentionally treats `--id` alphanumeric characters as ASCII only.
This is stricter than Firecracker `v1.16.0`'s Rust validator, which accepts
Unicode alphanumeric characters.

Only the Firecracker-style `--arg value` form is supported for the initial
startup arguments. The `--arg=value` form is rejected until a separate
compatibility decision expands the CLI parser.

CLI values are untrusted input. Current validation rejects invalid IDs, empty
socket paths, and socket paths containing control characters. API startup also
fails if the configured socket path already exists. Socket cleanup removes the
socket inode created by the current process during normal shutdown and handled
`SIGINT`/`SIGTERM` shutdown; uncatchable forced termination such as `SIGKILL`
can still leave a stale socket path behind. The API socket is unauthenticated;
filesystem permissions on the socket path and parent directory are the current
access-control boundary. Operators should use a private socket directory or a
restrictive umask on multi-user hosts. Process CLI parsing stays outside the
future VM/vCPU fast path and should add only trivial startup overhead. Error and
status output avoid echoing path-like CLI values.

### Process Exit Status

The current executable uses a small process exit status contract:

| Exit status | Current meaning | Compatibility notes |
| --- | --- | --- |
| `0` | Help or version completed successfully, or the API server exited without error, including handled `SIGINT`/`SIGTERM` shutdown. | Matches Firecracker's success status. |
| `153` | Startup argument parsing or validation failed. | Matches Firecracker's `ArgParsing` exit code. |
| `1` | API socket bind or accept failure. | Used for non-argument process failures before more specific Firecracker-compatible process errors exist. Per-connection read/write errors do not terminate the API server. |

Firecracker also defines bad-configuration and signal-specific exit codes.
bangbang does not expose those until the corresponding configuration loading,
signal handling, API server, or VM runtime behavior exists.

## Compatibility Baseline

bangbang's first Firecracker compatibility baseline is the upstream
`firecracker-microvm/firecracker` `v1.16.0` release tag:

- tag: `v1.16.0`
- commit: `d83d72b710361a10294480131377b1b00b163af8`

A release tag is the compatibility reference because it represents a published
Firecracker interface. Development branch commits can still inform
implementation research, but they must not redefine bangbang's compatibility
target without an explicit baseline update. A standalone pinned commit is
precise, but it should be tied to a release tag for this project so the baseline
is both reproducible and recognizable.

Use these upstream files and documents as sources of truth when comparing
Firecracker behavior:

- `src/firecracker/swagger/firecracker.yaml` for the published HTTP API surface
- `src/firecracker/src/api_server/parsed_request.rs` for method and path routing
- `src/vmm/src/rpc_interface.rs` for VMM actions and state-dependent behavior
- `docs/device-api.md` for endpoint, device, input, and output dependencies
- `docs/design.md` for process model, thread model, and threat-containment
  expectations

Unreviewed upstream drift in API routing, VMM actions, device behavior, or
published docs must not implicitly change bangbang's target. Future baseline
updates must be explicit pull requests that update this documentation and
describe API, state, documentation, security, performance, and test impact
before changing this reference.

## Support Level Vocabulary

The current scaffold implements `GET /`, `GET /version`, `GET /vm/config`,
`GET /machine-config`, pre-boot `PUT /machine-config` configuration storage, pre-boot
`PUT /boot-source`, `PUT /drives/{drive_id}`, `PUT /metrics`, and `PUT /logger` configuration
storage over HTTP on a Unix domain socket, plus runtime `FlushMetrics` after
successful startup. The support levels below describe compatibility targets for
future API work:

- supported target: planned for the first boot-oriented API implementation
- planned later: expected to be compatible later, but outside the first tier
- deferred: blocked on a separate capability, device, or backend design
- intentionally unsupported: not part of the current macOS/HVF target without a
  later compatibility policy change

For request fields, rejected means the future API should fail the request once
JSON models exist. Optional means the field may be omitted; for Firecracker
fields represented as nullable optional values, explicit `null` should be
treated like omission unless a row says otherwise. Ignored means accepted with
no effect. No supported target field is intentionally ignored. Deferred request
fields should be rejected until their capability is implemented. Some fields
have value-specific policy so Firecracker's explicit default values remain
accepted while feature-enabling values stay out of the first tier. Unknown JSON
fields should be rejected to match Firecracker `v1.16.0` request models that
deny unknown fields.

## Endpoint Compatibility Matrix

The first planned compatibility tier is the smallest boot-oriented API surface.
Rows marked as implemented describe current behavior; the rest describe planned
compatibility targets.

| Method | Endpoint | Support level | Scope notes |
| --- | --- | --- | --- |
| `GET` | `/` | supported target; implemented | Describe the microVM instance. The state becomes `Running` after successful startup with the internal boot run-loop worker across bounded step windows retained. |
| `GET` | `/version` | supported target; implemented | Report the VMM version with a Firecracker-shaped body. |
| `GET` | `/vm/config` | supported target; implemented | Returns the accumulated supported VM configuration subset. Unsupported sections are omitted until their models exist. |
| `GET` | `/machine-config` | supported target; implemented | Returns the stored/default machine configuration. |
| `PUT` | `/machine-config` | supported target; implemented | Stores the first vCPU and memory configuration subset before boot; values are applied during startup preparation. |
| `PUT` | `/boot-source` | supported target; implemented | Stores guest kernel path, optional initrd path, and optional boot arguments before boot; host files are opened during startup preparation. |
| `PUT` | `/drives/{drive_id}` | supported target; implemented | Stores initial virtio-block device configuration before boot; backing files are opened during startup preparation. |
| `PUT` | `/metrics` | supported target; minimal subset implemented | Stores process metrics output before boot, opens the configured file/FIFO path with nonblocking output semantics, and omits metrics from `GET /vm/config` because it is not guest configuration. Duplicate initialization returns a fault. |
| `PUT` | `/actions` | supported target; internal startup execution and minimal metrics flush implemented | Parses `InstanceStart` and `FlushMetrics` request bodies and routes them through the process VMM owner. `InstanceStart` validates stored boot-source and state preflight, prepares an owned HVF boot session with an internal serial MMIO console on success, starts a process-owned internal boot run-loop worker across bounded step windows, and commits `Running` only after the worker handle is retained. `FlushMetrics` is rejected before startup and returns `204 No Content` after startup; configured metrics output receives one minimal JSON line, while unconfigured metrics flush is a no-op success. |
| `PUT` | `/actions` with `SendCtrlAltDel` | intentionally unsupported; parser rejected | Firecracker gates this action on x86 keyboard behavior; the first bangbang target is Apple Silicon. |
| `PUT` | `/logger` | supported target; minimal subset implemented | Stores process logger configuration before boot, opens `log_path` with nonblocking output semantics when provided, accepts optional Firecracker-shaped level/show/module fields, and omits logger state from `GET /vm/config` because it is not guest configuration. Full internal log routing remains deferred. |
| `PATCH` | `/machine-config` | deferred | Partial updates belong with later state and validation rules. |
| `PUT` | `/cpu-config` | deferred | Needs HVF CPU feature design with VM and boot work in #8 and #10. |
| `PUT` | `/network-interfaces/{iface_id}` | supported target; configuration storage implemented | Stores initial virtio-net configuration before boot without opening host networking resources. Internal virtio-net config, inactive device-resource preparation, TX frame parsing, and RX buffer parsing exist, but packet backend selection, packet queue execution, MMIO/FDT registration, and device attachment remain tied to #14. |
| `PUT` | `/vsock` | deferred | Tied to virtio vsock work in #15. |
| `GET`, `PUT`, `PATCH` | `/mmds` | deferred | Tied to MMDS work in #16. |
| `PUT` | `/mmds/config` | deferred | Tied to MMDS work in #16. |
| `PUT` | `/snapshot/create`, `/snapshot/load` | deferred | Tied to snapshot and restore work in #19. |
| `GET`, `PUT`, `PATCH` | `/balloon` | deferred | Requires balloon device and runtime update design. |
| `GET`, `PATCH` | `/balloon/statistics` | deferred | Requires balloon statistics design. |
| `PATCH` | `/balloon/hinting/start`, `/balloon/hinting/stop` | deferred | Requires balloon free-page hinting design. |
| `GET` | `/balloon/hinting/status` | deferred | Requires balloon free-page hinting design. |
| `PUT`, `PATCH` | `/pmem/{id}` | deferred | Requires a separate pmem device design. |
| `PUT` | `/entropy`, `/serial` | deferred | Requires separate device and macOS/HVF design work. |
| `GET`, `PUT`, `PATCH` | `/hotplug/memory` | deferred | Requires memory hotplug device and runtime update design. |
| `PATCH` | `/vm` | deferred | Pause and resume state rules belong with #29 and VMM action work. |
| `PATCH` | `/drives/{drive_id}`, `/network-interfaces/{iface_id}` | deferred | Hotplug and runtime update behavior belongs with the relevant device issues. |
| `DELETE` | `/drives/{drive_id}`, `/pmem/{id}`, `/network-interfaces/{iface_id}` | deferred | Firecracker routes these hot-unplug requests in `parsed_request.rs`, but they are not in the `v1.16.0` swagger surface; support needs an explicit compatibility decision. |

## Initial Field Handling Policy

Field policy is based on Firecracker `v1.16.0` schemas and parser behavior. The
future API should use these tables as golden/API test input once JSON models
exist.

| Endpoint | Field | Handling | Notes |
| --- | --- | --- | --- |
| `PUT /boot-source` | `kernel_image_path` | required | Host path to the kernel image. The API/VMM storage path rejects empty paths without file IO; future startup validation must check access without leaking sensitive path details. |
| `PUT /boot-source` | `initrd_path` | optional | Host path to an initrd. The API/VMM storage path rejects explicitly empty initrd paths without file IO; future startup validation follows the kernel path policy. |
| `PUT /boot-source` | `boot_args` | optional | Firecracker uses its default kernel command line when omitted. The API/VMM storage path validates the 2048-byte aarch64 limit including the trailing NUL byte and rejects embedded NUL bytes. |
| `PUT /boot-source` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /machine-config` | `vcpu_count` | required | Firecracker bounds this to `1..=32`; HVF work must also account for host CPU and thread limits. |
| `PUT /machine-config` | `mem_size_mib` | required | Drives guest memory allocation and mapping; later work must cover bounds and startup performance. |
| `PUT /machine-config` | `smt` | optional when `false`; rejected when `true` | Firecracker defaults this to `false` and rejects `true` on aarch64; the initial HVF target should accept explicit no-SMT config without exposing SMT control. |
| `PUT /machine-config` | `cpu_template` | optional when omitted, `null`, or `None`; deferred for non-`None` templates | Explicit `None` matches Firecracker's deprecated default; non-default CPU templates need a separate HVF compatibility design. |
| `PUT /machine-config` | `track_dirty_pages` | optional when `false`; deferred when `true` | Explicit `false` matches Firecracker's default; enabling dirty tracking belongs with snapshot support. |
| `PUT /machine-config` | `huge_pages` | optional when `None`; rejected for `2M` | Explicit `None` matches Firecracker's default; Linux hugetlbfs does not directly apply to the macOS target. |
| `PUT /machine-config` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /drives/{drive_id}` | path `drive_id` | required | The API parser captures this value, and the internal model validates it as nonempty alphanumeric or `_`, matching Firecracker's `checked_id` rule. |
| `PUT /drives/{drive_id}` | body `drive_id` | required | The API parser rejects requests where this does not match the path `drive_id`. |
| `PUT /drives/{drive_id}` | `is_root_device` | required | Identifies whether this drive is the boot device. |
| `PUT /drives/{drive_id}` | `path_on_host` | required | The API/VMM path records this value only after rejecting empty paths; it does not open or stat the path yet. Future validation must cover access, file type, and path redaction in errors. |
| `PUT /drives/{drive_id}` | `is_read_only` | optional | The internal model defaults omitted virtio-block drives to read-write. |
| `PUT /drives/{drive_id}` | `partuuid` | optional | Only meaningful for root-device boot selection. |
| `PUT /drives/{drive_id}` | `cache_type` | optional when `Unsafe`; deferred when `Writeback` | The internal model accepts omitted/default `Unsafe` and rejects `Writeback` as unsupported. |
| `PUT /drives/{drive_id}` | `rate_limiter` | optional when absent or `null`; deferred when configured | The internal model rejects configured rate limiters; non-null rate limiting is tied to future block I/O performance work in #13. |
| `PUT /drives/{drive_id}` | `io_engine` | optional when `Sync`; rejected when `Async` | The internal model accepts omitted/default `Sync` and rejects `Async`; `Async` is tied to Linux io_uring and does not directly map to the first macOS target. |
| `PUT /drives/{drive_id}` | `socket` | optional when absent or `null`; deferred when set | The internal model rejects configured sockets; vhost-user-block is outside the first tier. |
| `PUT /drives/{drive_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /network-interfaces/{iface_id}` | path `iface_id` | required | The API parser captures this value, and the internal model validates it as nonempty alphanumeric or `_`, matching Firecracker's `checked_id` rule. |
| `PUT /network-interfaces/{iface_id}` | body `iface_id` | required | The API parser rejects requests where this does not match the path `iface_id`. |
| `PUT /network-interfaces/{iface_id}` | `host_dev_name` | required | The API/VMM path records this value only after rejecting empty values; it does not open, stat, or otherwise touch host networking resources. macOS packet backend selection remains deferred. |
| `PUT /network-interfaces/{iface_id}` | `guest_mac` | optional | The internal model accepts six colon-separated two-hex-digit octets, normalizes display to lowercase hex, and rejects duplicate configured MAC addresses across different interface IDs. |
| `PUT /network-interfaces/{iface_id}` | `mtu` | deferred when configured | The internal model rejects configured MTU values until virtio-net feature negotiation and backend behavior exist. |
| `PUT /network-interfaces/{iface_id}` | `rx_rate_limiter`, `tx_rate_limiter` | deferred when configured | The internal model rejects configured network rate limiters until virtio-net rate limiting behavior exists. |
| `PUT /network-interfaces/{iface_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /metrics` | `metrics_path` | required | Host path to the metrics output file or FIFO. The runtime opens it as per-process observability state and redacts path details from API-facing open errors. |
| `PUT /metrics` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /logger` | `log_path` | optional | Host path to the logger output file or FIFO. When present, the runtime opens it as per-process observability state and redacts path details from API-facing open errors. When omitted, the existing sink is left unchanged. |
| `PUT /logger` | `level` | optional | Case-insensitive values `Off`, `Trace`, `Debug`, `Info`, `Warn`, `Warning`, and `Error` are accepted. `Warning` is normalized to `Warn`. |
| `PUT /logger` | `show_level` | optional | Stored as logger formatting configuration for future log integration. |
| `PUT /logger` | `show_log_origin` | optional | Stored as logger formatting configuration for future log integration. |
| `PUT /logger` | `module` | optional | Stored as logger filtering configuration for future log integration. |
| `PUT /logger` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /actions` | `action_type=InstanceStart` | process-routed; internal startup execution across bounded step windows implemented | Validates stored boot-source and state preflight first, then attempts owned HVF boot-session preparation with an internal serial MMIO console and starts the process-owned internal boot run-loop worker across bounded step windows. Success returns `204 No Content` and commits `Running`; preparation or worker-start failures return a fault without mutating state. Public run-loop control and public serial streaming remain deferred. |
| `PUT /actions` | `action_type=FlushMetrics` | runtime-only; minimal execution implemented | Rejected before startup. After startup, returns `204 No Content`; if `/metrics` was configured, appends one minimal JSON line, otherwise succeeds without writing. Full Firecracker counters, periodic flush, and full logger integration remain deferred. |
| `PUT /actions` | `action_type=SendCtrlAltDel` | intentionally unsupported; parser rejected | Firecracker gates this on x86 keyboard behavior; the first target is Apple Silicon. |
| `PUT /actions` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |

The API and VMM state path implement the `PUT /machine-config` field policy
above. Valid pre-boot `PUT /machine-config` requests replace the stored full
machine configuration and return `204 No Content`; `GET /machine-config`
returns the stored or default configuration. The stored values are applied
during `InstanceStart` startup.

`GET /vm/config` returns the accumulated supported VM configuration subset
without side effects. It includes the stored/default `machine-config`, includes
`boot-source` only after it is configured, and always includes a `drives` array
for configured virtio-block drives plus a `network-interfaces` array for stored
network interface configs. Unsupported Firecracker sections such as MMDS, vsock,
balloon, snapshots, and hotplug are omitted until their configuration models
exist. Metrics and logger output configuration are also omitted because they are
process observability state rather than guest configuration.

The API and VMM state path implement the `PUT /boot-source` field policy above.
Valid pre-boot requests replace the stored boot-source configuration and return
`204 No Content`; invalid requests fail without mutating existing state or
echoing host path and boot-argument values. The public API path stores path
values at configuration time; `InstanceStart` opens kernel and initrd host paths,
loads payloads, builds an FDT, configures vCPU registers, and retains the owned
HVF boot run-loop worker only after preparation succeeds.

The API and VMM state path implement the `PUT /actions` field policy above for
`InstanceStart` and `FlushMetrics` and rejects malformed bodies before VMM state
mutation. Parsed actions now route to explicit runtime VMM actions.
`InstanceStart` validates that a boot source exists in `Not started` state before
startup preparation is attempted; when preflight succeeds, the process VMM owner
prepares an owned HVF boot session, starts a process-owned internal boot
run-loop worker across bounded step windows, and marks the instance `Running`
only after that worker handle is retained.
The API and VMM state path implement the `PUT /metrics` field policy above as a
pre-boot-only per-process output sink. Duplicate initialization fails without
replacing the original sink. `FlushMetrics` is runtime-only: it fails before
startup, succeeds without writing when the sink is unconfigured, and writes one
minimal JSON line when `/metrics` configured an output path. Public run-loop
control, guest boot output, public runner loop scheduling, full Firecracker
metrics counters, periodic metrics flush, full logger integration, and CLI
observability flags remain deferred.
The API and VMM state path implement the `PUT /logger` field policy above as a
pre-boot-only per-process observability configuration. Repeated pre-boot
requests update only the fields they provide. Runtime requests fail without
opening a new output path. The configured logger sink is not wired into the
process logging backend yet.
`SendCtrlAltDel` is rejected at parse time for the first aarch64 target.

Future implementation PRs should derive unit or golden tests from these tables.
User documentation should keep the same support and field-status vocabulary when
API behavior ships. Security review must cover host paths, socket-like fields,
device identifiers, and error messages. Performance review must cover boot path
setup, memory size, and block device I/O when those surfaces are implemented.

## Internal Virtio-Block Request And Queue Dispatch

The runtime crate can parse internal virtio-block request descriptor chains from
guest memory for future device handlers. It reads the 16-byte header, classifies
`IN`, `OUT`, `FLUSH`, `GET_ID`, and unsupported request types, validates the
required data/status descriptor direction and length rules, checks 512-byte
sector alignment and capacity bounds for `IN`/`OUT`, and checks the 20-byte
minimum `GET_ID` buffer.

The runtime crate can also execute one already-parsed request against
`GuestMemory` and `BlockFileBacking`. `IN` reads from the host backing into
guest memory, `OUT` writes guest memory into the host backing, `FLUSH` syncs the
host backing, `GET_ID` writes a fixed 20-byte device ID, and unsupported request
types write the virtio unsupported status. Completion metadata records the head
descriptor index and the bytes written to guest memory, including the status
byte when status writing succeeds.

The runtime crate can drain an internal virtio-block queue by popping available
descriptor chains, parsing and executing each request, publishing used-ring
completion elements, and returning queue-interrupt intent when at least one
completion is published. Parse failures after a valid descriptor head publish a
zero-length used element, matching Firecracker's discard shape for malformed
block requests.
The queue can be built from a ready `VirtioMmioQueueState` by reusing the
guest-selected descriptor table, driver ring, device ring, and queue size. The
builder rejects not-ready queues and wraps invalid ring metadata before guest
memory is touched.

The runtime crate also has an internal virtio-block device state that owns the
host-file backing, fixed 20-byte device ID, and optional active queue. It can
activate queue 0 from a `DRIVER_OK` virtio-mmio activation snapshot, reject
duplicate activation without replacing the existing queue, and clear the active
queue on virtio-mmio reset.

The composed runtime handler can drain recorded virtio-mmio queue notifications
and dispatch queue 0 through the active internal block queue. The dispatch
summary preserves the drained notification list, reports queue-interrupt
intent, marks the virtio-mmio queue interrupt status bit when completed work
needs an interrupt, and surfaces queue-dispatch errors with partial completion
metadata.
Boot runtime resources can also dispatch pending notifications for registered
boot block devices by using the same MMIO handler instances that guest register
writes mutated, while returning drive, region, and interrupt-line metadata for
backend interrupt signaling.

This is not runtime public `/drives` behavior and does not wire block queue
notifications into public continuous HVF runner loops or support indirect
descriptors yet. Startup preparation can register initial block MMIO devices,
and internal HVF boot sessions can consume this dispatch metadata and signal
needed block SPI interrupts.

## Guest Memory Address Space

The runtime crate models the backend-neutral guest physical address space used
by later allocation, HVF mapping, boot, and device work. The current model
contains guest physical addresses, checked RAM ranges, ordered non-overlapping
layouts, the first aarch64 DRAM layout and boot placement helpers, safe byte
slice access by guest address, and owned anonymous host memory allocation for
validated page-aligned layouts.

The aarch64 layout helper follows Firecracker's `v1.16.0` ARM layout shape:

- guest RAM starts at `0x8000_0000` (2 GiB)
- the architectural DRAM maximum is 1022 GiB
- RAM crossing the 256-512 GiB MMIO64 gap is split around that gap
- zero requested memory is rejected by the layout helper
- requests above the architectural maximum are capped inside the layout model

The allocation model creates one anonymous read/write private host memory
mapping for each validated guest RAM range and releases the mappings with
runtime ownership cleanup. It preserves each guest range with its host mapping
for HVF map/unmap work. It does not use Firecracker's `vm-memory` crate; future
device-memory, dirty-tracking, snapshot, or file-backed-memory work should
evaluate the right abstraction from its concrete requirements.

Guest memory byte access validates the whole requested guest address range
before copying. Overflow, unmapped holes, and the aarch64 MMIO64 gap fail
without partial copies, while zero-length reads and writes are no-ops. This
gives later boot-loading code a safe runtime-owned path for copying kernel,
initrd, command line, and FDT bytes without exposing additional raw host-memory
pointers.

The first arm64 placement helpers match Firecracker's published aarch64 layout
shape: system memory occupies the first 2 MiB of DRAM, the kernel load address
starts at `0x8020_0000`, the command-line size constant is 2048 bytes, the FDT
window is 2 MiB at the end of the first DRAM range when there is room, and
initrd placement is page-aligned immediately before the FDT window when it fits.
A zero-byte initrd resolves to the FDT address, matching Firecracker's helper
behavior.

## Internal Boot Source and Payload Loading

The runtime crate has an internal, Firecracker-shaped boot-source model with a
required kernel image path, optional initrd path, and optional boot arguments.
The public `PUT /boot-source` API stores a separate validated configuration
shape; it is not wired to payload loading or startup yet. Later startup work can
convert the stored configuration into this internal loader boundary when it is
ready to open files and load payloads.

When boot arguments are omitted, the runtime uses Firecracker's default aarch64
kernel command line. Custom boot arguments follow Firecracker's `linux-loader`
command-line parsing shape: leading and trailing boot/init-argument whitespace
is trimmed, the first unquoted ` -- ` separates init args, and the normalized
bytes must fit in the 2048-byte aarch64 command-line capacity including the
trailing NUL byte. Embedded NUL bytes and init args without boot args are
rejected. The validated command-line text is now available to the internal FDT
builder as the `chosen.bootargs` property, but this remains internal and is not
wired to public API behavior yet.

The internal loader supports the arm64 Linux `Image` header shape used by
Firecracker's aarch64 boot path. It validates the Image magic, text offset, and
legacy zero-size image behavior, then copies the complete kernel file into
guest memory at `kernel_load_address + text_offset`. The kernel range must be
fully backed by guest memory and must not overlap the reserved FDT address.

An explicitly configured initrd must be a non-empty regular file. It is placed
with the aarch64 initrd helper immediately before the FDT reservation, must be
fully backed by guest memory, and must not overlap the loaded kernel range.
Host path and file errors stay structured so future API code can redact paths
from user-facing messages.

The loader intentionally uses bangbang's safe `GuestMemory::write_slice` API and
does not expose new raw host-memory pointers. Direct `linux-loader`/`vm-memory`
integration is deferred until the project decides whether to add a narrow
adapter or adopt `vm-memory` more broadly.

## Internal Drive Configuration

The API crate has a strict Firecracker-shaped `PUT /drives/{drive_id}` request
parser and body model. It accepts the documented drive fields, rejects unknown
fields, rejects malformed or incomplete JSON bodies, rejects extra path
segments, and rejects path/body `drive_id` mismatches without echoing host paths.
The running API server converts parsed drive requests into a VMM action; valid
pre-boot requests are recorded as VM configuration state and return `204 No
Content`.

The runtime crate has an internal, Firecracker-shaped drive configuration model
for the initial virtio-block subset. It validates path and body `drive_id`
values as nonempty alphanumeric strings with `_`, requires the two IDs to
match, rejects an empty `path_on_host` without opening or statting host files,
and normalizes omitted `is_read_only` to read-write.

The internal model accepts omitted/default `cache_type=Unsafe` and
`io_engine=Sync`, and rejects `Writeback`, `Async`, configured rate limiters,
and configured sockets as unsupported. Displayed errors avoid echoing
`path_on_host` so future API code can preserve host path redaction.

The runtime crate can also open the normalized `path_on_host` as a regular
host file, preserve the configured read-only mode, report byte length, and
perform bounded positioned reads/writes and flushes for internal virtio-block
request execution. It rejects non-regular backing paths before data I/O and
rejects read-only writes before mutating the file. Backing errors also avoid
echoing `path_on_host`. This host-file opening path is internal and not invoked
by public drive configuration yet.

## Internal Network Interface Configuration

The API and runtime crates implement pre-boot, Firecracker-shaped network
interface configuration storage for future virtio-net work. The API parser
accepts `PUT /network-interfaces/{iface_id}`, rejects path/body ID mismatches
and unknown fields, and forwards the supported request shape through the VMM
action boundary. The runtime validates path and body `iface_id` values as
nonempty alphanumeric strings with `_`, requires the two IDs to match, requires
a nonempty `host_dev_name`, accepts optional `guest_mac` values only when they
are six colon-separated two-hex-digit octets, replaces existing entries with
the same `iface_id`, and rejects duplicate configured guest MAC addresses across
different interface IDs. Displayed validation errors avoid echoing invalid IDs,
host device names, and MAC strings.

The internal model rejects configured `mtu`, `rx_rate_limiter`, and
`tx_rate_limiter` fields as unsupported. It does not open host networking
resources, choose a macOS packet backend, or attach a virtio-net device yet.
Stored network interface configs are returned from `GET /vm/config` in the
`network-interfaces` array.

The runtime crate also has the first backend-neutral virtio-net config-space,
activation, TX frame parser, RX buffer parser, and prepared device-resource
skeletons. They define the
Firecracker-shaped virtio network device id, RX/TX queue indexes, two queue
sizes, the guest-MAC feature bit, a `VirtioMmioDeviceConfigHandler` for reading
a configured guest MAC through the existing virtio-mmio register handler, a
`DRIVER_OK` activation handler that validates and retains ready RX/TX queue
metadata, an internal TX frame parser for the 12-byte virtio-net header plus
guest-readable payload segments, and an internal RX buffer parser for
guest-writable receive buffer segments with the current 1526-byte Firecracker
non-merged-RX minimum. Preparation can build ordered owned resources from stored
configs, preserving `iface_id`, `host_dev_name`, guest-MAC config space, and an
inactive `VirtioNetworkDevice` without opening host networking resources. These
skeletons do not register network MMIO regions during boot, execute RX/TX
queues, signal interrupts, advertise MTU, choose a host packet backend, write
packets into guest RX buffers, or move packets.

The runtime crate can prepare owned internal block-device resources from a
validated list of stored drive configs. Preparation opens each backing file,
derives the virtio-block config space, builds an inactive `VirtioBlockDevice`,
uses the drive ID as the fixed 20-byte virtio device ID with zero padding or
truncation, and preserves the configured drive order. If a later config fails
to prepare, the source drive configs remain unchanged and the error identifies
the drive ID without echoing `path_on_host`.

The runtime crate can also consume prepared block-device resources into a fresh
internal `MmioDispatcher`. This assigns deterministic 4 KiB virtio-mmio device
windows and region IDs from an explicit layout, registers one composed
virtio-mmio block handler per prepared device, and returns read-only
registration metadata. Invalid address strides, duplicate region-id strides,
address or region-range overflows, region-id overflows, or dispatcher
registration failures do not expose a partially registered returned bundle.

The runtime crate can derive an internal virtio-block configuration space from
the backing length. It reports capacity as full 512-byte sectors, matching
Firecracker's truncation of non-sector-aligned tails, exposes the virtio block
device id and one 256-entry queue shape, always advertises
`VIRTIO_F_VERSION_1` and `VIRTIO_RING_F_EVENT_IDX`, and advertises
`VIRTIO_BLK_F_RO` only for read-only drives.
The config handler supports bounded read-only capacity reads through the
existing virtio-mmio device-configuration path and rejects config writes.

The runtime model is wired to successful pre-boot `PUT /drives/{drive_id}` VMM
configuration storage. Public `InstanceStart` startup can call block-device
preparation, MMIO registration, and FDT device description for initial
configured drives, and the internal boot run loop across bounded step windows can dispatch active
block notifications. It does not select a root block device for boot, provide an
public runner control, implement rate limiting, support
vhost-user-block sockets, or use an async I/O engine. Internal HVF boot sessions
can signal block SPI interrupts after boot-runtime block notification dispatch.

## Internal arm64 FDT Generation

The runtime crate can build a minimal Firecracker-shaped arm64 FDT using the
same `vm-fdt` writer crate that Firecracker uses. The generated tree currently
contains root properties, CPU data, memory, chosen, timer, PSCI, GIC nodes, and
an optional serial device node plus optional sorted virtio-mmio device nodes
from caller-supplied descriptors. It intentionally omits RTC, PCI, vmgenid,
vmclock, and other device nodes until the corresponding emulation paths exist.
Because the FDT advertises PSCI with `method = "hvc"`, the HVF backend decodes
arm64 HVC exception exits and handles `HVC #0` as a minimal PSCI 0.2 responder
for early single-vCPU boot probing. The responder returns `PSCI_VERSION`,
reports feature support only for the implemented minimal calls, returns
`MIGRATE_INFO_TYPE` as the PSCI value for a trusted OS that is MP-capable or
not present, where migration is not required, and writes `NOT_SUPPORTED` to X0
for other PSCI calls or HVC immediates.
Early boot also traps the guest's `OSDLR_EL1` and `OSLAR_EL1` OS lock
system-register accesses through the AArch64 SYS64 exception class (`0x18`),
not through SMC/SMCCC. The HVF runner handles only those observed
debug-register accesses with KVM-like RAZ/WI semantics: reads return zero,
writes are ignored, and other trapped system registers still fail closed.

The memory node excludes the first 2 MiB system area from the first DRAM range
and preserves later DRAM ranges from the runtime layout, but direct FDT
configuration must match the aarch64 DRAM layout helper for its total guest RAM
size. Sparse layouts, ranges overlapping the aarch64 MMIO64 gap, and total RAM
beyond the aarch64 maximum are rejected. The chosen node carries boot arguments
and optional initrd start/end properties from loaded boot-source metadata.
Firecracker's `rng-seed` and `linux,pci-probe-only` chosen properties are
deferred until guest startup and device work need them.
Direct FDT configuration still validates that `bootargs` fits in the 2048-byte
aarch64 command-line capacity including the trailing NUL byte and contains no
embedded NUL bytes. The GIC node consumes backend-neutral distributor and
redistributor metadata, advertises `arm,gic-v3`, and does not emit an ITS/MSI
child while the HVF metadata has no MSI support. The FDT builder rejects empty
or oversized CPU sets, duplicate CPU `reg` values, initrd ranges outside
guest-advertised memory or overlapping the reserved FDT window, and GIC MMIO
regions that are invalid, overlap each other, or overlap guest RAM. It also
rejects unexpected GIC compatibility strings and PPI collisions between the GIC
maintenance interrupt and timer interrupts.

Optional virtio-mmio nodes follow Firecracker's aarch64 FDT shape: node names
are `virtio_mmio@{base:x}`, each node has `dma-coherent`,
`compatible = "virtio,mmio"`, `reg = [base, size]`,
`interrupts = [SPI, line - 32, edge-rising]`, and `interrupt-parent` pointing
at the GIC phandle. Direct FDT configuration validates that each device range is
non-empty, does not overflow, does not overlap guest RAM, GIC distributor or
redistributor ranges, or another virtio-mmio range, and that each interrupt
line is an actual SPI INTID before encoding Firecracker's legacy GSI-style FDT
cell. The internal boot-resource assembly path composes block MMIO
registrations and caller-provided interrupt lines into these descriptors, while
HVF startup wiring allocates matching block SPI lines and can signal them after
boot-runtime notification dispatch.

An optional serial node follows Firecracker's aarch64 `ns16550a` shape: the
builder emits the shared `apb-pclk` fixed-clock node and a `uart@{base:x}` node
with `compatible = "ns16550a"`, `reg = [base, size]`,
`clocks = <apb-pclk>`, `clock-names = "apb_pclk"`, and
`interrupts = [SPI, line - 32, edge-rising]`. The serial node inherits the root
`interrupt-parent`, matching Firecracker's serial-specific node shape. Direct
FDT configuration validates that the serial region is non-empty, does not
overflow, does not overlap guest RAM, GIC distributor or redistributor ranges,
or any virtio-mmio range, and that the serial interrupt line is an SPI INTID
before encoding it. The internal boot-resource assembly path can register one
optional serial MMIO handler and pass matching serial FDT metadata from the
same placement and interrupt line.

FDT writes first reject mismatches between the layout used to describe guest RAM
and the allocated guest memory object. FDT bytes are then built before guest
memory is touched, checked against the reserved 2 MiB FDT window, and copied
with `GuestMemory::write_slice` at the aarch64 FDT address. Oversized,
overflowing, or unbacked writes fail before a partial copy. Memory layouts whose
memory `reg` property alone cannot fit in the FDT window are rejected before FDT
construction. The write result records the FDT guest address and byte size for
future boot-register setup.

## Internal Boot Resource Assembly

The runtime crate can assemble internal arm64 boot resources from stored VMM
controller configuration and caller-provided backend boot metadata. This path
requires a configured boot source, applies `mem_size_mib` to the aarch64 DRAM
layout, allocates guest memory, loads the arm64 Linux `Image` and optional
initrd, prepares configured block devices, registers their virtio-mmio regions
in a fresh internal `MmioDispatcher`, optionally registers one TX-only serial
MMIO handler in the same dispatcher, pairs block registrations with supplied
SPI interrupt lines, and writes the arm64 FDT with matching serial and
virtio-mmio metadata.

The assembled bundle owns the guest memory, loaded boot metadata, FDT write
metadata, MMIO dispatcher, optional serial metadata/output sink, and block/FDT
device metadata needed by later HVF startup wiring. It fails with typed errors
for missing boot source, memory size
overflow or a memory size above the arm64 architectural maximum,
layout/allocation failure, boot-source loading failure, block-device preparation
failure, serial or block MMIO registration failure, interrupt-line count
mismatch, or FDT write failure.

The assembled bundle is used by owned HVF startup preparation. HVF owns the
mapped guest memory while runtime metadata, the MMIO dispatcher, optional serial
metadata, and block metadata stay available to the retained session. bangbang
now starts an internal boot run-loop worker across bounded step windows after successful startup and retains internal active, terminal-outcome, or error worker status, but
does not yet provide public run-loop control, signal backend
interrupts outside the internal boot block notification path, or prove guest
boot with an integration test.

The runtime crate also contains an internal MMIO region registry, operation
model, and handler dispatch boundary for future real devices. It reuses
`GuestMemoryRange`'s end-exclusive semantics instead of Firecracker's
inclusive-end `BusRange` representation.
Region registration rejects zero-sized or overflowing ranges and accepts
adjacent non-overlapping ranges. Lookups validate that the whole access range is
owned by one region before returning the region owner and offset; accesses that
hit a hole, overflow, or cross a region boundary are rejected. A resolved access
can be wrapped as a read or write operation with bounded 1-, 2-, 4-, or 8-byte
data, and write construction rejects data whose length does not match the
resolved access size. A runtime dispatcher can route those checked operations
to registered internal handlers by region owner. HVF-specific helpers can now
dispatch one resolved HVF MMIO exit through those runtime handlers and complete
successful read results back into the trapped guest GPR. The runner can also
perform one `hv_vcpu_run` step, resolve a resulting MMIO exit against a shared
dispatcher, and dispatch or complete it on the vCPU-owning thread. This is
still not continuous run-loop policy, complete device emulation, or interrupt
delivery.

The runtime crate also contains a TX-only `ns16550a`-shaped serial MMIO output
device model. It supports one-byte transmit-register writes, divisor-latch
writes when DLAB is set, deterministic status/configuration reads, and explicit
errors for unsupported widths, invalid offsets, read-only writes, and output
sink failures. Output is captured through an injected sink instead of global
state, and the provided in-memory sink has an explicit byte limit, so
independent device instances do not share guest console data or grow host
memory without a caller-chosen bound. A shared bounded sink lets the internal
boot-resource assembly path register a serial handler while retaining an output
handle for later startup wiring or tests. The internal arm64 FDT builder can
describe the same serial MMIO descriptor as a Firecracker-shaped `uart@...`
node, but this is still internal groundwork only: the public `/serial`
endpoint, kernel `earlycon` wiring, runner-loop console capture, serial
input/RX, rate limiting, metrics, and host file output configuration are still
deferred. The first internal guest boot integration test uses this bounded capture
path directly without adding public serial streaming.

The runtime crate can decode checked MMIO operations into typed virtio-mmio
generic-register or device-configuration accesses for the Firecracker `v1.16.0`
transport window. The generic register decoder accepts only exact 4-byte reads
or writes at the Firecracker-supported common register offsets and rejects
unsupported offsets, unsupported widths, cross-register accesses, and accesses
outside the 4 KiB virtio-mmio device window before future device-specific state
can be mutated. A backend-neutral common-register state model can return
Firecracker-shaped identity values, expose selected 32-bit device feature
pages, accept selected driver feature pages only in the pre-`FEATURES_OK`
driver state, and enforce the cumulative VirtIO status transition sequence
plus reset-on-zero behavior. A separate backend-neutral queue-register model
tracks selected queue state, validates queue sizes, records queue ready state,
and composes descriptor, driver, and device ring address halves with the
alignment required by the virtqueue model. A queue-notification register model
records valid `QueueNotify` writes after `DRIVER_OK`, rejects notifications for
unsupported queues or invalid device states, and can drain the coalesced pending
queue indexes for future device handlers. A separate backend-neutral interrupt
register model can expose pending queue/configuration interrupt bits through
`InterruptStatus` and clear selected bits through `InterruptAck` after
`DRIVER_OK`, while rejecting unknown acknowledgement bits at the checked runtime
boundary. A composed backend-neutral register handler routes checked common
register reads and writes through those state models, implements the runtime
MMIO handler boundary, and exposes the notification drain without exposing
mutable nested state. Device-configuration accesses are classified by offset and
length and can be delegated through a backend-neutral config handler; config
writes are delegated only after the `DRIVER` status bit is set and while
`FAILED` and `DEVICE_NEEDS_RESET` are clear. The composed handler can invoke a
backend-neutral activation hook when `DRIVER_OK` is accepted and call its
reset hook when the virtio-mmio status is reset to zero or the handler is
explicitly reset. Activation failure marks the device as needing reset, but
concrete device activation effects, device config layouts, config generation
policy, and general runner-loop device-backed notification dispatch are still
deferred. Activated queue metadata can now feed the internal virtio-block queue
builder, and boot runtime resources can dispatch registered block-device queue
notifications against caller-supplied guest memory. Internal HVF boot sessions
can signal needed block SPI interrupts from those dispatch summaries, but HVF
runner-loop calls remain deferred. The
virtqueue model can publish one used-ring completion element with validated
layout, mapped-memory checks, wrapping, and release ordering, but batching,
event-index notification suppression, and device-backed completion loops are
still deferred.

The runtime crate also contains backend-neutral interrupt signaling groundwork.
It can validate nonzero guest interrupt lines, represent queue and
configuration pending-status bits, acknowledge selected pending bits, and let a
device-facing trigger record pending state before delegating backend signaling
to an injected sink. The HVF crate can allocate deterministic guest interrupt
lines from the validated GIC SPI range, signal validated SPI levels through
`hv_gic_set_spi`, and set or clear validated GIC PPI pending bits through
redistributor pending registers on the vCPU-owning thread. Internal HVF boot
sessions use the SPI signal path for block queue interrupts after boot-runtime
notification dispatch. This follows Firecracker's separation between
device-facing interrupt triggers and KVM-specific irqfd/GSI routing, but it is
not yet device interrupt masking, timer EOI policy, runner-loop interrupt
dispatch, or guest-visible device delivery.

The HVF backend can decode candidate MMIO accesses from arm64 data-abort
exception exits and decode trapped AArch64 SYS64 system-register exits. The
MMIO decoder converts supported ESR and IPA metadata into a checked access
range, direction, width, register number, and read-extension metadata while the
raw exit snapshot still preserves FAR. Unsupported exception classes, missing
instruction-syndrome metadata, table-walk aborts, cache-maintenance aborts, and
overflowing access ranges fail closed before runtime dispatch or later HVF
completion can use them. Decoded accesses can also be resolved against the
runtime MMIO registry to identify the owning region, offset, and preserved HVF
access metadata. Whole vCPU exits can be classified into resolved MMIO, SYS64,
HVC, virtual-timer, canceled, or unknown events while preserving typed decode
and bus-resolution errors. A single resolved HVF MMIO exit can be converted into
a runtime read/write operation by reading the trapped guest GPR for writes,
dispatched to a runtime handler, and completed back into the
trapped guest GPR for successful reads with zero/sign extension and 32-bit or
64-bit target width handling.
Guest GPR 31 is rejected explicitly so it is not confused with HVF's PC
register. The runner uses a non-blocking dispatcher lock after a run step
returns an MMIO exception; it does not hold the dispatcher while `hv_vcpu_run`
is blocked. There is still no continuous run-loop policy, public interrupt
delivery, or real device emulation beyond the internal boot block notification
signal step.

The HVF backend can map allocated guest memory regions into an existing
Hypervisor.framework VM with read/write/execute guest RAM permissions. The
backend-owned mapping owner consumes the `GuestMemory` allocation, unmaps mapped
regions on explicit unmap, partial failure, drop, and VM destruction, and keeps
cleanup local to the backend instance. The internal HVF boot-session preparation
path maps the guest memory after runtime boot-resource assembly and releases the
mapping with VM-owned state when the session shuts down or is dropped. An owned
internal boot-session handle can keep the prepared HVF backend/session resources
as one storable value for future process startup wiring.

On macOS 15.0 or newer, the HVF backend can create a GIC v3 device after VM
creation and before vCPU creation. It dynamically resolves the macOS 15 GIC
symbols so older hosts can return structured unsupported errors instead of
failing at process load time. The backend exposes internal boot metadata for the
future FDT path: distributor and redistributor regions below the 1 GiB MMIO32
boundary, the supported SPI range, timer interrupt IDs, and the `arm,gic-v3`
compatibility shape. An internal SPI signaler validates guest interrupt lines
against that supported range before setting explicit GIC SPI levels with
`hv_gic_set_spi`. A narrow internal PPI pending primitive validates real GIC
PPI INTIDs before writing `GICR_ISPENDR0` or `GICR_ICPENDR0` through
`hv_gic_set_redistributor_reg` on the vCPU-owning thread. HVF timer INTIDs are
converted to FDT PPI cells for the runtime timer node, and MSI/ITS metadata is
intentionally absent until a later device path needs it.

This still is not public guest startup. bangbang can now write an internal FDT
payload, create an internal single-vCPU HVF arm64 boot session, read the primary
runner-owned vCPU `MPIDR_EL1` for boot metadata, allocate deterministic block
and optional serial SPI interrupt lines, map the assembled guest memory into
HVF, and configure a single primary HVF vCPU with the arm64 Linux boot register
state: PC points at the loaded kernel entry, X0 points at the FDT guest address,
X1-X3 are zero, and CPSR/PSTATE is `0x3c5`. The runner path sets deterministic
single-vCPU `MPIDR_EL1` affinity before redistributor access, performs metadata
reads and boot-register setup on the vCPU-owning thread, rejects duplicate setup,
setup during shutdown, setup while a run is in flight, and setup after a run has
started. If setup fails after partially writing registers, the runner rejects
guest runs until setup is retried successfully. The runner also exposes explicit
single-exit MMIO commands and virtual timer mask commands that run on the
vCPU-owning thread. One command dispatches an already resolved MMIO access after
a run has started, and another command starts one vCPU run, resolves a resulting
MMIO exit, and dispatches or completes it through a caller-provided shared
dispatcher. The virtual timer mask commands expose HVF's explicit mask bit after
`HV_EXIT_REASON_VTIMER_ACTIVATED`; GIC PPI pending commands can set or clear a
validated timer PPI bit on the runner thread. The internal boot-session
run-loop now handles virtual timer exits by asserting the EL1 virtual timer PPI
through that runner-thread command. Full timer delivery policy, including how to
detect EOI/deactivation and unmask the HVF virtual timer, remains future work.
These commands reject overlapping metadata reads, runs, boot-register setup,
MMIO dispatches, virtual timer mask operations, or GIC PPI pending operations.
By themselves, these commands do not yet form a continuous guest run loop. The
boot session can run one vCPU step through the runner with its per-session shared
MMIO dispatcher, so a
resulting MMIO exit is handled on the vCPU-owning thread without global state.
The boot session can also expose a cloneable cancellation-only handle for an
in-flight run step without exposing the full runner. Public `InstanceStart`
now starts a process-owned internal boot run-loop worker across bounded step windows with retained internal worker status and an owned
HVF boot session and internal serial MMIO console after successful startup. A
bounded internal
boot-session run-loop pump now composes that one-step path with boot block
notification dispatch between successful MMIO steps and virtual timer PPI
assertion after virtual timer exits. It stops explicitly on a step limit,
stop-token request, canceled run exit, unknown run exit, dispatch error, or
timer handler error. This remains internal runner-loop plumbing, not the future
public guest scheduler. An owned internal session handle preserves the same
session operations while avoiding a self-referential backend/session owner in
process-level state.
The boot session can also dispatch pending boot block queue notifications
against mapped guest memory and signal the corresponding block SPI line when
the runtime dispatch summary reports queue-interrupt intent; per-device results
preserve dispatch, lookup, and signal failures for later runner-loop policy.
Boot notification dispatch locks the shared dispatcher only while draining
runtime notifications and releases it before HVF GIC signaling.

bangbang now wires `mem_size_mib` into startup preparation, but still does not
wire device interrupts into public guest execution, emulate devices, provide
public run-loop control, power on secondary vCPUs, or implement full PSCI CPU
and system power actions. Later API and startup work still needs to decide
whether an oversized `mem_size_mib` request should be rejected before layout
construction or should preserve Firecracker's architecture-helper truncation
behavior.

## API State and Response Policy

The current scaffold implements the first HTTP API behavior for `GET /`,
`GET /version`, `GET /vm/config`, pre-boot `/machine-config` configuration
storage, pre-boot `PUT /boot-source`, `PUT /drives/{drive_id}`, and
`PUT /metrics` and `PUT /logger` configuration storage, plus
process-routed `PUT /actions` startup with a bounded internal boot run-loop
worker and runtime metrics flush handling. The
policy below is the compatibility target for future request parsing, VMM action
mapping, state validation, and golden API tests.

The implemented `GET /version` path flows through the minimal VMM action model
as `GetVmmVersion` and returns VMM version data. The implemented `GET /` path
flows through the same boundary as `GetVmInstanceInfo` and returns
Firecracker-shaped instance information. Parsed `/machine-config` requests
flow through `GetMachineConfig` and `PutMachineConfig` and read or replace
stored machine configuration state. `GET /vm/config` flows through
`GetVmConfig` and returns the supported accumulated configuration subset:
`machine-config`, `boot-source` when configured, the `drives` array, and the
`network-interfaces` array.
Observability state such as metrics and logger configuration is omitted. Unsupported top-level sections are omitted until their models exist. The implemented pre-boot drive path flows
through `PutDrive` and records validated configuration state. The implemented
pre-boot network-interface path flows through `PutNetworkInterface` and records
validated configuration state without opening host networking resources. Parsed
`/boot-source` requests flow through `PutBootSource` and replace stored
boot-source configuration state. Parsed `/metrics` requests flow through
`PutMetrics` and initialize per-process metrics output state that is not part of
guest configuration. Parsed `/logger` requests flow through `PutLogger` and
initialize or update per-process logger configuration state that is not part of
guest configuration. Parsed `/actions` requests flow through
`InstanceStart` and `FlushMetrics` VMM actions. `InstanceStart` first validates
stored boot-source and state preflight, then the process VMM owner prepares and
starts an owned HVF boot-session worker with an internal serial MMIO console
and bounded capture buffer. It marks the instance `Running` only after the
bounded internal worker handle is retained; `FlushMetrics` fails before startup,
then succeeds after startup and writes one minimal JSON line only when metrics
output was configured.

### Initial API State Model

The first API implementation should model the same broad stages as Firecracker:

- pre-boot: configuration requests are accepted and stored before guest
  execution starts
- starting: `PUT /actions` with `InstanceStart` validates the accumulated
  configuration, prepares the owned HVF startup session with internal serial
  capture, and transitions the process out of pre-boot state on success
- runtime: the microVM is running; pre-boot-only configuration requests should
  fail with a Firecracker-shaped unsupported-state error
- paused/resumed: deferred until `/vm` state update work defines pause and
  resume behavior

### Initial Operation State Matrix

| Operation | Pre-boot behavior | Runtime behavior | Notes |
| --- | --- | --- | --- |
| `GET /` | implemented; `200` JSON | implemented; `200` JSON | Response state reflects the current microVM state. |
| `GET /version` | implemented; `200` JSON | implemented; `200` JSON | Body uses Firecracker's `firecracker_version` field shape. |
| `GET /vm/config` | implemented; `200` JSON | implemented; `200` JSON | Returns the accumulated supported configuration subset. Startup applies the supported boot subset to an owned HVF session and internal boot run-loop worker across bounded step windows. |
| `GET /machine-config` | implemented; `200` JSON | supported target; `200` JSON | Returns the stored/default machine configuration. |
| `PUT /machine-config` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Pre-boot-only configuration. Stored values are applied during startup preparation. |
| `PUT /boot-source` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Records validated pre-boot config; host paths are opened during startup preparation. Host path errors must avoid leaking sensitive path details. |
| `PUT /drives/{drive_id}` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Records validated pre-boot config; startup preparation opens backing files and registers initial block MMIO devices. Runtime hotplug remains deferred. |
| `PUT /network-interfaces/{iface_id}` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Records validated pre-boot config without opening host networking resources. Internal inactive device-resource preparation, TX frame parsing, and RX buffer parsing exist, while packet backend, virtio-net packet queue execution/attachment, PATCH, and DELETE remain deferred. |
| `PUT /metrics` | implemented; `204` empty response on successful output initialization | unsupported after start; `400` `fault_message` | Metrics output is process observability state, not guest configuration. Duplicate initialization fails. |
| `PUT /logger` | implemented; `204` empty response on successful pre-boot configuration | unsupported after start; `400` `fault_message` | Logger output is process observability state, not guest configuration. Repeated pre-boot requests update provided fields; full log routing remains deferred. |
| `PUT /actions` with `InstanceStart` | process-routed; `204` after successful owned HVF startup with internal boot run-loop worker across bounded step windows or `400` preflight/preparation fault | unsupported after start; `400` `fault_message` | Commits `Running` only after the owned HVF boot-session worker with internal serial capture is retained. The worker keeps internal active, terminal-outcome, or error status; public run-loop control and public serial streaming remain deferred. |
| `PUT /actions` with `FlushMetrics` | VMM-routed; `400` unsupported-state `fault_message` | implemented; `204` empty response | Firecracker treats this as runtime-only. bangbang writes one minimal JSON line when metrics output was configured and otherwise succeeds without writing. |
| `PUT /actions` with `SendCtrlAltDel` | intentionally unsupported; parser returns `400` `fault_message` | intentionally unsupported; `400` `fault_message` | Firecracker rejects this on aarch64; bangbang's first target is Apple Silicon. |
| Non-initial endpoints from the endpoint matrix | `400` `fault_message` until their capability exists | `400` `fault_message` until their capability exists | Covers planned later and deferred endpoints; a later capability PR may define more specific state behavior. |
| Unknown endpoint or invalid method/path | `400` `fault_message` | `400` `fault_message` | Matches Firecracker's parser-level invalid path or method handling. |

### Response Policy

| Case | HTTP status | Body policy |
| --- | --- | --- |
| Successful data response | `200 OK` | JSON body with Firecracker-shaped field names. |
| Successful empty response | `204 No Content` | Empty body. |
| Invalid path, invalid method, invalid JSON, unknown field, invalid field, unsupported endpoint, or unsupported state | `400 Bad Request` | JSON object with `fault_message`. |
| Startup, configuration, or VMM action failure | `400 Bad Request` | JSON object with `fault_message`; exact strings can be refined with the implementation. |
| MMDS payload-limit failure | deferred | Firecracker uses `413 Payload Too Large`; define this with MMDS support. |

Future API work should use `fault_message` consistently where Firecracker does.
Exact message strings should be covered by golden tests once the API parser and
VMM action model exist, but this document only defines the initial status/body
shape.

The initial API implementation uses Firecracker's default `51200` byte HTTP
request payload limit. The `--http-api-max-payload-size` process argument
remains rejected until configurable payload limits are introduced explicitly.
Invalid path and method errors use the Firecracker `fault_message` body shape
but intentionally avoid echoing path-like request values.
The initial blocking API server also uses a short per-connection timeout so an
incomplete request cannot hold the single server loop indefinitely.

API request bodies, path identifiers, and host resource paths are untrusted
input. Future implementations must validate them before mutating VMM state and
redact sensitive host path details from error messages. API parsing and response
serialization must stay outside the VM and vCPU fast path; expensive startup,
memory, or device work belongs in explicit VMM actions where it can be measured
and tested.

## Non-Initial Firecracker Features

The following Firecracker features are outside the first compatibility tier.
Their eventual support level should follow the endpoint matrix:

- packet networking beyond pre-boot `network-interfaces` configuration storage
  and the internal virtio-net config-space, activation, TX frame parser, RX
  buffer parser, and prepared device-resource skeletons
- vsock
- snapshots
- MMDS
- balloon devices and balloon statistics
- pmem
- entropy device configuration
- serial customization
- full logger integration, full Firecracker metrics counters, periodic metrics
  flush, and CLI observability configuration
- memory hotplug
- pause and resume VM state updates
- PATCH and DELETE hotplug/update behavior

Non-initial features should be introduced through narrower capability work that
covers behavior, validation, documentation, security, and performance together.

## macOS and HVF Differences

Firecracker targets Linux/KVM. bangbang targets macOS with Apple's
Hypervisor.framework. Some Firecracker host mechanisms therefore need explicit
macOS design work instead of direct implementation:

- KVM-specific VM and vCPU operations need HVF equivalents rather than direct
  KVM ioctl usage.
- HVF guest RAM is mapped with a backend-owned owner that holds the anonymous
  host allocation until unmap or VM destruction. Startup can load payloads into
  that memory and run the internal boot worker across bounded step windows; public run-loop control remains deferred.
- HVF vCPU handles are thread-affine: creation, register access, run, and
  destroy operations must happen on the owning thread. The current vCPU wrapper
  covers current-thread lifecycle, typed exit surface, narrow register access,
  single resolved MMIO exit dispatch/completion, and the single primary arm64
  Linux boot-register setup. The current runner skeleton creates a vCPU on a
  dedicated thread, applies that boot-register setup on the owning thread before
  the first run, gets and sets the HVF virtual timer mask on that owning thread,
  explicitly dispatches one resolved MMIO access through a shared runtime
  dispatcher on the owning thread, runs once and handles a resulting
  MMIO exit through that dispatcher, supports one cancellable
  `hv_vcpu_run` step at a time, exposes a cancellation-only handle for that run
  step, and shuts down by canceling and joining the runner thread. The internal
  boot session can compose those pieces into a bounded run-loop pump that
  dispatches boot block notifications between successful MMIO steps and asserts
  the EL1 virtual timer PPI after virtual timer exits.
- HVF exit snapshots preserve Hypervisor.framework reasons such as canceled,
  exception, virtual timer activation, and unknown after a run wrapper marks
  exit data available. Candidate arm64 MMIO data-abort exceptions can be decoded
  into checked access metadata and resolved against the internal MMIO registry.
  Checked runtime MMIO operations can be dispatched to registered internal
  handlers. A single resolved HVF exit can be converted into a runtime MMIO
  operation, dispatched through those handlers on the current thread or through
  an explicit runner-thread command, and completed back into guest GPRs for
  successful reads. The runner and boot session can perform that path for one
  run step, and the boot session can repeat it through a bounded internal loop
  that terminates on explicit outcomes, but they do not yet provide an
  public run-loop control or translate exits into interrupt or runtime events.
- Firecracker's full paused/resumed microVM loop is not implemented yet.
  bangbang's runner is only the HVF ownership and cancellation primitive set
  needed before guest memory, interrupt, timer, and device work can build the
  real run loop.
- Device-facing interrupt triggers are backend-neutral runtime state today, and
  HVF interrupt-line support can allocate deterministic SPI lines from GIC
  metadata and set validated SPI levels through `hv_gic_set_spi`. Internal boot
  sessions can now use that path for block queue interrupts, while device
  interrupt masking, timer EOI/deactivation-driven unmasking, runner-loop
  interrupt delivery beyond the current internal block/timer paths, and public
  device wiring still need macOS-specific backend work.
- Linux seccomp, jailer, cgroups, and namespaces do not directly apply.
- Linux TAP-based networking needs a macOS-specific design.
- Snapshot and device behavior may differ when backed by HVF.

The initial compatibility scope should document these differences without
pretending they are solved.

## Validation Expectations

Every future compatibility change should choose validation appropriate to its
surface:

- unit tests for parsing, configuration, and state transitions
- golden tests for Firecracker-shaped API responses once the API exists
- real HVF-backed integration tests on macOS Apple Silicon through
  `scripts/run-integration-tests.sh`, which signs the `bangbang-hvf`
  integration tests with the `com.apple.security.hypervisor` entitlement before
  running them; the script prepares the pinned Firecracker kernel plus
  generated tiny initrd for guest boot tests and fails when the host cannot run
  HVF tests unless CI explicitly uses `--allow-unsupported` after build/sign
  validation

## Security and Performance Scope

Security review should cover host paths, Unix sockets, FFI boundaries, guest
memory, device I/O, and untrusted API or guest input as those surfaces are
introduced. Performance review should cover startup path, memory mapping, vCPU
run loops, and device I/O when those areas change.

Detailed security and performance analysis belongs with the capability work that
introduces or changes the relevant surface.
