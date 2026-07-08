# Firecracker Compatibility Scope

This document describes bangbang's intended Firecracker compatibility scope. It
is a planning reference for future API, VMM, and backend work; it does not mean
the current scaffold implements all listed API behavior.

The current repository defines crate boundaries, endpoint names, a minimal
HTTP-over-Unix-socket API server for `GET /`, `GET /version`,
`GET /vm/config`, `GET /machine-config`, pre-boot `PUT /machine-config`
configuration storage, pre-boot `PUT /boot-source` configuration storage, pre-boot `PUT /drives/{drive_id}`
configuration storage, pre-boot `PUT /network-interfaces/{iface_id}` configuration storage, pre-boot `PUT /vsock` configuration storage plus an internal virtio-vsock config-space, packet header model, TX descriptor packet parser, TX available-ring drain helper with used-ring descriptor completion, prepared device resource, host Unix socket listener owner, accepted host stream owner, bounded accepted-stream polling and retention, accepted-stream `CONNECT <PORT>` handshake reader, host local port allocator, retained host connection table model with pending host-initiated request packet headers, RX delivery and late RX retry for host request packet headers, guest `RESPONSE` acknowledgement for retained host-initiated connections, guest `RST` cleanup and guest `SHUTDOWN` partial-state/full-cleanup for retained host-initiated and guest-initiated connections, bounded guest-visible `RST` queueing for unsupported or orphan host-destined guest packets, bounded guest-initiated `uds_path_<PORT>` connection handling with guest `RESPONSE` or `RST` header delivery, guest `RW` payload forwarding to retained host streams for established host-initiated or guest-initiated connections with bounded four-packet per-connection guest-to-host retry buffering, bounded four-packet per-connection host-to-guest `RW` backlog and delivery from established retained streams into guest RX buffers, minimal guest `CREDIT_UPDATE` consumption and `CREDIT_REQUEST` responses with guest-visible `CREDIT_UPDATE` headers for established retained streams, MMIO registration helper, MMIO handler skeleton with active queue metadata retention, handler-level RX/TX notification dispatch, no-op event notification handling, startup FDT attachment, boot-runtime/HVF RX/TX notification dispatch with queue interrupt signaling, and boot-runtime/HVF no-op event notification handling, pre-boot `PUT /metrics` output configuration, pre-boot `PUT /logger` output configuration, pre-boot `PUT /serial` output configuration, process-owned `PUT /actions` startup with an internal boot run-loop worker across bounded step windows, runtime `FlushMetrics` with a minimal per-process metrics sink, a macOS-gated internal vmnet descriptor, lifecycle, start owner, concrete system start/stop backend, and packet descriptor boundary model for future host networking, a backend-neutral VM trait, a minimal VMM action/data model with internal
`InstanceStart` preflight, transactional startup executor, and successful-start state transition helpers, an internal MMDS guest ARP/TCP packet classifier, process-local packet-payload HTTP exchange, process vmnet TX detour, internal MMDS ARP and TCP response-frame synthesis, bounded ordered split-request buffering, and queued virtio-net RX delivery with bounded post-TX retry, backend-neutral guest
physical address and aarch64 DRAM layout/access primitives, arm64 boot
placement helpers, internal boot-source validation and arm64 kernel/initrd
payload loading, an internal Firecracker-shaped drive configuration validation
model, a Firecracker-shaped network interface configuration storage and
validation model, internal virtio-net config-space, activation, TX frame parser, RX buffer parser, prepared device resources, MMIO registration helpers, startup FDT attachment, and internal TX/RX notification dispatch metadata plus injected TX packet sink and RX packet source boundaries for virtio-net devices, a host-file backing access layer, internal configured block-device
preparation and MMIO registration helpers, an internal virtio-block
config-space capacity model, an internal virtio-block request parser, single-request
executor, queue dispatcher, MMIO queue-state bridge, resettable activation
state, notification/interrupt-status dispatch helper, guest-visible raw block
read validation through the signed HVF boot test, an internal TX-only
serial MMIO output device model with shared bounded capture support, and a minimal
Hypervisor.framework VM create/destroy wrapper, a current-thread HVF vCPU
create/destroy wrapper, typed HVF exit surface with MMIO data-abort decoding,
registry resolution, vCPU exit classification, single resolved HVF MMIO
exit dispatch/completion through runtime handlers, explicit runner-thread MMIO
handling commands, narrow vCPU register wrappers, internal macOS 15+ HVF GIC v3 boot metadata without MSI/ITS, HVF SPI interrupt-line allocation and signaling, minimal internal
arm64 FDT generation with optional RTC, serial, and virtio-mmio device-node descriptors and guest-memory writes, anonymous guest memory allocation
for validated runtime layouts, HVF guest memory map/unmap ownership and
controlled mapped-memory access for allocated regions, an internal MMIO region ownership registry and operation/data
model plus handler dispatch boundary, an internal TX-only serial MMIO output
handler that captures transmit bytes without global state, an internal virtio-mmio register/access
decoder, feature/status, queue, queue notification, and interrupt
status/acknowledgement register state, a composed runtime handler that routes
common register accesses through those state models and exposes drained queue
notifications, delegated device-configuration accesses, and a `DRIVER_OK`
activation hook with reset callback, plus virtqueue descriptor-chain validator,
available-ring read model with negotiated indirect descriptor support,
used-ring write model, and internal virtio-block
queue construction, drain, resettable active queue ownership, and active queue
notification dispatch helper with virtio-mmio queue interrupt-status updates
for future device handlers, internal boot-resource assembly from stored VM
configuration with optional RTC and serial plus block and network MMIO registration,
boot-runtime block and network notification dispatch with per-device metadata,
including an HVF wrapper path for injected virtio-net packet I/O, an internal
backend-neutral interrupt line/status/trigger model, single-vCPU arm64 HVF
boot-register setup, internal HVF single-vCPU arm64 boot-session preparation
with a runner-compatible shared MMIO dispatcher, controlled mapped guest-memory
access, one-step runner-thread MMIO handling, a run-cancellation boundary, a
virtual-timer-mask control boundary, a bounded internal boot-session run-loop
pump, owned internal boot-session handle, process-level owned startup-session
wiring with optional serial capture and boot run-loop supervision across bounded
step windows with retained internal worker status, process-owned virtio-net
packet-I/O provider selection with no-op fallback and vmnet-backed startup for
configured interfaces, an internal vmnet virtio-net packet I/O provider keyed by
configured interface ID, boot block, virtio-net, and virtio-vsock queue
interrupt signaling,
virtual timer PPI assertion, per-controller metrics and logger output state, and an initial process startup argument model.
There is no broader API request body model beyond the initial boot-source,
drive configuration, drive update, network-interface configuration, vsock configuration, machine-configuration, metrics, logger, serial, and actions bodies, public guest
execution beyond internal startup execution across bounded step windows, public run-loop control, complete interrupt
delivery, including timer EOI/deactivation-driven unmasking,
general HVF runner-loop notification scheduling, public serial output streaming,
serial/backend interrupt wiring beyond the internal boot block and network notification
and retained serial capture paths,
broader device-backed feature negotiation,
device-backed runner-loop MMIO scheduling, complete device emulation,
full Firecracker metrics counters, full logger integration,
multi-vCPU setup, full PSCI behavior, or successful actions beyond owned `InstanceStart`
startup with an internal boot run loop across bounded step windows and runtime
`FlushMetrics` yet. Minimal startup metrics flushing writes the first metrics
line when startup metrics are configured, and periodic metrics flushing uses the
same runtime metrics sink every 60 seconds while the VM is running. Metrics
flush failures increment the minimal Firecracker-shaped
`logger.missed_metrics_count` counter, and logger action write failures
increment `logger.missed_log_count`, for later successful metrics output. Public drive configuration is
recorded as pre-boot VM state and applied during startup preparation. Runtime
`PATCH /drives/{drive_id}` can refresh the backing file of an existing active
virtio-block device through the process-owned boot session, but public
block-device attachment, boot selection changes, and hotplug remain deferred.

## Firecracker Model Alignment

bangbang should follow Firecracker's process model: one `bangbang` process
manages one microVM. Future API work should keep the control plane outside the
guest execution fast path.

The macOS host security baseline is documented separately in
[macOS Host Security Model](security.md). That document records the current
socket, host-path, HVF entitlement, guest-data, and multi-process boundaries, and
also records Linux Firecracker hardening features that are not implemented by the
current macOS/HVF scaffold.

The concise support-status and test-layer summary is maintained in
[Firecracker Validation Matrix](firecracker-validation-matrix.md). This document
remains the detailed source for endpoint behavior, field policy, compatibility
rationale, and platform limits.

The intended public control plane is Firecracker-style HTTP over a Unix domain
socket. The implemented `GET /`, `GET /version`, `GET /vm/config`,
`GET /machine-config`, pre-boot `PUT /machine-config`, pre-boot
`PUT /boot-source`, pre-boot `PUT /drives/{drive_id}`, pre-boot
`PUT /network-interfaces/{iface_id}`, pre-boot `PUT /pmem/{id}`, pre-boot `PUT /vsock`, pre-boot
`PUT /metrics`, pre-boot `PUT /logger`, pre-boot `PUT /serial`, parsed `PUT /actions`,
pre-boot `PATCH /machine-config`, parsed `PATCH /mmds`, parsed `PATCH /vm`, and
runtime `PATCH /drives/{drive_id}` requests already map through a minimal internal VMM
action/data boundary. Validation rejects malformed boot-source, drive update,
VM state update, and actions requests before VMM state mutation.
Successful `InstanceStart` startup, the `Running` transition, and an internal boot run-loop worker across bounded step windows are implemented with configured or default internal serial MMIO
output and retained internal active, terminal-outcome, or error worker status. Process-owned API-enabled and no-api runs can exit successfully after guest PSCI `SYSTEM_OFF` or `SYSTEM_RESET` terminal outcomes, and fail the process on non-success terminal worker states. Startup CLI and config-file metrics paths write one initial minimal metrics line when the metrics sink is configured. `FlushMetrics` is implemented as a runtime-only minimal JSON-line flush through per-process metrics state, and includes a terse `boot_run_loop_status` summary when a process-owned boot worker exists plus initial Firecracker-shaped GET, core configuration PUT, MMDS PUT, selected PATCH, observability PUT, `/actions` API request counters, selected deprecated HTTP API usage, `logger.missed_metrics_count` after a previous metrics write failure, and `logger.missed_log_count` after a previous logger action write failure. API-enabled and no-api runtime loops also flush the same minimal metrics output every 60 seconds while the VM is running. `PUT /logger` is implemented as pre-boot per-process observability configuration with minimal successful `InstanceStart` and `FlushMetrics` action-event output; public run-loop control, public serial
streaming, full Firecracker metrics counters, and full logger integration remain deferred.

## Process Startup CLI

The current `bangbang` executable parses only the first process-lifecycle
arguments and starts the first API socket surface. It binds a Unix socket and
serves `GET /`, `GET /version`, `GET /vm/config`, `GET /machine-config`,
pre-boot `PUT /machine-config`, pre-boot `PUT /boot-source` configuration storage, and
pre-boot `PUT /drives/{drive_id}` configuration storage, pre-boot
`PUT /pmem/{id}` configuration storage, runtime
`PATCH /drives/{drive_id}` backing refresh, pre-boot
`PUT /network-interfaces/{iface_id}` configuration storage, pre-boot `PUT /vsock` configuration storage, pre-boot `PUT /metrics`
output configuration, pre-boot `PUT /logger` output configuration, pre-boot
`PUT /serial` output configuration, metrics and logger startup CLI configuration, plus process-routed `PUT /actions` startup and metrics
flush with an internal boot run-loop worker across bounded step windows or
state/configuration faults. The process can also read `--config-file` for the
supported startup subset, start the VM before serving the API socket, and then
keep the API socket available for runtime requests. With `--no-api`, the same
supported config-file startup path runs without publishing an API socket and
exits on handled `SIGINT`, handled `SIGTERM`, or guest PSCI `SYSTEM_OFF` or
`SYSTEM_RESET`. It does not provide public run-loop control yet.

| Argument | Current behavior | Compatibility notes |
| --- | --- | --- |
| `--api-sock <PATH>` | binds the API Unix socket | Firecracker defaults to `/run/firecracker.socket`; bangbang defaults to `/tmp/bangbang.socket` because macOS does not normally provide `/run`. This is an intentional host-platform difference. |
| `--http-api-max-payload-size <BYTES>` | configures the maximum accepted HTTP API request size | Defaults to Firecracker's `51200` byte limit. The configured value applies to API socket reads and final request parsing. Zero, malformed, and duplicate values are rejected during argument parsing. |
| `--id <ID>` | parsed and stored | Defaults to Firecracker's `anonymous-instance`. IDs must be 1 to 64 bytes and contain only ASCII alphanumeric characters or `-`. |
| `--start-time-us <MICROS>` | parsed and reported in minimal metrics | Accepts non-negative `u64` microsecond values passed by Firecracker-style launchers. When provided, startup, explicit runtime `FlushMetrics`, and periodic runtime metrics output include `api_server.process_startup_time_us` as the sampled monotonic clock minus this value, saturating at zero for future timestamps. |
| `--start-time-cpu-us <MICROS>` | parsed and reported in minimal metrics | Accepts non-negative `u64` microsecond values passed by Firecracker-style launchers. When provided, startup, explicit runtime `FlushMetrics`, and periodic runtime metrics output include `api_server.process_startup_time_cpu_us` as the sampled process CPU clock minus this value, saturating at zero for future timestamps before adding optional parent CPU time. |
| `--parent-cpu-time-us <MICROS>` | parsed and reported in minimal metrics | Accepts non-negative `u64` microsecond values passed by Firecracker-style launchers. When `--start-time-cpu-us` is also provided, startup, explicit runtime `FlushMetrics`, and periodic runtime metrics output add this value into `api_server.process_startup_time_cpu_us`; it is not serialized separately. |
| `--metrics-path <PATH>` | configures metrics output before API serving | Uses the same per-process metrics sink and redacted host-path error policy as `PUT /metrics`. A later duplicate `PUT /metrics` request fails without replacing this sink. |
| `--log-path <PATH>` | configures logger output before API serving | Uses the same per-process logger sink and redacted host-path error policy as `PUT /logger`. |
| `--level <LEVEL>` | configures logger level before API serving | Accepts the existing logger levels `Off`, `Trace`, `Debug`, `Info`, `Warn`, `Warning`, and `Error`; minimal action logs are emitted only when the configured level allows `Info`. |
| `--module <MODULE>` | filters minimal action logs | Matches the stored `PUT /logger` field and filters the current minimal action logs with Firecracker-style module-path prefix matching. Minimal action logs use module path `bangbang_runtime::vmm_action`; full internal log routing remains deferred. |
| `--show-level` | enables level prefix for minimal action logs | Writes `level=Info` before minimal `InstanceStart` and `FlushMetrics` action lines. |
| `--show-log-origin` | enables origin field for minimal action logs | Writes `origin=<file>:<line>` before minimal `InstanceStart` and `FlushMetrics` action names. Full Firecracker logger integration remains deferred. |
| `--mmds-size-limit <BYTES>` | configures the maximum serialized MMDS data-store size | When omitted, follows the effective HTTP API payload limit like Firecracker; with default HTTP settings this is `51200` bytes. Zero, malformed, and duplicate values are rejected during argument parsing. |
| `--metadata <PATH>` | initializes MMDS data before API serving or no-api readiness | Reads a readable regular UTF-8 JSON metadata file up to 1 MiB and applies it through the same runtime validation and serialized data-store limit as `PUT /mmds`. Malformed files, non-object data, oversized files, duplicate object keys, empty paths, control-character paths, and missing-value inputs fail before readiness. |
| `--config-file <PATH>` | startup implemented for supported subset | Reads a Firecracker-shaped JSON configuration from a readable regular file up to 1 MiB, applies supported sections through the same validation path as matching API requests, and starts the VM with `InstanceStart`. In API-enabled mode, the API socket is published only after successful startup. Malformed files, oversized files, duplicate object keys, unknown sections, unsupported sections, or invalid sections fail before socket publication or no-api readiness. |
| `--help`, `-h` | prints help | Help describes the current API socket scope. |
| `--version`, `-V` | prints version | `-V` is retained from the existing bangbang scaffold. |
| `--no-api` | config-file startup without API socket | Requires `--config-file`. Starts the supported config-file subset without binding or publishing the configured API socket, then waits for handled `SIGINT`, handled `SIGTERM`, or guest PSCI `SYSTEM_OFF` or `SYSTEM_RESET`. Runtime control, reboot-in-place, and error exit-code parity remain deferred. |
| seccomp, snapshot, boot timer, and PCI process flags | rejected | These Firecracker options are Linux-specific or tied to later capability work. |

Startup timing arguments are intentionally not exposed in `GET /vm/config` or
logs because they are process observability data, not guest configuration. When
metrics are configured, startup, explicit `FlushMetrics`, and periodic runtime
metrics flushes write provided values under the minimal `vmm` metrics object;
omitted timing arguments remain omitted. Parsed `GET /`, `GET /version`,
`GET /machine-config`, `GET /mmds`, `GET /balloon`,
`GET /balloon/statistics`, `GET /balloon/hinting/status`, and
`GET /hotplug/memory` API requests are counted under `get_api_requests`; parsed
core configuration PUTs, `PUT /mmds`, `PUT /mmds/config`, `PUT /metrics`,
`PUT /logger`, `PUT /serial`, `PUT /balloon`, `PUT /hotplug/memory`,
`PUT /pmem/{pmem_id}`, and `/actions` API requests are counted under
`put_api_requests`; parsed `PATCH /machine-config`, `PATCH /mmds`,
`PATCH /drives/{drive_id}`, `PATCH /network-interfaces/{iface_id}`,
`PATCH /balloon`, `PATCH /balloon/statistics`,
`PATCH /balloon/hinting/start`, `PATCH /balloon/hinting/stop`,
`PATCH /hotplug/memory`, and `PATCH /pmem/{pmem_id}` requests routed through
VMM control are counted under `patch_api_requests`.
Parsed deprecated HTTP API usage is counted under
`deprecated_api.deprecated_http_api_calls` for explicit non-null machine
`cpu_template`, MMDS V1 config, deprecated `vsock_id`, and snapshot-load
`mem_file_path` or `enable_diff_snapshots: true` request forms. Parser failures,
including malformed bodies and path/body ID mismatches, for the PUT and PATCH
endpoints above with matching Firecracker-shaped request metric fields are
counted in the same count/fail counters when the endpoint is identifiable from
the request line.
Direct config-file and startup initialization paths are not API requests and
are not included in these counters. `PATCH /vm` remains outside
`patch_api_requests` because Firecracker does not expose a matching
`PatchRequestsMetrics` field for VM state changes. The balloon API request
fields are bangbang-specific extension counters: GET, PUT, and PATCH balloon
routes report `balloon_count`, and PUT/PATCH failures also report
`balloon_fails`. Firecracker exposes balloon device metrics but no matching
balloon API request metric fields. The startup timing fields from
Firecracker's `ProcessTimeReporter` are implemented; the broader full
Firecracker metrics set remains deferred.

bangbang intentionally treats `--id` alphanumeric characters as ASCII only.
This is stricter than Firecracker `v1.16.0`'s Rust validator, which accepts
Unicode alphanumeric characters.

Supported value-taking startup arguments accept both Firecracker-style
`--arg value` and `--arg=value` forms. Value-less flags, such as `--no-api`,
`--show-level`, and `--show-log-origin`, reject attached values.

`--config-file` currently accepts the supported Firecracker-shaped sections
`machine-config`, `boot-source`, `drives`, `network-interfaces`,
`mmds-config`, `vsock`, `entropy`, `balloon`, `pmem`, `metrics`, `logger`, `serial`, and
`cpu-config`. The `cpu-config` section is parsed through the same request model
as `PUT /cpu-config`: empty/no-op custom template bodies are accepted before
startup, while non-empty custom CPU templates still fail as unsupported CPU
configuration actions. The `entropy` section is parsed through `PUT /entropy`:
empty bodies and `rate_limiter: null` are accepted, while configured rate
limiters fail before the API socket is published or before no-api readiness is
reported. The `balloon` section is parsed through `PUT /balloon` and stores the
pre-boot control-plane configuration. Startup can attach the current
guest-visible virtio-balloon MMIO/FDT shell from that stored configuration. The
runtime handler and HVF boot loop can dispatch inflate and deflate queue
notifications, publish zero-length used-ring completions, and signal the
allocated balloon interrupt line when queue completion requires it. Completed
inflate and deflate descriptors update internal inflated-page accounting after
PFN ranges are validated against mapped guest memory.
Runtime `PATCH /balloon` can update the stored target size and active
virtio-balloon `num_pages` config-space value after startup while preserving the
other stored balloon fields. `GET /balloon/statistics` can return required
target and actual fields from the current target and internal inflated-page
accounting plus guest-reported optional fields from bounded statistics queue
reports. Free-page hinting command descriptors and active-run range
descriptors are validated and recorded by runtime queue dispatch, but host
memory reclaim, periodic statistics polling, timer-driven statistics descriptor
completion, and reporting remain deferred. The `pmem` section is
parsed through
`PUT /pmem/{id}`;
valid entries store Firecracker-shaped pre-boot configuration and appear in
`GET /vm/config`. Startup opens and validates each configured pmem backing as a
non-zero regular host file, mmaps it to a 2 MiB-aligned host range, and retains
the handles and mappings. Startup also assigns deterministic non-overlapping
2 MiB-aligned guest physical ranges after the aarch64 MMIO64 gap, skipping
current guest RAM, and populates the internal virtio-pmem config-space
`start`/`size` values from those ranges. HVF startup creates the VM with the
framework-reported maximum IPA size so the post-MMIO64 pmem ranges are
addressable, copies each prepared pmem mapping into an HVF-compatible anonymous
shadow, and registers that shadow at the assigned guest physical range after
DRAM mapping; a read-only pmem config uses read-only HVF memory permissions,
and writable pmem uses read/write non-executable permissions. Writable shadows
are copied back to the backing file with positional writes and a data sync for
guest queue-driven flush requests and after clean HVF unmap; failed unmap
cleanup skips writeback because HVF may still reference the shadow memory.
Runtime also has a backend-neutral virtio-pmem identity, queue metadata,
feature-bit, 16-byte config-space foundation, and flush request completion
handling.
Startup attaches each prepared pmem device as a guest-visible virtio-mmio/FDT
node whose MMIO config-space exposes the prepared `start` and `size` values.
Known unsupported sections such as
`memory-hotplug` fail before the API socket is published or before no-api
readiness is reported.
The config-file path does not load MMDS data; use Firecracker's separate
`--metadata <PATH>` startup argument for startup MMDS data.

CLI values are untrusted input. Current validation rejects invalid IDs, empty
socket paths, and socket paths containing control characters. API startup also
fails if the configured socket path already exists. Socket cleanup removes the
socket inode created by the current process during normal shutdown and handled
`SIGINT`/`SIGTERM` shutdown; uncatchable forced termination such as `SIGKILL`
can still leave a stale socket path behind. The API socket is unauthenticated;
bangbang restricts the published socket inode to owner-only permissions, and
the parent directory remains part of the access-control boundary. Operators
should use a private socket directory on multi-user hosts. Process CLI parsing
stays outside the future VM/vCPU fast path and should add only trivial startup
overhead. Error and status output avoid echoing path-like CLI values.

### Process Exit Status

The current executable uses a small process exit status contract:

| Exit status | Current meaning | Compatibility notes |
| --- | --- | --- |
| `0` | Help or version completed successfully, the API server exited without error, no-api mode handled `SIGINT`/`SIGTERM` shutdown, or a process-owned VM exited after guest PSCI `SYSTEM_OFF` or `SYSTEM_RESET`. | Matches Firecracker's success status. |
| `153` | Startup argument parsing failed before process configuration began. | Matches Firecracker's `ArgParsing` exit code. |
| `1` | Process failure, including config-file startup, startup metrics/logger configuration, API socket bind, signal handler registration, no-api signal wait failure, API accept failure, or a process-owned boot worker non-success terminal state. | Used for non-argument process failures before more specific Firecracker-compatible process errors exist. Per-connection read/write errors do not terminate the API server. |

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
`PUT /boot-source`, `PUT /drives/{drive_id}`,
`PUT /network-interfaces/{iface_id}`, `PUT /vsock`, `PUT /entropy`, `PUT /metrics`, and `PUT /logger` configuration
storage over HTTP on a Unix domain socket, plus runtime `FlushMetrics` and
periodic runtime metrics flushes after successful startup. The support levels below describe compatibility targets for
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
| `PATCH` | `/machine-config` | supported target; implemented | Applies pre-boot partial updates to the stored machine configuration, preserving omitted fields and rejecting invalid updates without mutation. |
| `PUT` | `/boot-source` | supported target; implemented | Stores guest kernel path, optional initrd path, and optional boot arguments before boot; host files are opened during startup preparation. |
| `PUT` | `/drives/{drive_id}` | supported target; implemented | Stores initial virtio-block device configuration before boot; backing files are opened during startup preparation. |
| `PUT` | `/metrics` | supported target; minimal subset implemented | Stores process metrics output before boot, opens the configured file/FIFO path with nonblocking output semantics, and omits metrics from `GET /vm/config` because it is not guest configuration. Duplicate initialization returns a fault. Explicit runtime `FlushMetrics` and periodic runtime metrics flushes every 60 seconds while the VM is running can append minimal output with Firecracker-style `api_server` startup timing fields plus initial `GET /`, `GET /version`, `GET /machine-config`, `GET /mmds`, core configuration PUT, MMDS PUT, selected PATCH, observability PUT, `/actions` API request counters, `logger.missed_metrics_count` after a previous metrics write failure, and `logger.missed_log_count` after a previous logger action write failure. |
| `PUT` | `/actions` | supported target; internal startup execution and minimal metrics flush implemented | Parses `InstanceStart` and `FlushMetrics` request bodies and routes them through the process VMM owner. `InstanceStart` validates stored boot-source and state preflight, prepares an owned HVF boot session with the configured serial output path or the default internal serial MMIO console on success, starts a process-owned internal boot run-loop worker across bounded step windows, writes one minimal action log line when logger output is configured and the level/module filters allow it, and commits `Running` only after the worker handle is retained and any emitted action log write succeeds. `FlushMetrics` is rejected before startup and returns `204 No Content` after startup; configured metrics output receives one minimal JSON line with flush count, selected `get_api_requests`, `put_api_requests`, and `patch_api_requests` counters, `logger.missed_metrics_count` after a previous metrics write failure, `logger.missed_log_count` after a previous logger action write failure, optional Firecracker-style `api_server` startup timing fields, and optional boot-run-loop status, configured logger output receives one minimal action log line when the level/module filters allow it, and unconfigured metrics/logger output is a no-op success. The aarch64 `SendCtrlAltDel` unsupported parser path still contributes to `put_api_requests.actions_count` but not `actions_fails`, matching Firecracker's parser-entry metric placement. Periodic metrics flushes are not `/actions` requests and do not write logger action lines. |
| `PUT` | `/actions` with `SendCtrlAltDel` | intentionally unsupported; parser rejected | Firecracker gates this action on x86 keyboard behavior; the first bangbang target is Apple Silicon. The unsupported request is counted under `put_api_requests.actions_count` without incrementing `actions_fails`. |
| `PUT` | `/logger` | supported target; minimal subset implemented | Stores process logger configuration before boot, opens `log_path` with nonblocking output semantics when provided, accepts optional Firecracker-shaped level/show/module fields, and omits logger state from `GET /vm/config` because it is not guest configuration. Successful `InstanceStart` and `FlushMetrics` append deterministic `action=...` lines when the configured level allows `Info` and the optional module prefix matches `bangbang_runtime::vmm_action`; full internal log routing remains deferred. |
| `PUT` | `/serial` | supported target; output path subset implemented | Stores Firecracker-shaped pre-boot serial output configuration with optional `serial_out_path`; `{}` and `"serial_out_path": null` clear the configured public output path. `InstanceStart` opens the configured file/FIFO path with nonblocking output semantics and routes guest TX serial MMIO bytes to it. Serial input/stdin, configured `rate_limiter`, metrics, and public streaming remain deferred. |
| `PUT` | `/cpu-config` | supported target; empty/no-op subset implemented | Parses Firecracker aarch64 custom CPU template request bodies, including empty `{}`. Malformed JSON, unknown or duplicate fields, x86-only fields, invalid numeric strings, and invalid bitmap strings return the normal malformed-request fault. Empty `{}` and explicitly empty `kvm_capabilities`, `reg_modifiers`, and `vcpu_features` arrays succeed before startup as no-op configuration requests. Non-empty custom CPU templates fail as an unsupported action before startup, and all post-start requests fail as unsupported state without mutating VM configuration. Real custom CPU template storage, static template behavior, CPU vendor/model checks, and HVF CPU feature wiring remain deferred. |
| `PUT` | `/network-interfaces/{iface_id}` | supported target; configuration storage implemented | Stores up to 16 initial virtio-net configurations before boot without opening host networking resources. Startup preparation attaches configured interfaces as virtio-mmio devices in the MMIO dispatcher and guest FDT. `InstanceStart` revalidates the interface count before selecting packet I/O. If every configured interface is listed in MMDS config, startup validates supported `host_dev_name` syntax and uses process-local MMDS-only packet I/O without opening vmnet resources; otherwise it selects vmnet packet I/O only for `vmnet:host`, `vmnet:shared`, and `vmnet:bridged:<interface>` host device names, where the bridged interface suffix must be nonempty and contain no NUL bytes or ASCII control characters. Unsupported names fail startup before `Running` is committed. Internal network notification dispatch can route each configured interface through selected packet I/O, parse TX descriptors through a packet sink boundary, and copy injected RX packets into guest buffers through a packet source boundary. Runtime PATCH accepts a no-op subset for existing interfaces. Public packet movement, real runtime mutation, configured rate limiting, and DELETE remain deferred to future virtio-net update work. |
| `PUT` | `/vsock` | supported target; startup listener attachment, host stream accept, bounded accepted-stream `CONNECT` polling, host local port allocation/table ownership, pending host request packet modeling, guest `RESPONSE` acknowledgement, guest `RST` cleanup, guest `SHUTDOWN` partial-state/full-cleanup, bounded guest-visible `RST` queueing, bounded guest `REQUEST` connect handling, guest `RW` payload forwarding with bounded guest-to-host retry buffering, bounded four-packet per-connection host-to-guest `RW` backlog and delivery, minimal guest credit control packet handling, RX/TX notification dispatch, and event notification no-op handling implemented | Stores one initial virtio-vsock configuration before boot without opening host resources during the API request. Startup preparation binds a nonblocking host Unix listener at `uds_path`, keeps ownership in the internal vsock device resource, and removes the path on shutdown only when it still refers to the created socket. Startup also attaches the configured device as one virtio-mmio FDT node backed by the internal MMIO handler, which retains active RX, TX, and event queue metadata after `DRIVER_OK`. The runtime has a Firecracker-shaped packet header model, internal TX descriptor packet parser, TX available-ring drain helper that publishes zero-length TX used-ring completions, a host socket accept helper that returns one owned nonblocking stream per dispatch pass, bounded accepted-stream retention across partial handshakes and retained connection records, an accepted-stream `CONNECT <PORT>` handshake reader, host local port allocator, and host connection table model that retains accepted streams under Firecracker-shaped host-initiated connection keys and exposes a one-shot `VSOCK_OP_REQUEST` packet header for the guest-facing connection request. The RX dispatch path can select pending reset packets before guest responses and host requests, deliver packet headers into writable guest RX descriptors, and publish used-ring completions while preserving pending packets on malformed buffers or used-ring failures. Runtime dispatch also retries RX delivery when host requests, guest responses, reset packets, credit updates, or host-to-guest `RW` payloads are pending, so a host `CONNECT`, guest `REQUEST`, host stream payload, guest credit request, or guest TX reset candidate arriving after an earlier RX queue notification can still be delivered without a second guest RX notification. The handler and startup notification path can drain RX, TX, and no-op event queue notifications, acknowledge guest `RESPONSE` packets for delivered host requests by writing `OK <local_port>\n` to the retained host stream, drop matching retained host-initiated or guest-initiated connections on guest `VSOCK_OP_RST` packets without queuing guest-visible RX output, record partial guest `VSOCK_OP_SHUTDOWN` receive/send state for retained host-initiated or guest-initiated connections, suppress later data movement in the closed direction, and treat merged or full shutdown flags as cleanup while queuing a guest-visible `VSOCK_OP_RST`, consume valid guest `VSOCK_OP_CREDIT_UPDATE` packets for established retained streams without queuing a reset, respond to valid guest `VSOCK_OP_CREDIT_REQUEST` packets with zero-payload guest-visible `VSOCK_OP_CREDIT_UPDATE` headers, queue bounded zero-payload `VSOCK_OP_RST` headers for unsupported or orphan host-destined guest TX packets, attempt nonblocking connects for supported guest `VSOCK_OP_REQUEST` packets to Firecracker-shaped `${uds_path}_${PORT}` sockets, retain successful guest-initiated streams with a pending `VSOCK_OP_RESPONSE`, forward bounded `VSOCK_OP_RW` payload bytes from established host-initiated or guest-initiated connections to the retained host stream through a bounded four-packet per-connection guest-to-host retry queue for partial or would-block nonblocking writes, retain a bounded four-packet per-connection backlog of host `VSOCK_OP_RW` payloads from established host-initiated or guest-initiated streams and deliver one queued payload at a time into guest RX buffers, queue `VSOCK_OP_RST` when connect, retention, RW forwarding overflow or terminal failure, host-stream EOF, or host-stream read fails, complete queued descriptors, mark the virtio queue interrupt status pending, and signal the allocated vsock interrupt line from the HVF boot loop. Firecracker's full graceful-shutdown timeout/kill-queue behavior, full virtio-vsock credit accounting, guest CID routing beyond current host/guest checks, full event payload dispatch, runtime updates, PATCH, and DELETE remain deferred to future virtio-vsock update work. |
| `GET`, `PUT`, `PATCH` | `/mmds` | supported target; control-plane storage, runtime guest-query formatting, internal guest GET response modeling, request parsing, process-local exchange handling, response-byte serialization, process-local token authority, process-local guest token `PUT` modeling, process-local MMDS v2 GET token enforcement, internal guest ARP/TCP packet classification, process-local packet-payload HTTP exchange, process vmnet TX detouring, bounded contiguous split-request buffering, internal ARP/TCP response-frame synthesis, and signed executable guest fetch paths implemented | Stores bounded in-memory JSON object contents in the process runtime, returns stored JSON for control-plane `GET` or JSON `null` before initialization, applies RFC 7396 merge-patch semantics for `PATCH`, rejects uninitialized `PATCH`, and keeps previous data on oversized update failure. The runtime can also resolve initialized metadata by JSON-pointer path, format JSON or Firecracker-shaped IMDS text, parse process-local guest HTTP `GET` request bytes into URI/output-format/token inputs, map internal guest GET requests to process-local status/content-type/body response values, turn complete process-local guest HTTP request buffers into deterministic HTTP response bytes that preserve accepted `HTTP/1.0` or `HTTP/1.1` status-line versions, generate/validate bounded process-local opaque MMDS tokens, and model process-local guest `PUT /latest/api/token` exchanges as prerequisites for guest-visible delivery. When configured for MMDS v2, process-local guest GET requests require exactly one valid `X-metadata-token` or `X-aws-ec2-metadata-token` value generated by token PUT; missing, duplicate, unknown, or expired tokens return `401 Unauthorized`. The runtime can classify ARP requests for the configured MMDS IPv4 address and raw Ethernet/IPv4/TCP guest packet bytes addressed to that IPv4 address and TCP port 80 while rejecting malformed, truncated, fragmented, non-TCP, or non-MMDS packets, and it can identify pure empty-payload TCP SYN, ACK-only packets that acknowledge bangbang's deterministic SYN-ACK, FIN close, packets carrying guest RST, and unsupported control packets, synthesize SYN-ACK frames for SYN packets, synthesize ACK plus FIN-ACK frames for empty FIN close packets, synthesize minimal RST frames for unsupported empty controls, consume guest RST packets without response even when they also carry payload bytes, and turn non-empty candidate TCP payloads that acknowledge bangbang's deterministic SYN-ACK and do not carry unsupported SYN or FIN payload control flags into the same process-local HTTP response bytes as the guest HTTP helper. Process vmnet packet I/O now detours MMDS ARP requests, pure empty-payload MMDS SYN packets, pure empty-payload MMDS ACK-only packets that acknowledge bangbang's deterministic SYN-ACK, pure empty-payload MMDS FIN close packets, guest packets carrying RST, unsupported empty control packets, and non-empty MMDS candidate TX payloads on MMDS-configured interfaces when they acknowledge bangbang's deterministic SYN-ACK and do not carry unsupported SYN or FIN payload control flags, keeps shared process-local MMDS state visible to control-plane and packet paths, buffers split request headers only when each fragment starts at the next expected TCP sequence number, rejects non-contiguous buffered fragments without forwarding them to vmnet, synthesizes deterministic Ethernet/ARP replies, Ethernet/IPv4/TCP SYN-ACK frames, minimal Ethernet/IPv4/TCP FIN close frames, minimal Ethernet/IPv4/TCP RST frames, and Ethernet/IPv4/TCP response frames carrying generated HTTP response bytes, retains those frames in bounded per-interface queues, exposes queued frames through the matching virtio-net RX source before vmnet reads, prioritizes ARP replies before queued TCP responses, and schedules one bounded post-TX RX retry when that source reports a queued response. The signed executable HVF e2e target includes direct-rootfs scenarios that configure `vmnet:shared`, deterministic MMDS data, and MMDS v1 or MMDS v2 before startup, then have the guest fetch `/meta-data/bangbang-marker` through `169.254.169.254` and write host-observable success markers. The v2 scenario first requests `/latest/api/token` with a bounded TTL and uses the returned token header for the metadata fetch. Full ARP cache management, gratuitous ARP, ARP timeout/retry policy, broader ACK-number validation beyond the narrow ACK-only and non-empty payload SYN-ACK acknowledgement paths, full TCP stream tracking, out-of-order reassembly, retransmission policy, stateful RST policy, session timeout policy, and stricter per-interface TCP session ownership remain deferred to future guest-visible MMDS networking work. |
| `PUT` | `/mmds/config` | supported target; control-plane config storage implemented | Parses Firecracker-shaped MMDS config with required `network_interfaces`, optional `version`, optional RFC 3927 usable link-local `ipv4_address`, and optional `imds_compat`; keeps empty or whitespace-only interface IDs as malformed request bodies, but routes empty interface lists to runtime semantic validation before mutation; validates referenced interface IDs against configured network interfaces; stores config before startup; and keeps post-start requests on the normal unsupported-state policy. Broader guest-visible MMDS behavior remains deferred to future MMDS networking work. |
| `PUT` | `/snapshot/create`, `/snapshot/load` | recognized; VMM-routed unsupported | Parses Firecracker-shaped snapshot create and load request bodies, rejects malformed or invalid bodies first, and then routes valid requests through the VMM state/action policy. Snapshot create is post-boot-only and currently returns the snapshot-specific unsupported fault after startup; snapshot load is pre-boot-only and currently returns the snapshot-specific unsupported fault before startup. Real snapshot creation, loading, file formats, dirty-page tracking, host memory backends, and VM/vCPU/device-state serialization remain deferred to future snapshot support. |
| `GET` | `/balloon` | supported target; pre-boot and runtime config read implemented | Returns the stored Firecracker-shaped balloon configuration after successful `PUT /balloon`, runtime `PATCH /balloon`, or valid runtime `PATCH /balloon/statistics`; returns the balloon-specific unsupported fault when no balloon configuration exists. Runtime derives backend-neutral virtio-balloon identity, features, queues, and config space from stored config. Startup attaches the current virtio-mmio/FDT shell, and the HVF boot loop can dispatch inflate, deflate, statistics, and free-page hinting notifications with interrupt signaling. Inflate/deflate descriptors update internal inflated-page accounting, hinting command descriptors update `guest_cmd`, active-run hinting ranges are recorded, and statistics queue reports are parsed and stored for `GET /balloon/statistics`. Periodic statistics polling, timer-driven statistics descriptor completion, reporting, host reclaim, and broader host-side hinting behavior remain deferred. |
| `PUT`, `PATCH` | `/balloon` | partial; pre-boot config storage, startup shell attachment, runtime target update, and runtime stats interval update implemented | `PUT /balloon` stores Firecracker-shaped balloon configuration before boot, rejects targets larger than current guest memory without mutating prior config, appears in `GET /balloon` and `GET /vm/config`, and feeds startup attachment of a virtio-balloon MMIO/FDT shell. Pre-boot machine-config updates also reject memory sizes smaller than an already configured balloon target. Runtime `PATCH /balloon` updates the stored `amount_mib`, active `num_pages` config space, config generation, and config interrupt. Runtime `PATCH /balloon/statistics` can update a nonzero statistics polling interval to another nonzero value while preserving Firecracker's rejection of runtime statistics enable/disable transitions. Runtime queue dispatch covers inflate, deflate, configured statistics reports, and active-run hinting ranges; host reclaim, periodic statistics polling, timer-driven statistics descriptor completion, and reporting remain deferred. |
| `GET` | `/balloon/statistics` | partial; required target/actual and optional guest-reported fields implemented | Routes through the VMM state/action policy. Statistics queries are post-boot-only, require configured active balloon state, and return Firecracker-shaped required fields: `target_pages`, `actual_pages`, `target_mib`, and `actual_mib`. Target values come from the current stored balloon target, including runtime `PATCH /balloon` updates. Actual values come from active inflated-page accounting. Optional guest-reported fields are included only after a bounded statistics queue report records them. Periodic statistics polling and timer-driven descriptor completion remain deferred. |
| `PATCH` | `/balloon/statistics` | partial; runtime nonzero interval update implemented | Parses Firecracker-shaped statistics interval update request bodies and rejects malformed or invalid bodies first. Valid requests are post-boot-only, require a configured balloon, accept unchanged intervals as no-ops, and update stored plus active balloon state for nonzero-to-nonzero interval changes. Runtime `0 -> nonzero` and `nonzero -> 0` transitions are rejected without mutation, matching Firecracker's statistics state-change rule because the stats queue cannot be hot-added or removed. Periodic statistics polling and timer-driven descriptor completion remain deferred. |
| `PATCH` | `/balloon/hinting/start` | partial; minimal host command state implemented | Parses Firecracker-shaped free-page hinting start commands, including empty/default commands, rejects malformed or invalid bodies first, and then routes valid requests through the VMM state/action policy. Hinting start is post-boot-only, requires a configured balloon with `free_page_hinting: true`, preserves `acknowledge_on_stop` in backend-neutral device state, advances the host command id while skipping reserved values, updates active config space, raises a config interrupt, and returns `204 No Content`. Hinting queue command acknowledgements can update `guest_cmd`, active-run range descriptors are validated and recorded, and completed guest `STOP(0)`/`DONE(1)` commands can automatically acknowledge host `DONE(1)`; reporting and host reclaim remain deferred. |
| `PATCH` | `/balloon/hinting/stop` | partial; minimal host command state implemented | Routes through the VMM state/action policy without parsing the request body, matching Firecracker's stop-command parser behavior. Hinting stop is post-boot-only, requires a configured balloon with `free_page_hinting: true`, writes the Firecracker done command into host-owned active device state and config space, raises a config interrupt, and returns `204 No Content`. Hinting queue command acknowledgements can update `guest_cmd`, active-run range descriptors are validated and recorded, and completed guest `STOP(0)`/`DONE(1)` commands can automatically acknowledge host `DONE(1)`; reporting and host reclaim remain deferred. |
| `GET` | `/balloon/hinting/status` | partial; host and guest command status implemented | Routes through the VMM state/action policy. Hinting status is post-boot-only, requires a configured balloon with `free_page_hinting: true`, and returns Firecracker-shaped `host_cmd` and `guest_cmd` fields from active device state. Current status reports the latest start/stop host command and the latest 4-byte guest command observed on the hinting queue; `guest_cmd` remains `null` until the guest sends a command descriptor. Active-run range descriptors are validated and recorded internally but not exposed in this response; host reclaim remains deferred. |
| `PUT`, `PATCH` | `/pmem/{id}` | partial; pre-boot config storage, startup backing mapping, guest-visible MMIO/FDT attachment, HVF guest-memory registration, shadow writeback, guest-range config-space, and flush queue dispatch implemented | `PUT /pmem/{id}` stores Firecracker-shaped pmem configuration before boot, replaces prior config for the same ID, and appears in `GET /vm/config` under `pmem`. It rejects empty backing paths without opening host files during the request. `InstanceStart` opens each configured backing, validates it as a non-zero regular file, mmaps it to a 2 MiB-aligned host range, assigns a deterministic non-overlapping 2 MiB-aligned guest physical range after the aarch64 MMIO64 gap while skipping current guest RAM, creates the HVF VM with the framework-reported maximum IPA size, copies the prepared mapping into an HVF-compatible anonymous shadow, registers that shadow with HVF after DRAM using read-only or read/write non-executable permissions, copies writable shadows back to the backing file for guest queue-driven flush requests and after clean unmap, skips writeback after failed unmap cleanup, and attaches one virtio-mmio/FDT node per prepared pmem device. Runtime defines the backend-neutral virtio-pmem identity, one 256-entry queue, modern virtio feature bit, 2 MiB alignment constant, 16-byte config-space layout, and flush request completion/status handling; the attached MMIO config-space exposes the prepared `start`/`size` values. Configured rate limiters are rejected without mutating stored pmem configuration. `PATCH /pmem/{id}` remains post-boot-only unsupported, and real runtime updates, rate limiting, root-device semantics, dirty-range tracking, direct file-backed HVF mapping, and hot-unplug need dedicated device designs. |
| `PUT` | `/entropy` | supported target; configuration storage, startup attachment, and signed executable guest read validation implemented | Stores one Firecracker-shaped virtio-rng entropy configuration before boot when `rate_limiter` is omitted or `null`. `GET /vm/config` includes `"entropy": {}` after API or config-file configuration, and `InstanceStart` attaches the existing HVF virtio-rng MMIO/FDT device backed by the session-owned host OS randomness source. The signed executable HVF e2e target boots a direct-rootfs guest, checks that Linux selected `virtio_rng` as the current hardware RNG, reads non-empty data from `/dev/hwrng`, and writes a host-observable success marker. Configured rate limiters are validated and then rejected with the entropy rate-limiter unsupported fault without mutating stored entropy configuration. Post-start requests follow the pre-boot-only unsupported-state policy. Real rate limiting and entropy metrics remain deferred. |
| `GET`, `PUT`, `PATCH` | `/hotplug/memory` | recognized; VMM-routed unsupported | Parses Firecracker-shaped `PUT` and `PATCH` memory hotplug request bodies, rejects malformed or schema-invalid bodies first, and then routes valid requests through the VMM state/action policy. `PUT` is treated as pre-boot-only and reaches the memory-hotplug-specific unsupported fault before startup. `GET` and `PATCH` are treated as post-boot-only and reach the memory-hotplug-specific unsupported fault after startup. Requests made in the wrong lifecycle state return the normal unsupported-state fault. This is a control-plane compatibility baseline only: it preserves request-shape validation, lifecycle classification, memory-hotplug-specific unsupported faults, metrics accounting, and non-echoing API faults for size fields. Real virtio-mem device support, hotplug configuration storage, hotpluggable guest memory accounting, semantic config validation, runtime resize, and memory hot-unplug need a dedicated design. |
| `PATCH` | `/vm` | recognized; rejected | Parses the Firecracker-shaped VM state request with required `state` values `Paused` and `Resumed`, then routes valid requests through `Pause` or `Resume` VMM actions. Requests before startup fail as unsupported in `Not started` state, and runtime requests fail as unsupported actions without mutating VM state. Real pause/resume state transitions and public run-loop control remain deferred. |
| `PATCH` | `/drives/{drive_id}` | supported target; runtime backing refresh implemented | Parses the Firecracker-shaped block-device update request with required `drive_id`, optional `path_on_host`, and optional `rate_limiter`, then routes valid updates through `UpdateBlockDevice`. Pre-boot requests fail as post-boot-only operations. Runtime requests for an existing active drive open the replacement backing before mutating stored configuration, refresh the matching virtio-block MMIO handler, update the guest-visible block capacity/config generation, and leave the old backing and stored config intact on failure. Configured rate limiters remain unsupported. |
| `PATCH` | `/network-interfaces/{iface_id}` | no-op subset implemented | Returns unsupported-state before startup, validates the target interface after startup, and accepts omitted or `null` rate limiters as a runtime no-op. Configured rate limiters are rejected, and real network-interface mutation and hotplug behavior need dedicated device designs. |
| `DELETE` | `/drives/{drive_id}`, `/pmem/{id}`, `/network-interfaces/{iface_id}` | recognized; VMM-routed unsupported | Firecracker routes bodyless hot-unplug requests in `parsed_request.rs`, but they are not in the `v1.16.0` swagger surface. bangbang parses bodyless hot-unplug requests into one VMM action, returns the normal post-boot-only unsupported-state fault before startup, and returns the matching device-specific unsupported fault after startup. Body-bearing `DELETE` requests fail first as malformed request shape before hot-unplug routing. Real hot-unplug behavior remains deferred. |

## Initial Field Handling Policy

Field policy is based on Firecracker `v1.16.0` schemas and parser behavior. The
future API should use these tables as golden/API test input once JSON models
exist.

Firecracker-shaped rate limiter objects reject duplicate `bandwidth` or `ops`
fields and duplicate token bucket fields before VMM dispatch.

| Endpoint | Field | Handling | Notes |
| --- | --- | --- | --- |
| `PUT /boot-source` | `kernel_image_path` | required | Host path to the kernel image. The API/VMM storage path rejects empty paths without file IO; future startup validation must check access without leaking sensitive path details. |
| `PUT /boot-source` | `initrd_path` | optional | Host path to an initrd. The API/VMM storage path rejects explicitly empty initrd paths without file IO; future startup validation follows the kernel path policy. |
| `PUT /boot-source` | `boot_args` | optional | Firecracker uses its default kernel command line when omitted. The API/VMM storage path validates the 2048-byte aarch64 limit including the trailing NUL byte and rejects embedded NUL bytes. |
| `PUT /boot-source` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /machine-config` | `vcpu_count` | required | Firecracker bounds this to `1..=32`, and bangbang stores that range for API compatibility. Current HVF startup supports exactly one vCPU and rejects larger stored values during `InstanceStart` before creating an HVF VM. |
| `PUT /machine-config` | `mem_size_mib` | required | Drives guest memory allocation and mapping; accepted range is `1..=1046528` MiB for the current Apple Silicon/aarch64 target. When a pre-boot balloon target is already configured, the new memory size must be at least that target size or the previous machine config is preserved. Host free-memory preflight remains deferred. |
| `PUT /machine-config` | `smt` | optional when `false`; rejected when `true` | Firecracker defaults this to `false`; the initial HVF target accepts explicit no-SMT config and currently returns `machine smt is not supported` when SMT is enabled. |
| `PUT /machine-config` | `cpu_template` | optional when omitted, `null`, or `None`; rejected for known non-`None` templates | Explicit `null` is treated as omitted and does not count as deprecated API usage. Explicit `None` matches Firecracker's deprecated default and counts as deprecated API usage; known non-default CPU templates currently return `machine cpu_template <template> is not supported` and need a separate HVF compatibility design. |
| `PUT /machine-config` | `track_dirty_pages` | optional when `false`; rejected when `true` | Explicit `false` matches Firecracker's default; enabling dirty tracking belongs with snapshot support and currently returns `machine track_dirty_pages is not supported`. |
| `PUT /machine-config` | `huge_pages` | optional when `None`; rejected for `2M` | Explicit `None` matches Firecracker's default; Linux hugetlbfs does not directly apply to the macOS target and `2M` currently returns `machine huge_pages is not supported`. |
| `PUT /machine-config` | unknown or duplicate fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /machine-config` | `vcpu_count` | optional | When present, updates the stored vCPU count with the same `1..=32` bounds as `PUT`; omitted fields keep their current values. Current HVF startup still rejects values above one during `InstanceStart`. |
| `PATCH /machine-config` | `mem_size_mib` | optional | When present, updates the stored memory size with the same `1..=1046528` MiB target bound as `PUT`; omitted fields keep their current values. When a pre-boot balloon target is already configured, the patched memory size must be at least that target size or the previous machine config is preserved. |
| `PATCH /machine-config` | `smt` | optional when `false`; rejected when `true` | Matches the current `PUT` policy for the Apple Silicon target and currently returns `machine smt is not supported` when SMT is enabled. |
| `PATCH /machine-config` | `cpu_template` | optional when omitted or `null`; accepted when `None`; rejected for known non-`None` templates | Explicit `null` is treated as omitted and does not count as deprecated API usage. Explicit `None` is accepted and counts as deprecated API usage; known non-default CPU templates currently return `machine cpu_template <template> is not supported`. |
| `PATCH /machine-config` | `track_dirty_pages` | optional when `false`; rejected when `true` | Matches the current `PUT` dirty-tracking policy. |
| `PATCH /machine-config` | `huge_pages` | optional when `None`; rejected for `2M` | Matches the current `PUT` huge-pages policy and currently returns `machine huge_pages is not supported` for `2M`. |
| `PATCH /machine-config` | unknown or duplicate fields; empty patch | rejected | Matches Firecracker's strict request model behavior and avoids silent no-op updates. |
| `PUT /snapshot/create` | `snapshot_type` | optional; VMM-routed unsupported | Accepts Firecracker values `Full` and `Diff`, defaulting to `Full`, before routing the valid request through the VMM state/action policy. Real snapshot type behavior remains deferred. |
| `PUT /snapshot/create` | `snapshot_path` | required; VMM-routed unsupported | Parsed as a Firecracker-shaped path string before routing the valid request through the VMM state/action policy. Host file creation remains deferred. |
| `PUT /snapshot/create` | `mem_file_path` | required; VMM-routed unsupported | Parsed as a Firecracker-shaped path string before routing the valid request through the VMM state/action policy. Guest memory persistence remains deferred. |
| `PUT /snapshot/create` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /snapshot/load` | `snapshot_path` | required; VMM-routed unsupported | Parsed as a Firecracker-shaped path string before routing the valid request through the VMM state/action policy. Host file reads and restore state remain deferred. |
| `PUT /snapshot/load` | `mem_backend` | required unless deprecated `mem_file_path` is present; VMM-routed unsupported | Parsed as a strict object with `backend_path` and `backend_type`. Exactly one of `mem_backend` or `mem_file_path` must be present. Real memory backend loading remains deferred. |
| `PUT /snapshot/load` | `mem_backend.backend_type` | required when `mem_backend` is present | Accepts Firecracker values `File` and `Uffd`; invalid values are rejected before the snapshot unsupported fault. |
| `PUT /snapshot/load` | `mem_file_path` | deprecated-compatible alternative to `mem_backend`; VMM-routed unsupported | Accepted syntactically to match Firecracker's current parser. It must not be combined with `mem_backend`. Real deprecated-field handling and restore behavior remain deferred. |
| `PUT /snapshot/load` | `enable_diff_snapshots` | deprecated-compatible optional boolean | Accepted syntactically to match Firecracker's current parser before routing the valid request through the VMM state/action policy. Only `true` increments deprecated API usage metrics; `false` is accepted without incrementing the deprecated counter. |
| `PUT /snapshot/load` | `track_dirty_pages`, `resume_vm`, `clock_realtime` | optional booleans | Parsed before routing the valid request through the VMM state/action policy. Real dirty tracking, resume behavior, and architecture-specific clock restore remain deferred. |
| `PUT /snapshot/load` | `network_overrides` | optional | Parsed as Firecracker-shaped override entries with `iface_id` and `host_dev_name` before routing the valid request through the VMM state/action policy. Real network restore override behavior remains deferred. |
| `PUT /snapshot/load` | `vsock_override` | optional | Parsed as a Firecracker-shaped object with `uds_path` before routing the valid request through the VMM state/action policy. Real vsock restore override behavior remains deferred. |
| `PUT /snapshot/load` | unknown `network_overrides` or `vsock_override` fields | accepted by parser | Matches Firecracker's current nested override parser, which ignores unknown fields in these objects while preserving typed validation for required fields. |
| `PUT /snapshot/load` | unknown or duplicate top-level fields; unknown or duplicate `mem_backend` fields | rejected | Matches Firecracker's strict top-level and memory-backend request model behavior. |
| `PUT /balloon` | `amount_mib` | required; stored pre-boot | Stored as an unsigned 32-bit Firecracker-shaped target balloon size and returned by `GET /balloon` and `GET /vm/config`. Values larger than current configured guest memory fail without mutating any prior balloon config. The internal virtio-balloon foundation converts this value to 4 KiB `num_pages` with checked arithmetic and exposes it through the startup-attached config space. Runtime `PATCH /balloon` can update the same stored target and active config-space value after startup. `GET /balloon/statistics` reports this current target through the required `target_*` fields and can add optional guest-reported fields from statistics queue reports. Statistics polling and host reclaim remain deferred. |
| `PUT /balloon` | `deflate_on_oom` | required; stored pre-boot | Stored as a boolean and returned by `GET /balloon` and `GET /vm/config`. The internal foundation advertises `VIRTIO_BALLOON_F_DEFLATE_ON_OOM` only when this is enabled. Real guest OOM deflation behavior remains deferred. |
| `PUT /balloon` | `stats_polling_interval_s` | optional; stored pre-boot | Missing values follow Firecracker's parser default shape and are stored as `0`. Nonzero values add the internal statistics feature bit and queue metadata. Runtime dispatch can record bounded guest statistics queue reports, and `PATCH /balloon/statistics` can update nonzero intervals after startup without changing whether statistics are enabled. Timer-driven polling and runtime statistics enable/disable transitions remain deferred. |
| `PUT /balloon` | `free_page_hinting` | optional; stored pre-boot | Missing values follow Firecracker's parser default shape and are stored as `false`. `true` adds the internal free-page hinting feature bit and queue metadata. Runtime `PATCH /balloon/hinting/start`, `PATCH /balloon/hinting/stop`, and `GET /balloon/hinting/status` can update and report host-owned command state and 4-byte guest command acknowledgements when this is enabled. Runtime dispatch validates and records active-run range descriptors; reporting and host reclaim remain deferred. |
| `PUT /balloon` | `free_page_reporting` | optional; stored pre-boot | Missing values follow Firecracker's parser default shape and are stored as `false`. `true` adds the internal free-page reporting feature bit and queue metadata. Real free-page reporting remains deferred. |
| `PUT /balloon` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `GET /balloon/hinting/status` | response body | runtime host and guest command status implemented | Before startup this remains a state-specific unsupported action. After startup, requests without a configured balloon or with `free_page_hinting: false` return the existing balloon unsupported fault. With `free_page_hinting: true`, bangbang returns Firecracker-shaped `host_cmd` and `guest_cmd` fields from the active device state; initial state is `host_cmd: 0` and `guest_cmd: null`, start advances `host_cmd`, stop reports Firecracker's done command, and a 4-byte hinting queue descriptor updates `guest_cmd`. Active-run range descriptors are validated and recorded internally, and completed guest `STOP(0)`/`DONE(1)` commands can automatically acknowledge host `DONE(1)`; host reclaim remains deferred. |
| `PATCH /balloon` | `amount_mib` | required; runtime target update implemented | Parsed as an unsigned 32-bit Firecracker-shaped target balloon size before VMM dispatch. After startup with a configured balloon device, the value replaces the stored `amount_mib`, updates active virtio-balloon `num_pages`, increments config generation, and raises a config interrupt. Values larger than configured guest memory or not representable as 4 KiB pages fail without mutating stored config. |
| `PATCH /balloon` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /balloon/statistics` | `stats_polling_interval_s` | required; runtime nonzero interval update implemented | Parsed as an unsigned 16-bit Firecracker-shaped polling interval before VMM dispatch. After startup with a configured balloon, unchanged intervals are accepted, nonzero-to-nonzero changes update stored and active balloon state, and zero/nonzero enabled-state changes fail without mutation. Real timer-driven statistics polling remains deferred. |
| `PATCH /balloon/statistics` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /balloon/hinting/start` | body | optional when absent or empty | Missing or empty bodies use Firecracker's default hinting start command before VMM dispatch. An empty JSON array is also accepted as a default command, matching the current Firecracker Serde parser behavior. After startup with `free_page_hinting: true`, valid requests update host command state and return `204 No Content`. |
| `PATCH /balloon/hinting/start` | `acknowledge_on_stop` | optional | Missing values follow Firecracker's default `true` command shape. The current implementation preserves the value in host-owned device state for automatic host `DONE(1)` acknowledgement when the guest later sends `STOP(0)` or `DONE(1)`. Hinting queue command acknowledgements and active-run range descriptor validation/recording are implemented; host reclaim remains deferred. |
| `PATCH /balloon/hinting/start` | unknown fields | accepted by parser | Matches Firecracker's current hinting start command parser, which ignores unknown fields while preserving typed validation for `acknowledge_on_stop`. |
| `PUT /drives/{drive_id}` | path `drive_id` | required | The API parser captures this value, and the internal model validates it as nonempty alphanumeric or `_`, matching Firecracker's `checked_id` rule. |
| `PUT /drives/{drive_id}` | body `drive_id` | required | The API parser rejects requests where this does not match the path `drive_id`. |
| `PUT /drives/{drive_id}` | `is_root_device` | required | Identifies whether this drive is the boot device. |
| `PUT /drives/{drive_id}` | `path_on_host` | required | The API/VMM path records this value only after rejecting empty paths; it does not open or stat the path yet. Future validation must cover access, file type, and path redaction in errors. |
| `PUT /drives/{drive_id}` | `is_read_only` | optional | The internal model defaults omitted virtio-block drives to read-write. |
| `PUT /drives/{drive_id}` | `partuuid` | optional | Only meaningful for root-device boot selection. |
| `PUT /drives/{drive_id}` | `cache_type` | optional when `Unsafe`; supported when `Writeback` | The internal model accepts omitted/default `Unsafe` and explicit `Writeback`. `Unsafe` does not advertise `VIRTIO_BLK_F_FLUSH`; `Writeback` advertises it and routes guest flush requests through the backing flush path. |
| `PUT /drives/{drive_id}` | `rate_limiter` | optional when absent or `null`; rejected when configured | The internal model rejects configured rate limiters with `drive rate_limiter is not supported`; non-null rate limiting needs future block I/O performance work. |
| `PUT /drives/{drive_id}` | `io_engine` | optional when `Sync`; rejected when `Async` | The internal model accepts omitted/default `Sync` and rejects `Async`; `Async` is tied to Linux io_uring and does not directly map to the first macOS target. |
| `PUT /drives/{drive_id}` | `socket` | optional when absent or `null`; rejected when configured | The internal model rejects configured sockets with `drive socket is not supported`; vhost-user-block is outside the first tier. |
| `PUT /drives/{drive_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /drives/{drive_id}` | path `drive_id` | required | The API parser captures this value before building the runtime update action. |
| `PATCH /drives/{drive_id}` | body `drive_id` | required | The API parser rejects requests where this does not match the path `drive_id`. |
| `PATCH /drives/{drive_id}` | `path_on_host` | optional | When present at runtime, the process opens the replacement backing for the existing active drive before committing the stored configuration. When omitted, the existing path is retained without refreshing the active handler. Open and handler lookup failures leave the old backing and stored configuration intact. |
| `PATCH /drives/{drive_id}` | `rate_limiter` | optional when absent or `null`; rejected when configured | The parser accepts the Firecracker-shaped field name and validates configured rate-limiter shape; the runtime update model rejects configured rate limiters with `drive rate_limiter is not supported` before opening a replacement backing or mutating drive state. Real rate limiting belongs with future block update work. |
| `PATCH /drives/{drive_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /pmem/{id}` | path `id` | required | The API parser captures the path ID for path/body validation before routing valid requests through the VMM state/action policy. Invalid path IDs continue to fail as invalid path/method, and the runtime model also rejects empty IDs or IDs containing characters other than alphanumeric characters and `_`. |
| `PUT /pmem/{id}` | body `id` | required | The API parser rejects requests where this does not match the path `id`. |
| `PUT /pmem/{id}` | `path_on_host` | required; startup-mapped, range-assigned, guest-attached, HVF-registered through a shadow, and writeback-capable | Required Firecracker-shaped host backing path. The value is retained and reported in `GET /vm/config` after rejecting empty paths. Startup opens the path, validates it as a non-zero regular file, mmaps it to a 2 MiB-aligned host range, assigns matching guest physical range/config-space metadata, attaches a virtio-mmio/FDT node exposing that config-space, copies the prepared mapping into an HVF-compatible anonymous shadow, registers that shadow with HVF, writes writable shadows back for guest queue-driven flush requests and after clean unmap, and redacts path details from backing-open, mapping, range-allocation, shadow-allocation, HVF-registration, flush, or writeback errors. Path normalization remains deferred with broader virtio-pmem support. |
| `PUT /pmem/{id}` | `root_device` | optional; stored only | Missing values default to `false` and are reported in `GET /vm/config`. Real root-device conflict validation and boot semantics remain deferred with virtio-pmem support. |
| `PUT /pmem/{id}` | `read_only` | optional; startup-open, host mapping, HVF permission access, guest attachment, and writeback policy | Missing values default to `false` and are reported in `GET /vm/config`. Startup opens and maps the host backing read-only when this is `true` and read/write when this is `false`; HVF shadow registration mirrors that intent with read-only or read/write non-executable memory permissions. Read-only shadows never write back, while writable shadows write back for guest queue-driven flush requests and after clean unmap. The current guest-visible virtio-pmem attachment exposes config-space, range discovery, and flush completion behavior. |
| `PUT /pmem/{id}` | `rate_limiter` | optional when absent, `null`, or empty; rejected when configured | Empty or omitted limiter shapes do not create stored limiter state. Configured Firecracker-shaped rate limiter objects are validated and rejected without mutating stored pmem configuration. Real pmem flush rate limiting remains deferred. |
| `PUT /pmem/{id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /pmem/{id}` | path `id` | required | The API parser captures the path ID for path/body validation before routing valid requests through the VMM state/action policy. Invalid path IDs continue to fail as invalid path/method. |
| `PATCH /pmem/{id}` | body `id` | required | The API parser rejects requests where this does not match the path `id`. |
| `PATCH /pmem/{id}` | `rate_limiter` | optional; unsupported after parser validation | Missing, `null`, or empty objects are accepted. Configured Firecracker-shaped rate limiter objects are validated before VMM dispatch, but their values are not retained or echoed. Real pmem runtime rate-limiter updates remain deferred. |
| `PATCH /pmem/{id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /network-interfaces/{iface_id}` | path `iface_id` | required | The API parser captures this value, and the internal model validates it as nonempty alphanumeric or `_`, matching Firecracker's `checked_id` rule. |
| `PUT /network-interfaces/{iface_id}` | body `iface_id` | required | The API parser rejects requests where this does not match the path `iface_id`. |
| `PUT /network-interfaces/{iface_id}` | `host_dev_name` | required | The API/VMM path records this value only after rejecting empty values and enforcing the current 16-interface bangbang limit; it does not open, stat, or otherwise touch host networking resources during configuration. `InstanceStart` later accepts only `vmnet:host`, `vmnet:shared`, and `vmnet:bridged:<interface>` for vmnet packet I/O startup. |
| `PUT /network-interfaces/{iface_id}` | `guest_mac` | optional | The internal model accepts six colon-separated two-hex-digit octets, normalizes display to lowercase hex, and rejects duplicate configured MAC addresses across different interface IDs. |
| `PUT /network-interfaces/{iface_id}` | `mtu` | optional | The internal model accepts Firecracker-compatible `68..=65535` values, stores them with the interface config, advertises `VIRTIO_NET_F_MTU`, and exposes the value through virtio-net config space. Host vmnet MTU changes remain out of scope. |
| `PUT /network-interfaces/{iface_id}` | `rx_rate_limiter`, `tx_rate_limiter` | rejected when configured | The internal model rejects configured network rate limiters until virtio-net rate limiting behavior exists. |
| `PUT /network-interfaces/{iface_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /network-interfaces/{iface_id}` | path `iface_id` | required | The API parser captures this value before routing valid requests through the runtime lifecycle policy. |
| `PATCH /network-interfaces/{iface_id}` | body `iface_id` | required | The API parser rejects requests where this does not match the path `iface_id`. |
| `PATCH /network-interfaces/{iface_id}` | `rx_rate_limiter`, `tx_rate_limiter` | optional when absent or `null`; rejected when configured | Omitted or `null` rate limiters are accepted as a runtime no-op for existing interfaces after startup. Configured Firecracker-shaped rate limiter objects are parsed and then rejected before any runtime network mutation. Real rate limiting belongs with future network update work. |
| `PATCH /network-interfaces/{iface_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /vsock` | `vsock_id` | optional and deprecated | Firecracker `v1.16.0` accepts this field but treats it as deprecated. The internal model accepts it when present and rejects empty or control-character values. `GET /vm/config` omits this deprecated field. |
| `PUT /vsock` | `guest_cid` | required | Firecracker's published schema requires a 32-bit guest CID with minimum value `3`; smaller values are rejected before state mutation. |
| `PUT /vsock` | `uds_path` | required | Host Unix socket path used for startup listener preparation. The API/VMM configuration path records this value only after rejecting empty paths and control characters; it does not open, bind, connect, unlink, or create the socket during the request. Relative paths remain accepted to match Firecracker's documented `./v.sock` examples. Startup later binds the listener when `InstanceStart` prepares resources. |
| `PUT /vsock` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /metrics` | `metrics_path` | required | Host path to the metrics output file or FIFO. The runtime opens it as per-process observability state and redacts path details from API-facing open errors. |
| `PUT /metrics` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /logger` | `log_path` | optional | Host path to the logger output file or FIFO. When present, the runtime opens it as per-process observability state and redacts path details from API-facing open errors. When omitted, the existing sink is left unchanged. |
| `PUT /logger` | `level` | optional | Case-insensitive values `Off`, `Trace`, `Debug`, `Info`, `Warn`, `Warning`, and `Error` are accepted. `Warning` is normalized to `Warn`. |
| `PUT /logger` | `show_level` | optional | When true, minimal action log lines include a `level=Info` prefix. |
| `PUT /logger` | `show_log_origin` | optional | When true, minimal action log lines include an `origin=<file>:<line>` field for the runtime action callsite. |
| `PUT /logger` | `module` | optional | Filters minimal action logs with Firecracker-style module-path prefix matching. The current minimal action log module path is `bangbang_runtime::vmm_action`; non-matching filters suppress these action lines without failing the action. |
| `PUT /logger` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /serial` | `serial_out_path` | optional | Host path to the serial output file or FIFO. The runtime stores it before boot, startup opens it as per-process observability output, and API-facing open errors redact path details. Omit the field or set it to `null` to clear the configured public output path. |
| `PUT /serial` | `rate_limiter` | optional token bucket; unsupported when configured | Missing or `null` values are accepted. Firecracker-shaped token buckets with `size`, optional `one_time_burst`, and `refill_time` are parsed before returning the unsupported fault without mutating previous serial output configuration. |
| `PUT /serial` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /entropy` | `rate_limiter` | optional; unsupported when validly configured | Missing or `null` values are accepted and store the supported empty entropy configuration. Configured Firecracker-shaped rate limiter objects are validated and then rejected as `entropy rate_limiter is not supported` without mutating previous entropy configuration; request values are never echoed in faults. Real virtio-rng rate limiting remains deferred. |
| `PUT /entropy` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /hotplug/memory` | `total_size_mib` | required; unsupported after VMM routing | Required Firecracker-shaped hotpluggable-memory total size. The parser accepts syntactically valid unsigned integer values, then the API server routes the request through the pre-boot-only memory-hotplug action before returning the unsupported fault. Real virtio-mem semantic validation and storage remain deferred. |
| `PUT /hotplug/memory` | `block_size_mib` | optional; unsupported after VMM routing | Missing values use Firecracker's parser default shape. Present values must be unsigned integers. Real block-size semantic validation remains deferred with virtio-mem support. |
| `PUT /hotplug/memory` | `slot_size_mib` | optional; unsupported after VMM routing | Missing values use Firecracker's parser default shape. Present values must be unsigned integers. Real slot-size semantic validation remains deferred with virtio-mem support. |
| `PUT /hotplug/memory` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PATCH /hotplug/memory` | `requested_size_mib` | required; unsupported after VMM routing | Required Firecracker-shaped target hotpluggable-memory size. The parser accepts syntactically valid unsigned integer values, then the API server routes the request through the post-boot-only memory-hotplug action before returning the unsupported fault. Real resize behavior remains deferred. |
| `PATCH /hotplug/memory` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /actions` | `action_type=InstanceStart` | process-routed; internal startup execution across bounded step windows implemented | Validates stored boot-source and state preflight first, then attempts owned HVF boot-session preparation with configured serial output or the default internal serial MMIO console and starts the process-owned internal boot run-loop worker across bounded step windows. Success returns `204 No Content`, writes one minimal logger action line when configured and allowed by level/module filters, and commits `Running`; preparation, worker-start, or logger-output failures return a fault without mutating state. Public run-loop control and public serial streaming remain deferred. |
| `PUT /actions` | `action_type=FlushMetrics` | runtime-only; minimal execution implemented | Rejected before startup. After startup, returns `204 No Content`; if metrics output was configured, appends one minimal JSON line with `metrics_flush_count`, the current selected API request counters, `logger.missed_metrics_count` after a previous metrics write failure, and `logger.missed_log_count` after a previous logger action write failure; if logger output was configured and allowed by level/module filters, appends one minimal action line. Periodic metrics flushes reuse the same metrics payload every 60 seconds while running, but are not `/actions` requests and do not write logger action lines. Full Firecracker counters and full logger integration remain deferred. |
| `PUT /actions` | `action_type=SendCtrlAltDel` | intentionally unsupported; parser rejected | Firecracker gates this on x86 keyboard behavior; the first target is Apple Silicon. The request is still counted in `put_api_requests.actions_count` without an `actions_fails` increment. |
| `PUT /actions` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |

The API and VMM state path implement the `PUT /machine-config` and
`PATCH /machine-config` field policies above. Valid pre-boot
`PUT /machine-config` requests replace the stored full machine configuration,
while valid pre-boot `PATCH /machine-config` requests update only provided
fields and preserve omitted stored values. Both return `204 No Content` on
success; invalid updates leave the stored configuration unchanged.
`GET /machine-config` returns the stored or default configuration. The stored
values are applied during `InstanceStart` startup.

Entropy support is tracked by #797 and needs to move as one guest-visible
capability, not only a configuration endpoint. The current supported subset is
one Firecracker-shaped virtio-rng device configured before startup with no
rate limiter, attached as a virtio-mmio device in the arm64 FDT, backed by host
OS randomness, and dispatched from the HVF boot loop. A backend-neutral
virtio-rng queue handler and runtime MMIO activation/notification layer can fill
writable guest descriptor chains from an injected entropy source under unit
tests, including malformed-buffer, source-failure, reset, and queue-interrupt
completion paths. The public API can now store the empty entropy configuration,
include it in `GET /vm/config` as `"entropy": {}`, and pass it into
`InstanceStart` so the existing HVF startup path attaches the device. The
config-file startup path accepts the same no-rate-limiter entropy configuration
and rejects configured rate limiters before publishing readiness. The signed
executable HVF e2e target now validates the guest-visible path by booting the
generated direct-rootfs image, checking that Linux selected `virtio_rng` as the
current hardware RNG, and reading non-empty data from `/dev/hwrng` before
writing a host-observable marker. Metrics, security docs, and real rate
limiting remain deferred.

The API and VMM state path also route valid snapshot requests through explicit
actions before returning unsupported faults. `PUT /snapshot/create` fails as an
unsupported state before startup and reaches the snapshot unsupported fault once
the instance is started. `PUT /snapshot/load` reaches the snapshot unsupported
fault before startup and fails as an unsupported state after startup. Malformed
or schema-invalid snapshot bodies are still rejected by the parser first.

Snapshot support needs a dedicated macOS/HVF design before either request can
move beyond recognized unsupported behavior. Firecracker's implementation saves
microVM state, KVM VM state, vCPU state, and device-manager state, writes a
separate guest-memory image, can load memory from a file or Linux userfaultfd,
can enable KVM dirty-page tracking for diff snapshots, and can apply
network/vsock restore overrides before optionally resuming the VM. bangbang must
first define HVF equivalents for paused run-loop ownership, guest-memory file
persistence, HVF vCPU and VM state capture/restore, device-state serialization,
host-path redaction and ownership, and failure handling that leaves the process
in a known terminal or recoverable state. These are implementation deferrals
until a concrete macOS/HVF limitation is proven; unknown HVF feasibility should
not be reported as a platform limit by default. The first supported subset also
needs an explicit snapshot format decision: Firecracker file-format
compatibility, a bangbang-native format behind Firecracker-shaped API requests,
or a documented unsupported boundary.

`GET /vm/config` returns the accumulated supported VM configuration subset
without side effects. It includes the stored/default `machine-config`, includes
`boot-source` only after it is configured, and always includes a `drives` array
for configured virtio-block drives plus a `network-interfaces` array for stored
network interface configs. It includes `vsock` only after `PUT /vsock` stores a
valid configuration. It includes `mmds-config` after successful MMDS
configuration storage. It includes `entropy` as an empty object after
successful `PUT /entropy` configuration, and includes `balloon` after
successful `PUT /balloon` configuration. Firecracker sections without stored
configuration models, including snapshots and hotplug, are omitted until those
models exist.
Metrics and logger output configuration are also omitted because they are
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
The process startup path and API/VMM state path implement the metrics field
policy above as a pre-boot-only per-process output sink. Startup CLI can
initialize the metrics sink before the API socket is served. Duplicate
initialization fails without replacing the original sink. `FlushMetrics` is
runtime-only: it fails before startup, succeeds without writing when the sink is
unconfigured, and writes one minimal JSON line when `--metrics-path` or
`PUT /metrics` configured an output path. Startup CLI and config-file metrics
paths also write one initial minimal metrics line when the sink is configured.
API-enabled and no-api runtime loops flush the same metrics line every 60
seconds while the VM is running; those periodic flushes do not log `/actions`
events. The line includes initial Firecracker-shaped GET API counters for
`get_api_requests.instance_info_count`, `vmm_version_count`,
`machine_cfg_count`, `mmds_count`, and `hotplug_memory_count`, plus selected
`put_api_requests` counters for parsed core configuration PUTs, `PUT /mmds`,
`PUT /mmds/config`, `PUT /metrics`, `PUT /logger`, `PUT /serial`,
`PUT /hotplug/memory`, `PUT /pmem/{pmem_id}`, and `/actions` requests routed
through VMM control, parser failures for those endpoints in the matching
`put_api_requests` count/fail counters, plus selected
`patch_api_requests` counters for parsed
`PATCH /machine-config`, `PATCH /mmds`, `PATCH /drives/{drive_id}`,
`PATCH /network-interfaces/{iface_id}`, `PATCH /hotplug/memory`, and
`PATCH /pmem/{pmem_id}` requests routed through VMM control and parser failures
for those PATCH endpoints. bangbang also records
`balloon_count` extension fields for parsed balloon GET, PUT, and PATCH routes,
plus `balloon_fails` extension fields for parsed balloon PUT and PATCH
failures and identifiable malformed balloon PUT/PATCH parser failures, because
Firecracker does not expose matching request metrics. It also does not emit
entropy request-counter fields; Firecracker does not define
`put_api_requests.entropy_count` or `entropy_fails`. Entropy device runtime
metrics remain deferred until their producers exist.
Parsed deprecated HTTP API usage is counted under
`deprecated_api.deprecated_http_api_calls` for the supported deprecated fields
above; malformed parser failures remain outside the deprecated counter.
Device runtime counters, remaining API request counters, and parser-level
malformed-request counters for endpoints without Firecracker-shaped request
metric fields remain deferred. Public run-loop control, guest boot
output, public runner loop scheduling, full Firecracker metrics counters, and
full logger integration remain deferred. Metrics write failures increment
`logger.missed_metrics_count`; logger action write failures increment
`logger.missed_log_count`; broader logger metrics remain deferred.
The process startup path and API/VMM state path implement the logger field
policy above as pre-boot-only per-process observability configuration. Startup
CLI flags can configure the initial logger before the API socket is served.
Repeated pre-boot `PUT /logger` requests update only the fields they provide,
including after startup CLI configuration. Runtime requests fail without opening
a new output path. The configured logger sink records minimal successful
`InstanceStart` and `FlushMetrics` action lines when the logger level allows
`Info`. `show_level` adds `level=Info`, and `show_log_origin` adds the runtime
action callsite as `origin=<file>:<line>`. It is not wired into the full process
logging backend yet.
The API and VMM state path implement the `PUT /vsock` field policy above as a
pre-boot-only guest configuration section. Valid requests replace the stored
vsock config and return `204 No Content`; invalid requests fail without
mutating previous vsock state. The current response reports `guest_cid` and
`uds_path` while omitting the accepted deprecated `vsock_id`, matching
Firecracker's effective config output. Startup preparation attaches the
configured virtio-vsock device as guest-visible FDT/MMIO metadata. The runtime
also models Firecracker's 44-byte little-endian virtio-vsock packet header and
can parse guest-readable TX descriptor chains into validated packet metadata and
payload segments. Header byte parsing rejects payload lengths above the 64 KiB
maximum packet buffer length, and TX parsing rejects payload lengths larger
than readable descriptor bytes after the header. The handler can dispatch TX
queue notifications into descriptor completions and can dispatch RX work into
pending host request-header delivery, including retrying delivery for pending
host requests after host-side `CONNECT` polling. Startup can dispatch those
notifications through the boot loop while signaling the allocated vsock
interrupt line when completed descriptors require it. Startup can also bind and
own a nonblocking host listener at `uds_path`. The runtime also parses
Firecracker-shaped host `CONNECT <PORT>` requests, allocates Firecracker-shaped
host local ports, retains host-initiated accepted streams in an internal table,
can expose a one-shot guest-facing `VSOCK_OP_REQUEST` packet header for a
retained host connection, can dispatch that pending request header into
writable guest RX descriptors with used-ring completion metadata, can accept
one pending host connection per dispatch pass into an owned nonblocking stream,
can retain bounded accepted streams across partial handshakes and retained
connection records, can drop invalid accepted-stream handshakes without path
exposure, can retry RX delivery when pending host requests exist without
requiring a fresh guest RX notification, and can acknowledge guest
`VSOCK_OP_RESPONSE` packets for delivered host requests by writing
`OK <local_port>\n` to the retained host stream. The runtime can also queue
bounded zero-payload `VSOCK_OP_RST` headers for unsupported or orphan
host-destined guest TX packets and deliver those reset headers through the
existing RX queue path. Supported guest `VSOCK_OP_REQUEST` packets can attempt
nonblocking connects to Firecracker-shaped `${uds_path}_${PORT}` sockets,
retain successful guest-initiated streams, and deliver guest-visible
`VSOCK_OP_RESPONSE` headers; established host-initiated or guest-initiated
connections can forward bounded `VSOCK_OP_RW` payload bytes to the retained
host stream, keep a bounded four-packet per-connection guest-to-host retry queue
for partial or would-block nonblocking writes, and retry pending bytes on later
notification dispatch before accepting more guest `RW` data for the same
connection;
established host-initiated and guest-initiated connections can retain a bounded
four-packet per-connection backlog of host `VSOCK_OP_RW` payloads and deliver
one queued payload at a time into guest RX buffers; guest `VSOCK_OP_RST`
packets drop matching retained host-initiated or guest-initiated connections
without queuing guest-visible RX output; partial guest `VSOCK_OP_SHUTDOWN`
packets record receive/send closure state and apply TX shutdown control before
same-window RX host-payload delivery, while full guest `VSOCK_OP_SHUTDOWN`
packets drop matching retained connections and queue guest-visible
`VSOCK_OP_RST` headers; valid guest `VSOCK_OP_CREDIT_UPDATE` packets for
established retained streams are consumed without a reset, and valid guest
`VSOCK_OP_CREDIT_REQUEST` packets queue zero-payload guest-visible
`VSOCK_OP_CREDIT_UPDATE` headers through the existing RX path; connect,
retention, RW forwarding, host-stream EOF, or host-stream read failures deliver
guest-visible `VSOCK_OP_RST` headers.
Beyond connection setup, forceful guest reset/full-shutdown cleanup, and minimal
partial shutdown state, guest-visible socket lifecycle remains deferred;
Firecracker's full graceful-shutdown timeout/kill-queue behavior, full
virtio-vsock credit accounting, CID routing beyond current host/guest checks,
and full event payload dispatch also remain deferred. Event queue notifications
are accepted as no-op dispatch metadata.
Signed executable validation currently covers narrow guest-initiated and
host-initiated multi-payload exchanges through Firecracker-style vsock paths,
guest-initiated and host-initiated multi-stream exchanges, plus guest-initiated
and host-initiated EOF cleanup after guest close; it does not cover
throughput-oriented streaming, full graceful-shutdown timeout/kill-queue
behavior, full credit accounting, or broader socket lifecycle parity.
`SendCtrlAltDel` is rejected at parse time for the first aarch64 target while
still contributing to the `/actions` request count metric.

Future implementation PRs should derive unit or golden tests from these tables.
User documentation should keep the same support and field-status vocabulary when
API behavior ships. Security review must cover host paths, socket-like fields,
device identifiers, and error messages. Performance review must cover boot path
setup, memory size, and block device I/O when those surfaces are implemented.

## Internal Virtio-Balloon Foundation

The runtime crate can derive a backend-neutral virtio-balloon prepared model
from stored `PUT /balloon` configuration. The model includes virtio device ID
5, `VIRTIO_F_VERSION_1`, optional deflate/statistics/hinting/reporting feature
bits, compacted inflate/deflate/statistics/hinting/reporting queue metadata
with 256 descriptors per queue, and Firecracker's 12-byte config space:
`num_pages`, `actual_pages`, and `free_page_hint_cmd_id`.

Startup can attach this model as a virtio-mmio/FDT device with the configured
identity, feature, queue, and config-space registers. Guest config-space writes
update only the local device register state. Runtime hinting start and stop
commands update only host-owned command state, mirror it into the active
config-space command field, and raise a config interrupt. The hinting status API
reports that host command state plus the latest 4-byte guest command observed on
the hinting queue; `guest_cmd` remains `null` until the guest sends a command
descriptor. The backend-neutral inflate queue dispatcher
reads bounded PFN descriptor payloads, compacts them into page ranges,
and publishes zero-length used-ring entries; the deflate queue dispatcher
also reads bounded PFN descriptor payloads, compacts them into page ranges, and
publishes zero-length used-ring entries. The hinting queue dispatcher records
the latest 4-byte command descriptor as `guest_cmd`, validates and records
active-run range descriptors in dispatch state, and publishes zero-length
used-ring entries. Host-side hinting reclaim and reporting remain deferred.
Boot runtime resources and the HVF boot loop can drain pending balloon
inflate/deflate/hinting notifications and signal the allocated balloon interrupt
line when the runtime dispatch summary reports queue-interrupt intent.
Completed inflate/deflate descriptors update internal inflated-page accounting
on the owning balloon device after PFN ranges are validated against mapped guest
memory, and reset clears that accounting.
Runtime `PATCH /balloon` updates the active `num_pages` config-space value,
increments config generation, and raises a config interrupt. The
`GET /balloon/statistics` endpoint returns required target fields from the
current stored target and required actual fields from the internal inflated-page
accounting, and it includes optional guest-reported fields after bounded
statistics queue reports record them.
Runtime `PATCH /balloon/statistics` updates stored and active nonzero polling
intervals while rejecting runtime statistics enable/disable transitions.
Periodic statistics polling, timer-driven statistics descriptor completion,
reporting, and host memory reclaim remain deferred.

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
- requests above the architectural maximum are capped inside the layout model,
  while public machine configuration rejects them before storage

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

The API crate has strict Firecracker-shaped `PUT /drives/{drive_id}` and
`PATCH /drives/{drive_id}` request parser and body models. Initial drive
configuration accepts the documented drive fields, rejects unknown fields,
rejects malformed or incomplete JSON bodies, rejects extra path segments, and
rejects path/body `drive_id` mismatches without echoing host paths. Drive update
requests parse the Firecracker-shaped `drive_id`, `path_on_host`, and
`rate_limiter` fields, reject invalid or mismatched bodies, and route valid
runtime updates to the process-owned block-device refresh path. The running API server
converts parsed initial drive requests into a VMM action; valid pre-boot
requests are recorded as VM configuration state and return `204 No Content`.

The runtime crate has an internal, Firecracker-shaped drive configuration model
for the initial virtio-block subset. It validates path and body `drive_id`
values as nonempty alphanumeric strings with `_`, requires the two IDs to
match, rejects an empty `path_on_host` without opening or statting host files,
and normalizes omitted `is_read_only` to read-write.

The internal model accepts omitted/default `cache_type=Unsafe`, explicit
`cache_type=Writeback`, and omitted/default `io_engine=Sync`, and rejects
`Async`, configured rate limiters, and configured sockets as unsupported.
Displayed errors avoid echoing `path_on_host` so future API code can preserve
host path redaction.

The runtime crate can also open the normalized `path_on_host` as a regular
host file, preserve the configured read-only mode, report byte length, and
perform bounded positioned reads/writes and flushes for internal virtio-block
request execution. It rejects non-regular backing paths before data I/O and
rejects read-only writes before mutating the file. Backing errors also avoid
echoing `path_on_host`. Public startup opens configured backing paths during
`InstanceStart`, and runtime drive updates open replacement backing paths before
mutating the active virtio-block handler or stored VMM configuration.

Virtio-block feature negotiation follows the selected cache mode:
`cache_type=Unsafe` keeps the flush feature hidden, while
`cache_type=Writeback` advertises `VIRTIO_BLK_F_FLUSH` and uses the existing
backing-file flush path for guest flush requests.

Runtime drive backing refresh is limited to replacing an existing active drive's
host file. It does not create new virtio-block devices, remove existing devices,
renegotiate features, or apply rate limiters. A successful refresh updates the
matching MMIO handler's backing, config space, config generation, and config
interrupt status; failure preserves the previous backing and stored config.

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

The internal model accepts configured `mtu` values in the Firecracker-compatible
`68..=65535` range, preserves them in stored configs and `GET /vm/config`,
and exposes them to the guest through `VIRTIO_NET_F_MTU`. The internal model
still rejects configured `rx_rate_limiter` and `tx_rate_limiter` fields as
unsupported. bangbang currently limits stored network interfaces to 16.
Firecracker `v1.16.0` does not publish a separate network-interface count
limit; this is a macOS/HVF host-resource boundary for the current scaffold.
Configuration storage does not open host networking resources or change host
vmnet MTU settings. Stored network interface configs are returned from
`GET /vm/config` in the `network-interfaces` array. During `InstanceStart`, the
process crate revalidates the count before selecting packet I/O. If every
configured interface is listed in MMDS config, startup still validates
`host_dev_name` syntax against the supported vmnet forms but can use
process-local MMDS-only packet I/O without opening vmnet resources. Otherwise,
it maps `host_dev_name` values `vmnet:host`, `vmnet:shared`, and
`vmnet:bridged:<interface>` to vmnet host, shared, and bridged configurations,
with the bridged suffix required to be nonempty and free of NUL bytes and ASCII
control characters. Startup does not verify that the named macOS interface
exists before building cleanup-owning packet I/O for each configured interface.
Other nonempty names are still accepted before boot but fail startup before
`Running` is committed.

## Internal Vsock Configuration

The API and runtime crates implement pre-boot, Firecracker-shaped vsock
configuration storage and internal virtio-vsock device work. The API parser accepts
`PUT /vsock`, rejects unknown fields, and forwards the supported request shape
through the VMM action boundary. The runtime requires `guest_cid >= 3`, accepts
the deprecated optional `vsock_id` when it is nonempty and contains no control
characters, and requires a nonempty `uds_path` with no control characters.
Displayed validation errors avoid echoing configured socket paths.

Stored vsock configuration replaces any previous pre-boot vsock configuration
and is returned from `GET /vm/config` as `vsock` with `guest_cid` and
`uds_path`; the deprecated input-only `vsock_id` is omitted. The configuration
request itself does not open, bind, connect, unlink, or create the configured
`uds_path`; startup preparation later binds the nonblocking host listener.

The runtime crate has an internal virtio-vsock prepared resource, MMIO
registration helper, config-space, 44-byte little-endian packet header model,
guest-readable TX descriptor packet parser, TX available-ring drain helper with
used-ring descriptor completion,
MMIO handler skeleton with active queue metadata retention, RX/TX notification
dispatch, no-op event notification handling, and startup FDT attachment. It uses the
virtio device id `19`, three 256-entry RX, TX, and event queues, Firecracker's
`VERSION_1`, `IN_ORDER`, and `EVENT_IDX` feature bits, and a guest-CID config
field that supports Firecracker-shaped 8-byte and 4-byte-half reads. Config
writes are rejected. The packet header model preserves Firecracker's header
field order and rejects header byte parsing when payload length exceeds
Firecracker's 64 KiB maximum packet buffer length. The TX packet parser reads
headers across descriptor boundaries, validates readable descriptor ranges, and
returns payload segment metadata trimmed to the advertised payload length. The
TX drain helper consumes available TX descriptor chains from the active queue
into parsed packet metadata while preserving queue progress and publishes
zero-length used-ring completions for consumed descriptor heads. When
`EVENT_IDX` is negotiated, RX and TX dispatch use the available-ring
`used_event` value to decide whether completed descriptors require a queue
interrupt. The handler can
drain RX, TX, and no-op event queue notifications, dispatch the active RX queue
for pending host request headers, dispatch the active TX queue, preserve
completed RX/TX dispatch metadata on errors, and mark the virtio queue
interrupt status when completed descriptors require guest notification. Boot
runtime resources can dispatch the registered vsock MMIO handler's RX/TX
notifications plus no-op event notifications, and internal HVF boot sessions can
signal the allocated vsock SPI line from those dispatch summaries. The prepared
resource preserves the validated guest CID, socket path,
config-space, and inactive device state. Arm64 startup resource assembly can
bind and own the nonblocking host listener at `uds_path`, retain that owner in
the internal vsock device resource, and expose one configured vsock device in
the guest FDT. The runtime can parse Firecracker-shaped host `CONNECT <PORT>`
requests into a guest port, allocate Firecracker-shaped host local ports,
retain host-initiated accepted streams in an internal table, expose a one-shot
guest-facing `VSOCK_OP_REQUEST` packet header for a retained host connection,
dispatch that pending request header into writable guest RX descriptors with
used-ring completion metadata, accept one pending host connection per dispatch
pass into an owned nonblocking stream, retain bounded accepted streams across
partial handshakes and retained connection records, drop invalid
accepted-stream handshakes without path exposure, retry RX delivery when
pending host requests exist without requiring a fresh guest RX notification,
and acknowledge guest `VSOCK_OP_RESPONSE` packets for delivered host requests
by writing `OK <local_port>\n` to the retained host stream. The runtime can
also queue bounded zero-payload `VSOCK_OP_RST` headers for unsupported or orphan
host-destined guest TX packets and deliver those reset headers through the
existing RX queue path. Supported guest `VSOCK_OP_REQUEST` packets can attempt
nonblocking connects to Firecracker-shaped `${uds_path}_${PORT}` sockets,
retain successful guest-initiated streams, and deliver guest-visible
`VSOCK_OP_RESPONSE` headers; established host-initiated or guest-initiated
connections can forward bounded `VSOCK_OP_RW` payload bytes to the retained
host stream, keep a bounded four-packet per-connection guest-to-host retry queue
for partial or would-block nonblocking writes, and retry pending bytes on later
notification dispatch before accepting more guest `RW` data for the same
connection;
established host-initiated and guest-initiated connections can retain a bounded
four-packet per-connection backlog of host `VSOCK_OP_RW` payloads and deliver
one queued payload at a time into guest RX buffers; guest `VSOCK_OP_RST`
packets drop matching retained host-initiated or guest-initiated connections
without queuing guest-visible RX output; partial guest `VSOCK_OP_SHUTDOWN`
packets record receive/send closure state and apply TX shutdown control before
same-window RX host-payload delivery, while full guest `VSOCK_OP_SHUTDOWN`
packets drop matching retained connections and queue guest-visible
`VSOCK_OP_RST` headers; valid guest `VSOCK_OP_CREDIT_UPDATE` packets for
established retained streams are consumed without a reset, and valid guest
`VSOCK_OP_CREDIT_REQUEST` packets queue zero-payload guest-visible
`VSOCK_OP_CREDIT_UPDATE` headers through the existing RX path; connect,
retention, RW forwarding, host-stream EOF, or host-stream read failures deliver
guest-visible `VSOCK_OP_RST` headers.
Beyond connection setup, forceful guest reset/full-shutdown cleanup, and minimal
partial shutdown state, guest-visible socket lifecycle remains deferred;
Firecracker's full graceful-shutdown timeout/kill-queue behavior, full
virtio-vsock credit accounting, CID routing beyond current host/guest checks,
and full event payload dispatch also remain deferred. Event queue notifications
are accepted as no-op dispatch metadata.

The runtime crate also has the first backend-neutral virtio-net config-space,
activation, TX frame parser, RX buffer parser, prepared device resources, and
MMIO registration helpers. They define the
Firecracker-shaped virtio network device id, RX/TX queue indexes, two queue
sizes, the guest-MAC feature bit, a `VirtioMmioDeviceConfigHandler` for reading
a configured guest MAC through the existing virtio-mmio register handler, a
`DRIVER_OK` activation handler that validates and retains dispatchable RX and TX
queues, an internal TX frame parser for the 12-byte virtio-net header plus
guest-readable payload segments, and an internal RX buffer parser for
guest-writable receive buffer segments with the current 1526-byte Firecracker
non-merged-RX minimum. Preparation can build ordered owned resources from stored
configs, preserving `iface_id`, `host_dev_name`, guest-MAC config space, and an
inactive `VirtioNetworkDevice` without opening host networking resources. The
prepared resources can be consumed into deterministic virtio-mmio regions and
handlers in a fresh or existing internal `MmioDispatcher`, returning read-only
registration metadata. Startup preparation can pair those registrations with
caller-provided interrupt lines and write matching inert virtio-mmio descriptors
into the guest FDT while preserving interface order and host device names.
Internal network notification dispatch can drain pending TX and RX queue
notifications and can choose injected packet I/O per configured interface at the
boot-runtime boundary. The HVF boot-session wrapper can invoke that injected
path. Process-owned startup uses a no-op provider when no network interfaces are
configured, can build process-local MMDS-only packet I/O when every configured
interface is selected by MMDS config, and otherwise builds vmnet packet I/O for
configured interfaces during `InstanceStart`. TX dispatch walks the TX
available ring, parses descriptor chains into `VirtioNetworkTxFrame` metadata,
publishes used-ring
completions with length 0, delivers parsed frames to an injected internal packet
sink, preserves parse, sink, and partial-dispatch errors, and marks queue
interrupt status when descriptor heads complete unless negotiated `EVENT_IDX`
suppresses the notification. RX dispatch uses an injected
internal packet source, can perform one bounded post-TX RX retry when that
source reports already-ready packets, copies a zeroed 12-byte virtio-net header
plus packet payload into validated guest-writable RX buffers, publishes
used-ring completions with the written length, preserves malformed-buffer and
partial-dispatch metadata, and marks queue interrupt status when RX buffers
complete unless negotiated `EVENT_IDX` suppresses the notification. On macOS,
the process crate also defines internal vmnet descriptor,
lifecycle, start owner, packet descriptor, and concrete system start/stop
backend boundaries with vmnet mode, status, operation error, XPC descriptor
configuration, retained dispatch queue ownership, completion-status mapping,
backend start/stop ownership, packet `iovec` layout, single-packet system
`vmnet_read`/`vmnet_write` wrappers, count validation, owned cleanup models,
an internal cleanup-owning packet backend that can delegate read/write while
retaining vmnet stop-on-drop ownership, an internal virtio-net packet I/O
adapter that copies TX guest-memory payload segments into vmnet writes and
caches one vmnet RX packet until consumed, an MMDS-only packet I/O adapter that
detours MMDS TX frames and drops non-MMDS TX frames without opening vmnet, and
prebuilt adapter providers keyed by configured interface ID. It also defines an
internal `host_dev_name` mapping for `vmnet:host`, `vmnet:shared`, and
`vmnet:bridged:<interface>`, where bridged interface names are nonempty and
contain no NUL bytes or ASCII control characters. Startup with configured
network interfaces revalidates the 16-interface limit before selecting packet
I/O, can use the MMDS-only adapter when every configured interface is selected
by MMDS config, and otherwise opens vmnet resources through those supported
forms and retains stop-on-drop cleanup. Startup without network interfaces
still uses a no-op TX sink and an empty RX source. These helpers can advertise
configured guest-visible MTU values, but they do not change host vmnet MTU settings,
support rate limiters, prove host connectivity, or provide public runtime
packet movement.

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
`VIRTIO_F_VERSION_1` and `VIRTIO_RING_F_EVENT_IDX`, advertises
`VIRTIO_BLK_F_FLUSH` for `cache_type=Writeback`, and advertises
`VIRTIO_BLK_F_RO` for read-only drives.
The config handler supports bounded read-only capacity reads through the
existing virtio-mmio device-configuration path and rejects config writes.

The runtime model is wired to successful pre-boot `PUT /drives/{drive_id}` VMM
configuration storage. Public `InstanceStart` startup can call block-device
preparation, MMIO registration, and FDT device description for initial
configured drives. When a configured drive is the root device, startup appends
Firecracker-style root-drive kernel command-line arguments before writing FDT
`chosen.bootargs`: `root=PARTUUID=<partuuid>` when `partuuid` is configured,
otherwise `root=/dev/vda`, followed by `ro` for read-only drives or `rw` for
writable drives. The final command line is still checked against the arm64
2048-byte command-line limit after these automatic arguments are appended.
The internal boot run loop across bounded step windows can dispatch active
block queue notifications and signal interrupts. The signed `guest_boot`
integration target now validates that the pinned Firecracker arm64 kernel can
discover the first virtio-block device as `/dev/vda` and read a marker from a
temporary host backing file through the raw block device. The same target also
validates a basic writable-drive path by writing a marker from the guest to
`/dev/vda` and checking the scratch host backing file. It also attaches the
pinned Firecracker squashfs rootfs as a read-only root drive, mounts it from the
deterministic initrd, and reads `/mnt/etc/os-release` from the guest. Direct
root-drive boot validation is covered by a generated ext4 variant of the same
Firecracker rootfs that adds a deterministic test init, boots without an initrd,
mounts the virtio-block root drive as `/`, and reads `/etc/os-release`; the
signed `guest_boot` target validates guest-visible `root=/dev/vda ro`
command-line arguments through captured serial output, and the signed
executable HVF e2e target validates the same direct-rootfs path through public
API configuration and config-file startup plus a host-observed scratch block
marker. This does not yet claim arbitrary distro rootfs boot or default Ubuntu
systemd startup.
Block hotplug, remaining cache-mode semantics, and rate limiting remain
deferred.
It does not provide a public runner control, implement rate limiting, support
vhost-user-block sockets, or use an async I/O engine. Internal HVF boot sessions
can signal block SPI interrupts after boot-runtime block notification dispatch.

## Internal arm64 FDT Generation

The runtime crate can build a minimal Firecracker-shaped arm64 FDT using the
same `vm-fdt` writer crate that Firecracker uses. The generated tree currently
contains root properties, CPU data, memory, chosen, timer, PSCI, GIC nodes, and
optional RTC, serial, and sorted virtio-mmio device nodes from caller-supplied
descriptors. The optional RTC node uses Firecracker's aarch64 PL031 shape with
`compatible = "arm,pl031", "arm,primecell"`, `reg`, `clocks`, and
`clock-names = "apb_pclk"`, and intentionally omits `interrupts` because the
minimal RTC device does not implement alarm interrupts. PCI and other device
nodes remain deferred until the corresponding emulation paths exist.
Because the FDT advertises PSCI with `method = "hvc"`, the HVF backend decodes
arm64 HVC exception exits and handles `HVC #0` as a minimal PSCI 0.2 responder
for early single-vCPU boot probing. The responder returns `PSCI_VERSION`,
reports feature support only for the implemented minimal calls, returns
`MIGRATE_INFO_TYPE` as the PSCI value for a trusted OS that is MP-capable or
not present, where migration is not required, and translates `SYSTEM_OFF` and
`SYSTEM_RESET` into guest-requested terminal boot run-loop outcomes. It writes
`NOT_SUPPORTED` to X0 for other PSCI calls or HVC immediates.
Early boot also traps the guest's `OSDLR_EL1` and `OSLAR_EL1` OS lock
system-register accesses through the AArch64 SYS64 exception class (`0x18`),
not through SMC/SMCCC. The HVF runner handles only those observed
debug-register accesses with KVM-like RAZ/WI semantics: reads return zero,
writes are ignored, and other trapped system registers still fail closed.

The memory node excludes the first 2 MiB system area from the first DRAM range
and preserves later DRAM ranges from the runtime layout, but direct FDT
configuration must match the aarch64 DRAM layout helper for its total guest RAM
size. Sparse layouts, ranges overlapping the aarch64 MMIO64 gap, and total RAM
beyond the aarch64 maximum are rejected. The chosen node carries boot arguments,
optional initrd start/end properties from loaded boot-source metadata,
Firecracker's `linux,pci-probe-only = 1` property, and a Firecracker-shaped
64-byte `rng-seed` generated from host OS randomness during FDT construction.
`rng-seed` generation failures are reported before guest memory is mutated
during FDT writes. Emitting `linux,pci-probe-only` matches Firecracker's arm64
FDT shape but does not imply PCI device support.
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

## RTC-Adjacent Time And Identity Devices

bangbang currently implements only the guest-visible PL031 RTC subset described
above. RTC alarm interrupts are intentionally unsupported in that subset because
Firecracker's aarch64 PL031 node is exposed without an interrupt line; guest
flows that depend on RTC alarm interrupts should not be treated as supported by
the current compatibility surface.

PVTime/steal-time is a separate platform capability rather than an RTC feature.
Firecracker implements ARM steal-time by allocating per-vCPU memory and
registering it through KVM ARM vCPU device attributes. bangbang should not claim
PVTime until an HVF-specific capability and guest ABI design exists; for now it
is platform-limited and deferred.

VMGenID/SysGenID and VMClock are supported-target device families, but they are
not part of the minimal RTC device. Firecracker exposes them as guest memory
regions with interrupt notification and updates them around snapshot restore.
bangbang should implement them only after guest-memory placement, FDT metadata,
interrupt signaling, persistence, and snapshot/restore semantics are designed.
Until snapshot support is implemented, VMGenID/VMClock restore notifications and
generation-counter updates remain deferred to the snapshot work tracked by
#543.

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
initrd, prepares configured block and network devices, registers their
virtio-mmio regions in a fresh internal `MmioDispatcher`, optionally registers
one PL031 RTC handler and one TX-only serial MMIO handler in the same dispatcher,
pairs block and network registrations with supplied SPI interrupt lines, and
writes the arm64 FDT with matching RTC, serial, and virtio-mmio metadata.

The assembled bundle owns the guest memory, loaded boot metadata, FDT write
metadata, MMIO dispatcher, optional RTC metadata, optional serial metadata/output
sink, and block and network FDT device metadata needed by later HVF startup
wiring. It fails with typed errors
for missing boot source, memory size
overflow or a memory size above the arm64 architectural maximum,
layout/allocation failure, boot-source loading failure, block-device preparation
failure, RTC, serial, block, or network MMIO registration failure, interrupt-line count
mismatch, or FDT write failure.

The assembled bundle is used by owned HVF startup preparation. HVF owns the
mapped guest memory while runtime metadata, the MMIO dispatcher, optional RTC
metadata, optional serial metadata, and block/network metadata stay available to
the retained session. bangbang
now starts an internal boot run-loop worker across bounded step windows after successful startup and retains internal active, terminal-outcome, or error worker status, but
does not yet provide public run-loop control, signal backend
interrupts outside the internal boot block and network notification paths,
or prove guest boot with an integration test.

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
memory without a caller-chosen bound. A shared sink lets the internal
boot-resource assembly path register a serial handler while retaining an output
handle for default internal capture or a configured file-backed output path.
The internal arm64 FDT builder can describe the same serial MMIO descriptor as
a Firecracker-shaped `uart@...` node. Public `/serial` supports pre-boot
`serial_out_path` storage and startup-time host output redirection; kernel
`earlycon` wiring, serial input/RX, rate limiting, metrics, and public serial
streaming remain deferred. The first internal guest boot integration test uses
the bounded capture path directly.

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
deferred. Activated queue metadata can now feed the internal virtio-block,
virtio-net, and virtio-vsock queue dispatchers. Boot runtime resources
can dispatch registered block-device, virtio-net, and virtio-vsock queue notifications
against caller-supplied guest memory. Internal HVF boot sessions can signal
needed block, network, and vsock SPI interrupts from those dispatch summaries, but
future public scheduler and device policy remain deferred. The
shared virtqueue descriptor-chain reader supports direct chains by default and
negotiated `VIRTIO_RING_F_INDIRECT_DESC` indirect descriptor tables for
virtio-block, virtio-net, and virtio-vsock RX/TX queues, while preserving the
main descriptor head for used-ring completions. The virtqueue model can publish
one used-ring completion element with validated layout, mapped-memory checks,
wrapping, and release ordering. Virtio-block queue dispatch, network RX/TX
dispatch, and vsock RX/TX dispatch honor negotiated used-event interrupt
suppression for each published completion and publish a shared used-ring
`avail_event` hint for available-buffer notification suppression, while
batching and device-backed completion loops are still deferred.

The runtime crate also contains backend-neutral interrupt signaling groundwork.
It can validate nonzero guest interrupt lines, represent queue and
configuration pending-status bits, acknowledge selected pending bits, and let a
device-facing trigger record pending state before delegating backend signaling
to an injected sink. The HVF crate can allocate deterministic guest interrupt
lines from the validated GIC SPI range, signal validated SPI levels through
`hv_gic_set_spi`, and set or clear validated GIC PPI pending bits through
redistributor pending registers on the vCPU-owning thread. Internal HVF boot
sessions use the SPI signal path for block queue interrupts and virtio-net
queue interrupts after boot-runtime notification dispatch. This follows Firecracker's separation between
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
delivery, or real device emulation beyond the internal boot block and
virtio-net notification signal steps.

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
HVF boot session plus configured or default internal serial output after successful startup. A
bounded internal
boot-session run-loop pump now composes that one-step path with boot block,
virtio-net, and virtio-vsock notification dispatch between successful MMIO steps and virtual
timer PPI assertion after virtual timer exits. It stops explicitly on a step limit,
stop-token request, canceled run exit, PSCI `SYSTEM_OFF` or `SYSTEM_RESET`
guest request, unknown run exit, dispatch error, or timer handler error. This
remains internal runner-loop plumbing, not the future public guest scheduler.
For process-owned API-enabled and no-api runs, PSCI `SYSTEM_OFF` and
`SYSTEM_RESET` wake the process supervisor and let the process exit
successfully. Non-success terminal worker states wake the same supervisor path
and fail the process with the current coarse process-failure exit status.
An owned internal session handle preserves the same
session operations while avoiding a self-referential backend/session owner in
process-level state.
The boot session can also dispatch pending boot block, virtio-net, virtio-vsock,
and virtio-balloon queue notifications against mapped guest memory and signal
the corresponding block, network, vsock, or balloon SPI line when the runtime
dispatch summary reports queue-interrupt intent; per-device results preserve
dispatch, lookup, and signal failures for later runner-loop policy.
Boot notification dispatch locks the shared dispatcher only while draining
runtime notifications and releases it before HVF GIC signaling.

bangbang now wires `mem_size_mib` into startup preparation, and process-level
tests cover the current HVF startup rejection for stored `vcpu_count` values
above one. It still does not wire device interrupts into public guest execution,
emulate devices, provide public run-loop control, power on secondary vCPUs,
implement reboot-in-place after `SYSTEM_RESET`, or implement full PSCI CPU
control and process exit-code parity for error power actions. Public
machine configuration rejects `mem_size_mib` above the current 1022 GiB Apple
Silicon/aarch64 DRAM maximum before storage; startup keeps its architectural
maximum check as a defensive guard. Dynamic host-memory availability policy
remains deferred.

## API State and Response Policy

The current scaffold implements the first HTTP API behavior for `GET /`,
`GET /version`, `GET /vm/config`, pre-boot `/machine-config` configuration
storage, pre-boot `PUT /boot-source`, `PUT /drives/{drive_id}`, and
`PUT /network-interfaces/{iface_id}`, `PUT /vsock`, `PUT /metrics`,
`PUT /logger`, and `PUT /serial` configuration storage, plus
process-routed `PUT /actions` startup with a bounded internal boot run-loop
worker and runtime metrics flush handling. The
policy below is the compatibility target for future request parsing, VMM action
mapping, state validation, and golden API tests.

The implemented `GET /version` path flows through the minimal VMM action model
as `GetVmmVersion` and returns VMM version data. The implemented `GET /` path
flows through the same boundary as `GetVmInstanceInfo` and returns
Firecracker-shaped instance information. Parsed `/machine-config` requests
flow through `GetMachineConfig`, `PutMachineConfig`, and `PatchMachineConfig`
and read, replace, or partially update stored machine configuration state.
`GET /vm/config` flows through
`GetVmConfig` and returns the supported accumulated configuration subset:
`machine-config`, `boot-source` when configured, the `drives` array, and the
`network-interfaces` array, plus `vsock` when configured.
Observability state such as metrics, logger, and serial output configuration is omitted. Unsupported top-level sections are omitted until their models exist. The implemented pre-boot drive path flows
through `PutDrive` and records validated configuration state. The implemented
pre-boot network-interface path flows through `PutNetworkInterface` and records
validated configuration state without opening host networking resources. Parsed
`/vsock` requests flow through `PutVsock` and replace validated vsock
configuration state without opening host Unix socket resources. Parsed
`/boot-source` requests flow through `PutBootSource` and replace stored
boot-source configuration state. Parsed `/metrics` requests flow through
`PutMetrics` and initialize per-process metrics output state that is not part of
guest configuration. Parsed `/logger` requests flow through `PutLogger` and
initialize or update per-process logger configuration state that is not part of
guest configuration. Parsed `/serial` requests flow through `PutSerial` and
store pre-boot serial output configuration that is also omitted from
`GET /vm/config`. Parsed `/actions` requests flow through `InstanceStart` and
`FlushMetrics` VMM actions. `InstanceStart` first validates stored boot-source
and state preflight, then the process VMM owner prepares and starts an owned
HVF boot-session worker with the configured serial output path or the default
internal serial MMIO capture buffer. It marks the instance `Running` only after
the bounded internal worker handle is retained; `FlushMetrics` fails before
startup, then succeeds after startup and writes one minimal JSON line only when
metrics output was configured. The API-enabled and no-api runtime loops also
write the same minimal metrics line every 60 seconds while the VM is running,
without routing those periodic flushes through `/actions`.

### Initial API State Model

The first API implementation should model the same broad stages as Firecracker:

- pre-boot: configuration requests are accepted and stored before guest
  execution starts
- starting: `PUT /actions` with `InstanceStart` validates the accumulated
  configuration, prepares the owned HVF startup session with configured or
  default internal serial output, and transitions the process out of pre-boot
  state on success
- runtime: the microVM is running; pre-boot-only configuration requests should
  fail with a Firecracker-shaped unsupported-state error
- paused/resumed: `PATCH /vm` parses `Paused` and `Resumed` requests, but real
  pause and resume behavior remains deferred

### Initial Operation State Matrix

| Operation | Pre-boot behavior | Runtime behavior | Notes |
| --- | --- | --- | --- |
| `GET /` | implemented; `200` JSON | implemented; `200` JSON | Response state reflects the current microVM state. |
| `GET /version` | implemented; `200` JSON | implemented; `200` JSON | Body uses Firecracker's `firecracker_version` field shape. |
| `GET /vm/config` | implemented; `200` JSON | implemented; `200` JSON | Returns the accumulated supported configuration subset, including an always-present `pmem` array that is populated after successful pre-boot pmem configuration, `mmds-config` after successful MMDS config storage, `entropy` after successful entropy configuration, and `balloon` after successful pre-boot balloon configuration. Startup applies the supported boot subset to an owned HVF session and internal boot run-loop worker across bounded step windows. |
| `GET /machine-config` | implemented; `200` JSON | supported target; `200` JSON | Returns the stored/default machine configuration. |
| `PUT /machine-config` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Pre-boot-only configuration. Stored values are applied during startup preparation. |
| `PATCH /machine-config` | implemented; `204` empty response on successful partial config update | unsupported after start; `400` `fault_message` | Pre-boot-only partial configuration. Omitted fields preserve current stored values; invalid updates leave stored values unchanged. |
| `PUT /boot-source` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Records validated pre-boot config; host paths are opened during startup preparation. Host path errors must avoid leaking sensitive path details. |
| `PUT /drives/{drive_id}` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Records validated pre-boot config; startup preparation opens backing files and registers initial block MMIO devices. Runtime hotplug remains deferred. |
| `PUT /pmem/{id}` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Records Firecracker-shaped pre-boot config and replaces prior config for the same ID without opening the host backing file during the request. Stored configs appear in `GET /vm/config`; startup opens and validates each backing as a non-zero regular file, mmaps it to a 2 MiB-aligned host range, assigns non-overlapping guest physical range/config-space metadata, attaches one virtio-mmio/FDT node per prepared pmem device, copies the prepared mapping into an HVF-compatible anonymous shadow, registers that shadow with HVF after DRAM, writes writable shadows back for guest queue-driven flush requests and after clean unmap, skips writeback after failed unmap cleanup, and retains the handles, mappings, and range metadata. Root-device semantics, rate limiting, runtime updates, dirty-range tracking, direct file-backed HVF mapping, and hot-unplug remain deferred. |
| `PATCH /drives/{drive_id}` | recognized post-boot-only operation; `400` `fault_message` | supported target; `204` empty response on successful backing refresh | Parses Firecracker-shaped update requests and routes valid bodies through `UpdateBlockDevice`. Runtime updates for existing active drives open replacement backing files first, refresh the matching virtio-block MMIO handler, update stored drive configuration after handler success, and preserve previous state on failure. Configured rate limiters remain parser-level unsupported until rate limiting exists. |
| `DELETE /drives/{drive_id}`, `DELETE /pmem/{id}`, `DELETE /network-interfaces/{iface_id}` | recognized bodyless hot-unplug; `400` `fault_message` | recognized bodyless hot-unplug; `400` `fault_message` | Bodyless requests route through one `HotUnplugDevice` VMM action. Pre-boot requests return the normal unsupported-state fault, and runtime requests return the matching device-specific unsupported fault without mutating stored configuration. Requests with a body fail first as malformed request shape before hot-unplug routing. Real hot-unplug remains deferred. |
| `PUT /network-interfaces/{iface_id}` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Records up to 16 validated pre-boot configs without opening host networking resources. Startup preparation attaches configured interfaces as virtio-mmio devices in the MMIO dispatcher and guest FDT. `InstanceStart` revalidates the count before selecting packet I/O. If every configured interface is listed in MMDS config, startup validates supported `host_dev_name` syntax and uses process-local MMDS-only packet I/O without opening vmnet resources; otherwise it opens vmnet packet I/O for `vmnet:host`, `vmnet:shared`, and `vmnet:bridged:<interface>` host device names. Unsupported names fail before `Running`. Internal network notification dispatch can route each interface through selected packet I/O, complete TX descriptor heads through a packet sink boundary, and write injected RX packets into guest buffers through a packet source boundary. Runtime PATCH accepts a no-op subset for existing interfaces. Public packet movement, real runtime mutation, configured rate limiting, and DELETE remain deferred. |
| `PATCH /network-interfaces/{iface_id}` | recognized post-boot-only operation; `400` `fault_message` | no-op subset supported; `204` empty response when limiters are omitted or `null` | Parses Firecracker-shaped update requests, rejects malformed or mismatched bodies first, returns unsupported-state before startup, validates that the target interface already exists after startup, and accepts omitted or `null` rate limiters as a no-op without mutating stored configuration. Configured rate limiters and real post-boot network mutation remain deferred. |
| `PUT /vsock` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Records validated pre-boot config without opening host Unix socket resources during the request. Startup preparation binds the configured `uds_path` as a nonblocking host Unix listener and attaches one configured virtio-vsock device as guest-visible FDT/MMIO metadata backed by the internal MMIO handler, which retains active RX, TX, and event queue metadata after `DRIVER_OK`. The runtime has an internal TX descriptor packet parser, TX available-ring drain helper, used-ring TX descriptor completion, host socket accept helper for one owned nonblocking stream per dispatch pass, bounded accepted-stream retention across partial handshakes and retained connection records, accepted-stream `CONNECT <PORT>` handshake reader, host local port allocator, retained host connection table model with one-shot host request packet headers, RX packet-header delivery into writable guest descriptors with late retry for pending host requests, guest responses, reset packets, credit updates, or host-to-guest `RW` payloads, guest `RESPONSE` acknowledgement to retained host streams, guest `RST` and guest `SHUTDOWN` partial-state/full-cleanup for retained host-initiated and guest-initiated connections, minimal guest `CREDIT_UPDATE` consumption and `CREDIT_REQUEST` responses with guest-visible `CREDIT_UPDATE` headers for established retained streams, bounded zero-payload `VSOCK_OP_RST` queueing for unsupported or orphan host-destined guest TX packets, guest `REQUEST` connection to Firecracker-shaped `${uds_path}_${PORT}` sockets with guest-visible `RESPONSE` or `RST` header delivery, bounded guest `RW` payload forwarding from established host-initiated or guest-initiated connections to retained host streams with bounded four-packet per-connection guest-to-host retry buffering, bounded four-packet per-connection host `RW` backlog delivery from established host-initiated or guest-initiated streams into guest RX buffers, handler-level and startup-level RX/TX notification dispatch, no-op event notification handling, and HVF boot-loop vsock queue interrupt signaling, but beyond connection setup, forceful guest reset/full-shutdown cleanup, and minimal partial shutdown state, guest-visible socket lifecycle remains deferred; Firecracker's full graceful-shutdown timeout/kill-queue behavior, full virtio-vsock credit accounting, CID routing beyond current host/guest checks, full event payload dispatch, PATCH, and DELETE remain deferred. |
| `GET /mmds` | implemented; `200` JSON | implemented; `200` JSON | Returns the current process-local MMDS JSON object, or JSON `null` before `PUT /mmds` initializes the data store. Initialized data is also used by the implemented guest-visible MMDS path when MMDS config selects startup network interfaces; guest-visible queries still fail if the store is uninitialized. Packet handling remains limited to the documented internal vmnet detour boundary. |
| `PUT /mmds` | implemented; `204` empty response on successful data storage | implemented; `204` empty response | Stores a JSON object in the process runtime using the effective MMDS data-store limit. Oversized data is rejected without replacing the previous value. |
| `PATCH /mmds` | implemented after data initialization; `204` empty response | implemented after data initialization; `204` empty response | Applies RFC 7396 merge-patch semantics to the stored JSON object using the effective MMDS data-store limit. Oversized patched results are rejected without mutating the previous value. |
| `PUT /mmds/config` | implemented; `204` empty response on successful config storage | unsupported after start; `400` `fault_message` | Stores control-plane MMDS config before startup after runtime validation rejects empty interface lists and validates that each listed interface ID already exists in the configured network interface set. At startup, the configured interfaces can enable the implemented guest-visible MMDS packet path; runtime MMDS config updates and public packet movement remain deferred. |
| `PUT /metrics` | implemented; `204` empty response on successful output initialization | unsupported after start; `400` `fault_message` | Metrics output is process observability state, not guest configuration. Duplicate initialization fails. Malformed parser failures for this endpoint increment the selected metrics request count/fail counters without replacing the configured sink. When configured, startup CLI/config-file metrics paths can write one initial metrics line, and runtime `FlushMetrics` plus periodic runtime flushes every 60 seconds while the VM is running append later minimal metrics lines. |
| `PUT /logger` | implemented; `204` empty response on successful pre-boot configuration | unsupported after start; `400` `fault_message` | Logger output is process observability state, not guest configuration. Repeated pre-boot requests update provided fields; malformed parser failures increment the selected logger request count/fail counters without mutating logger state; minimal successful action logging supports configured level, origin, and module-prefix fields, but full log routing remains deferred. |
| `PUT /serial` | implemented; `204` empty response on successful pre-boot output configuration or clear request | unsupported after start; `400` `fault_message` | Serial output is process observability state, not guest configuration. Valid `serial_out_path` values are stored without opening host resources during the request; startup opens the path and routes guest TX serial bytes to it. Malformed parser failures increment the selected serial request count/fail counters without mutating previous serial output configuration; empty/control-character paths and configured rate limiters are also rejected without mutating it. |
| `PUT /entropy` | implemented; `204` empty response on successful no-rate-limiter configuration | unsupported after start; `400` `fault_message` | Stores the supported empty virtio-rng configuration before startup. `GET /vm/config` then includes `"entropy": {}`, and `InstanceStart` attaches the existing HVF virtio-rng MMIO/FDT device backed by the session-owned host OS randomness source. Configured rate limiters are rejected without mutating previous entropy configuration. |
| `PUT /balloon` | partial; `204` empty response on successful pre-boot configuration | unsupported after start; `400` `fault_message` | Stores Firecracker-shaped balloon configuration before startup, rejects targets larger than configured guest memory without mutating previous balloon config, exposes it through `GET /balloon` and `GET /vm/config`, and can attach the current virtio-mmio/FDT shell during `InstanceStart`. Runtime `PATCH /balloon` can update the target size after startup when a balloon was configured, runtime `PATCH /balloon/statistics` can update nonzero statistics polling intervals without toggling statistics enabled state, `GET /balloon/statistics` can return required target and actual fields from stored target state plus internal inflated-page accounting and optional guest-reported fields from statistics queue reports, and hinting start/stop/status can update and report host-owned command state plus 4-byte guest command acknowledgements when `free_page_hinting` is enabled. Runtime-handler and HVF boot-loop inflate/deflate/statistics descriptor dispatch is implemented with queue interrupt signaling and internal inflated-page accounting for mapped guest PFNs; active-run hinting range descriptors are validated and recorded, while periodic statistics polling, timer-driven statistics descriptor completion, reporting, and host reclaim remain deferred. |
| `GET /balloon/hinting/status` | post-boot-only unsupported-state fault; `400` `fault_message` | partial; `200` JSON with `free_page_hinting: true`, otherwise `400` `fault_message` | Requires a configured balloon with free-page hinting enabled and returns the active host command and guest command state. Start/stop commands update `host_cmd`; a 4-byte hinting queue descriptor updates `guest_cmd`, which remains `null` until the guest sends one. Guest `STOP(0)` and unexpected guest `DONE(1)` descriptors complete the current hinting run and, when the active run was started with `acknowledge_on_stop=true`, update `host_cmd` to `DONE(1)` through the same config-space/config-interrupt path as explicit stop. Active-run range descriptors are validated and recorded internally; host reclaim remains deferred. |
| `PATCH /balloon/hinting/start`, `PATCH /balloon/hinting/stop` | post-boot-only unsupported-state fault; `400` `fault_message` | partial; `204` with `free_page_hinting: true`, otherwise `400` `fault_message` | Start advances the host command id, skips Firecracker reserved command values, updates active config space, raises a config interrupt, and preserves `acknowledge_on_stop` in host-owned state. Stop writes Firecracker's done command, updates active config space, and raises a config interrupt. Hinting queue command acknowledgements can update `guest_cmd`, completed guest `STOP(0)`/`DONE(1)` commands automatically write host `DONE(1)` when `acknowledge_on_stop` is enabled, and active-run range descriptors are validated and recorded internally. Host reclaim remains deferred. |
| `PUT /actions` with `InstanceStart` | process-routed; `204` after successful owned HVF startup with internal boot run-loop worker across bounded step windows or `400` preflight/preparation/logger-output fault | unsupported after start; `400` `fault_message` | Commits `Running` only after the owned HVF boot-session worker with configured serial output or default internal serial capture is retained and any emitted action logging succeeds. The worker keeps internal active, terminal-outcome, or error status; guest PSCI `SYSTEM_OFF` or `SYSTEM_RESET` can terminate the owning process successfully. Public run-loop control, reboot-in-place, error exit-code parity, and public serial streaming remain deferred. |
| `PUT /actions` with `FlushMetrics` | VMM-routed; `400` unsupported-state `fault_message` | implemented; `204` empty response or `400` logger/metrics output fault | Firecracker treats this as runtime-only. bangbang writes one minimal JSON line when metrics output was configured, including selected API request counters, `logger.missed_metrics_count` after a previous metrics write failure, `logger.missed_log_count` after a previous logger action write failure, and `boot_run_loop_status` as `running`, `exited`, or `failed` when a process-owned boot worker exists, writes one minimal action line when logger output is configured and allowed by level/module filters, and otherwise succeeds without writing. Periodic metrics flushes reuse the same metrics payload every 60 seconds while running, but are not `/actions` requests and do not write logger action lines. |
| `PUT /actions` with `SendCtrlAltDel` | intentionally unsupported; parser returns `400` `fault_message` | intentionally unsupported; `400` `fault_message` | Firecracker rejects this on aarch64; bangbang's first target is Apple Silicon. The request contributes to `put_api_requests.actions_count` but not `actions_fails`. |
| Non-initial endpoints from the endpoint matrix | `400` `fault_message` until their capability exists | `400` `fault_message` until their capability exists | Covers planned later and deferred endpoints; a later capability PR may define more specific state behavior. |
| Unknown endpoint or invalid method/path | `400` `fault_message` | `400` `fault_message` | Matches Firecracker's parser-level invalid path or method handling. |

### Response Policy

| Case | HTTP status | Body policy |
| --- | --- | --- |
| Successful data response | `200 OK` | JSON body with Firecracker-shaped field names. |
| Successful empty response | `204 No Content` | Empty body. |
| Invalid path, invalid method, invalid JSON, unknown field, invalid field, unsupported endpoint, or unsupported state | `400 Bad Request` | JSON object with `fault_message`. |
| Startup, configuration, or VMM action failure | `400 Bad Request` | JSON object with `fault_message`; exact strings can be refined with the implementation. |
| MMDS payload-limit failure | `400 Bad Request` | Uses the repository-wide payload limit error envelope until broader HTTP status expansion defines Firecracker's `413 Payload Too Large` behavior. |

Future API work should use `fault_message` consistently where Firecracker does.
Exact message strings should be covered by golden tests once the API parser and
VMM action model exist, but this document only defines the initial status/body
shape.

The initial API implementation uses Firecracker's default `51200` byte HTTP
request payload limit unless `--http-api-max-payload-size <BYTES>` configures a
different per-process API socket request limit.
The MMDS data store uses the effective `--mmds-size-limit <BYTES>` value as its
serialized JSON limit. When that argument is omitted, the limit follows the
effective HTTP API payload limit like Firecracker; with default HTTP settings
this remains `51200` bytes. Startup `--metadata <PATH>` initializes the same
data store before API serving or no-api readiness and is subject to the same
serialized JSON limit after its file is parsed.
Internal MMDS guest GET response modeling checks the configured MMDS v2 token
requirement before reading metadata. Once a request is permitted to read
metadata, it follows the same uninitialized data policy: before `PUT /mmds`, it
returns a process-local `400` plain-text error value rather than a successful
response. Process-local guest request parsing currently accepts complete
HTTP/1.0 or HTTP/1.1 `GET` request buffers and
`PUT /latest/api/token` token request buffers, rejects request bodies and
transfer encodings, maps GET `Accept: application/json` to JSON output, and
defaults missing, empty, wildcard, or `text/plain` GET `Accept` headers to IMDS
text output. The runtime can also convert complete process-local guest HTTP
request buffers into deterministic response bytes, mapping unsupported methods
to `405 Method Not Allowed` with the current `Allow: GET, PUT` header and other
parse failures to `400 Bad Request` plain-text responses without echoing
malformed request bytes. The runtime also has a process-local opaque token
authority with Firecracker-compatible TTL bounds of `1..=21600` seconds and a
default `1024`-entry active-token store. Process-local guest token
`PUT /latest/api/token` handling requires either
`X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds`,
rejects `X-Forwarded-For`, and returns a plain-text token response with the
accepted TTL header. When configured for MMDS v2, process-local guest GET
handling requires exactly one valid `X-metadata-token` or
`X-aws-ec2-metadata-token` value generated by the token authority; missing,
duplicate, unknown, or expired tokens return `401 Unauthorized`.
The runtime can also classify ARP requests for the configured MMDS IPv4 address
and raw Ethernet/IPv4/TCP guest packet bytes as MMDS candidates only when the
IPv4 destination matches the configured MMDS address and the TCP destination
port is `80`. Truncated, malformed, non-IPv4, non-TCP, fragmented, and non-MMDS
packets are treated as non-candidates without exposing metadata. For pure
empty-payload candidate TCP SYN packets, the runtime can synthesize
deterministic SYN-ACK frames, identify pure empty-payload ACK-only packets that
acknowledge that deterministic SYN-ACK, FIN close, guest packets carrying RST,
and unsupported control packets, synthesize ACK plus FIN-ACK frames for empty FIN
close packets, synthesize minimal RST frames for unsupported empty controls,
consume guest RST packets without response even when they also carry payload
bytes, and for non-empty candidate TCP payloads that acknowledge that
deterministic SYN-ACK and do not carry unsupported SYN or FIN payload control
flags, it can also produce the same process-local HTTP response bytes as the
existing guest HTTP helper, including token PUT and MMDS v2 GET token
enforcement. Non-empty candidates carrying SYN or FIN are not interpreted as
process-local MMDS HTTP requests. The process vmnet packet I/O path detours MMDS
ARP requests, pure empty-payload MMDS SYN packets, pure empty-payload MMDS
ACK-only packets that acknowledge bangbang's deterministic SYN-ACK, pure
empty-payload MMDS FIN close packets, guest packets carrying RST, unsupported
empty control packets, and non-empty MMDS candidate TX payloads on configured
MMDS interfaces when they acknowledge bangbang's deterministic SYN-ACK and do
not carry unsupported SYN or FIN payload control flags, keeps shared MMDS state
visible to both API and packet paths,
buffers split request headers only when each fragment starts at the next
expected TCP sequence number, rejects non-contiguous buffered fragments without
forwarding them to vmnet,
synthesizes deterministic Ethernet/ARP replies, Ethernet/IPv4/TCP SYN-ACK
frames, minimal Ethernet/IPv4/TCP FIN close frames, minimal Ethernet/IPv4/TCP
RST frames, and Ethernet/IPv4/TCP response frames carrying the generated HTTP
response bytes, retains those frames in bounded per-interface queues, exposes
queued frames through the matching
virtio-net RX source before vmnet reads, prioritizes ARP replies before queued
TCP responses, and schedules one bounded post-TX RX retry when that source
reports a queued response. When every configured network interface is selected
by MMDS config, process startup can instead build process-local MMDS-only packet
I/O that reuses the same detour and response-queue logic, drops non-MMDS TX
frames, and serves queued MMDS responses without opening vmnet. Full ARP cache
management, gratuitous ARP, ARP
timeout/retry policy, broader ACK-number validation beyond the narrow ACK-only
and non-empty payload SYN-ACK acknowledgement paths, full TCP stream tracking,
out-of-order reassembly, retransmission policy, stateful RST policy, session
timeout policy, and stricter per-interface TCP session ownership remain
deferred.
Process-local guest response-byte serialization preserves accepted `HTTP/1.0`
or `HTTP/1.1` request versions in response status lines. Malformed request
lines and unsupported versions use the existing safe parse-error response path
without echoing arbitrary version tokens.
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

- packet networking beyond pre-boot `network-interfaces` configuration storage,
  internal virtio-net config-space, activation, TX frame parser, RX buffer
  parser, prepared device resources, MMIO registration, startup FDT metadata,
  TX/RX notification dispatch metadata helpers, and startup-time vmnet packet
  I/O selection for supported `host_dev_name` forms
- virtio-vsock socket lifecycle beyond connection setup, forceful guest
  reset/full-shutdown cleanup, current narrow signed guest-initiated and
  host-initiated EOF cleanup, and signed guest-initiated and host-initiated
  multi-payload exchanges, plus signed guest-initiated and host-initiated
  multi-stream exchanges; Firecracker's full graceful-shutdown timeout/kill-queue
  behavior, full virtio-vsock credit accounting, CID routing beyond current
  host/guest checks, broader socket lifecycle parity, and full event payload
  dispatch.
  Implemented vsock scope is limited
  to no-op event notifications, pre-boot `/vsock` configuration storage,
  startup FDT attachment,
  startup host listener ownership, host socket accept into one owned nonblocking stream per call, accepted-stream `CONNECT <PORT>` handshake reading, host
  local port allocation, retained host connection table modeling with pending host request packet headers, guest `RESPONSE` acknowledgement to retained host streams, guest `RST` and guest `SHUTDOWN` partial-state/full-cleanup for retained host-initiated and guest-initiated connections, minimal guest `CREDIT_UPDATE` consumption and `CREDIT_REQUEST` responses with guest-visible `CREDIT_UPDATE` headers for established retained streams, bounded guest-visible `RST` queueing for unsupported or orphan host-destined guest packets, guest `REQUEST` connection to Firecracker-shaped `${uds_path}_${PORT}` sockets with guest-visible `RESPONSE` or `RST` header delivery, bounded guest `RW` payload forwarding from established host-initiated or guest-initiated connections to retained host streams with bounded four-packet per-connection guest-to-host retry buffering, bounded four-packet per-connection host `RW` backlog delivery from established host-initiated or guest-initiated streams into guest RX buffers, and the internal prepared resource/MMIO
  registration/config-space/MMIO handler skeleton with active queue metadata retention plus packet header model, TX
  descriptor packet parser, TX available-ring drain helper with used-ring
  descriptor completion, RX request-header delivery through queue notifications,
  handler-level and startup-level RX/TX notification dispatch, and boot-loop
  queue interrupt signaling
- snapshots
- full MMDS TCP routing, stream reassembly, and retransmission policy
- full balloon statistics polling, reporting, and host reclaim
- pmem root-device semantics, runtime updates, rate limiting, dirty-range
  tracking, direct file-backed HVF mapping, and hot-unplug beyond startup
  backing-file validation/mapping, guest-visible MMIO/FDT attachment, HVF
  guest-memory registration, the internal guest-range config-space foundation,
  and queue-driven flush writeback
- entropy rate limiting, runtime metrics, and security docs
- serial input, serial rate limiting, public serial streaming, and serial metrics
- full logger integration, full Firecracker metrics counters beyond the minimal
  `logger.missed_metrics_count` and `logger.missed_log_count`
- memory hotplug
- real pause and resume VM state transitions
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
  dispatches boot block and virtio-net notifications between successful MMIO
  steps and asserts the EL1 virtual timer PPI after virtual timer exits.
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
  sessions can now use that path for block queue interrupts and virtio-net
  network queue interrupts, while device interrupt masking, timer EOI/deactivation-driven
  unmasking, runner-loop interrupt delivery beyond the current internal
  block/network/timer paths, and public device wiring still need
  macOS-specific backend work.
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
  `scripts/run-integration-tests.sh`, which signs the selected HVF test
  binaries or executable e2e artifacts with the
  `com.apple.security.hypervisor` entitlement before running them; the script
  prepares the pinned Firecracker kernel plus generated tiny initrd for guest
  boot and executable HVF e2e tests, and fails when the host cannot run HVF
  tests unless CI explicitly uses `--allow-unsupported` after build/sign
  validation

Changes that alter support status or validation coverage should also update
[Firecracker Validation Matrix](firecracker-validation-matrix.md).

## Security and Performance Scope

Security review should cover host paths, Unix sockets, FFI boundaries, guest
memory, device I/O, and untrusted API or guest input as those surfaces are
introduced. Performance review should cover startup path, memory mapping, vCPU
run loops, and device I/O when those areas change.

Detailed security and performance analysis belongs with the capability work that
introduces or changes the relevant surface.
