# bangbang

bangbang is a Rust VMM project for macOS hosts. The public control plane is intended to stay compatible with the Firecracker HTTP API over a Unix domain socket, while the VM backend is built on Apple's Hypervisor.framework.

This repository is currently a scaffold. It defines crate boundaries, an initial Firecracker-compatible API socket, machine-configuration API storage, boot-source API storage, a drive configuration path, actions request routing through the VMM action boundary, process-owned `InstanceStart` boot run-loop continuation across bounded step windows, a process startup CLI, a minimal internal VMM action model with `InstanceStart` preflight, transactional startup executor, and successful-start state transition helpers, a backend trait, a backend-neutral guest address/layout model, anonymous guest memory allocation and byte access, arm64 boot placement helpers, internal boot-source validation with arm64 kernel/initrd payload loading, an internal Firecracker-shaped drive configuration validation model, a host-file backing access layer, internal configured block-device preparation and MMIO registration helpers, an internal TX-only serial MMIO output device model with shared bounded capture support, an internal virtio-block config-space capacity model, an internal virtio-block request parser, single-request executor, queue dispatcher, MMIO queue-state bridge, activation state, notification/interrupt-status dispatch helper, and boot-runtime block notification dispatch, minimal arm64 FDT generation with optional serial and virtio-mmio device-node descriptors and guest-memory writes, internal boot-resource assembly from stored VM configuration with optional serial and block MMIO registration, internal MMIO region/operation/handler-dispatch groundwork, internal interrupt line/status/trigger groundwork, internal virtio-mmio register/access decoding, feature/status, queue, queue notification, and interrupt status/acknowledgement register state plus a composed register handler with notification drain, device-configuration delegation, and reset-aware `DRIVER_OK` activation-hook helpers, virtqueue descriptor-chain, available-ring read, and used-ring write groundwork, and the smallest Hypervisor.framework VM, GIC boot metadata, SPI interrupt-line allocation and signaling, GIC PPI pending control, guest memory map/unmap ownership with controlled mapped-memory access, current-thread vCPU lifecycle, typed exit surface with MMIO data-abort decoding, registry resolution, and vCPU exit classification, register and virtual-timer-mask wrappers, single resolved HVF MMIO exit dispatch/completion through runtime handlers, explicit runner-thread primary MPIDR affinity/metadata, virtual-timer-mask, GIC PPI pending, and MMIO handling commands, single-vCPU arm64 boot-register setup, internal HVF single-vCPU arm64 boot-session preparation with a runner-compatible shared MMIO dispatcher, one-step runner-thread MMIO handling, a cloneable run-cancellation handle, a bounded internal boot-session run-loop pump, owned internal boot-session handle, process-owned serial MMIO capture wiring, process-owned boot run-loop supervision across bounded step windows with retained internal worker status, boot block notification dispatch, SPI interrupt signaling, and virtual timer PPI assertion, and cancellable vCPU runner skeleton.

See [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) for the intended compatibility target and current limitations.
See [macOS Host Security Model](docs/security.md) for the current host trust boundary and security limitations.
See [Pull Request Review Guidelines](docs/review-guidelines.md) for the project-specific review standard.

## Layout

```text
crates/api        Firecracker-compatible API request and response surface
crates/runtime    Backend-neutral VM trait and error type
crates/hvf        Hypervisor.framework backend skeleton
crates/bangbang   VMM process entrypoint and startup CLI
```

## Current Scope

The first target is Apple Silicon macOS. The current scaffold includes HTTP over a Unix domain socket for `GET /`, `GET /version`, `GET /vm/config`, `GET /machine-config`, pre-boot `PUT /boot-source` configuration storage, pre-boot `PUT /machine-config` configuration storage, pre-boot `PUT /drives/{drive_id}` configuration storage, pre-boot `PUT /metrics` output configuration, pre-boot `PUT /logger` output configuration, and process-owned `PUT /actions` startup with an internal boot run-loop worker across bounded step windows. The implemented read endpoints, configuration storage paths, and parsed actions route through a minimal VMM action model. Machine-configuration requests are parsed, validated, stored as VM configuration state, and returned by `GET /machine-config` and `GET /vm/config`; those values are applied only when `InstanceStart` successfully starts the owned HVF startup path. Boot-source requests are parsed, validated, recorded as VM configuration state, and returned by `GET /vm/config`; kernel/initrd files are opened and loaded only during `InstanceStart`. Drive requests are parsed, validated, recorded as VM configuration state, and returned by `GET /vm/config`; configured block backing files are opened only during `InstanceStart`. Metrics requests are parsed, validated, opened as per-process output state, and intentionally omitted from `GET /vm/config` because metrics are not guest configuration. Logger requests are parsed, validated, opened as per-process output state when `log_path` is provided, and intentionally omitted from `GET /vm/config` because logger settings are process observability state. Actions requests for `InstanceStart` and `FlushMetrics` are parsed with Firecracker-shaped bodies and routed through the process VMM owner. `InstanceStart` validates stored boot-source and state preflight, attempts owned HVF arm64 boot-session preparation with an internal serial MMIO console and process-owned bounded capture buffer, starts a process-owned internal boot run-loop worker across bounded step windows only on success, retains internal active, terminal-outcome, or error worker status, and marks the instance `Running` only after that worker handle is retained; preparation or worker-start failures leave the instance `Not started`. `FlushMetrics` is rejected before startup and returns `204 No Content` after startup, writing one minimal JSON metrics line when metrics output is configured and succeeding as a no-op when it is not configured; `SendCtrlAltDel` is rejected as unsupported on aarch64. Separate runtime helpers can prepare owned internal block-device resources from validated stored drive configs by opening their backing files, deriving config space, and constructing inactive virtio-block device state, then register those prepared resources as deterministic virtio-mmio regions and handlers in a fresh internal MMIO dispatcher. A runtime-internal boot-resource assembler can combine stored machine, boot-source, and drive configuration with caller-provided backend boot metadata to allocate guest memory, load the kernel/initrd, register block MMIO devices, optionally register serial MMIO, and write the arm64 FDT; public `InstanceStart` now invokes this through the owned HVF startup path with a default internal serial MMIO console and process-owned internal boot run-loop worker across bounded step windows plus retained internal worker status, but still does not provide public run-loop control or boot-smoke guarantee. The runtime crate defines guest physical address, range, and aarch64 DRAM layout primitives, can allocate owned anonymous host memory for validated page-aligned guest memory layouts, can safely read/write byte slices by guest address, exposes the first arm64 kernel/FDT/initrd placement helpers, can internally validate a Firecracker-shaped boot source before loading a supported arm64 Linux `Image` kernel plus optional non-empty initrd into guest memory, can internally validate and normalize a Firecracker-shaped drive configuration subset, can access regular host-file backing with bounded positioned reads/writes and flushes, can prepare owned internal virtio-block device resources from stored drive configs and consume prepared resources into MMIO-dispatchable virtio-mmio block handlers, can expose an internal TX-only serial MMIO output device that captures byte writes through isolated or shared bounded sinks without global state, can expose an internal virtio-block config-space capacity model with read-only feature bits, can parse internal virtio-block request descriptor chains, execute one parsed request with Firecracker-shaped status/completion metadata, build an internal virtio-block queue from ready virtio-mmio queue metadata, hold that queue in resettable internal block activation state, drain that queue into used-ring completions with queue-interrupt intent, dispatch drained queue 0 notifications through the active internal block queue while marking the virtio-mmio queue interrupt status bit when completions need an interrupt, and dispatch pending boot block-device notifications from boot runtime resources with per-device metadata for backend interrupt signaling, can build/write a minimal Firecracker-shaped arm64 FDT from loaded boot metadata, backend-neutral interrupt-controller metadata, and optional serial and virtio-mmio device metadata, can register and look up non-overlapping MMIO region ownership for future device dispatch, can represent bounded MMIO read/write operations after lookup, can dispatch those operations to registered internal handlers, can decode checked runtime MMIO operations into typed virtio-mmio register or device-configuration accesses, can model Firecracker-shaped virtio-mmio identity, feature selector, driver-feature, status, queue, queue notification, and interrupt status/acknowledgement register state, can route those register accesses through a composed runtime handler, can drain recorded queue notifications for future device handlers, can delegate device-configuration accesses to an injected backend-neutral handler, can invoke an injected backend-neutral activation hook when `DRIVER_OK` is accepted and reset it on virtio-mmio reset, can read and validate backend-neutral virtqueue descriptor chains from guest memory, can pop one available-ring head into a parsed descriptor chain, can publish one used-ring completion element, can model backend-neutral device interrupt lines, pending status bits, acknowledgements, and trigger signaling through an injected sink, and can own per-process minimal metrics and logger output state without global mutable observability sharing. The HVF crate can create/destroy a process VM, create macOS 15+ GIC v3 boot metadata without MSI/ITS, allocate deterministic guest interrupt lines from the validated GIC SPI range, signal validated SPI lines through Hypervisor.framework, set and clear validated GIC PPI pending bits through redistributor registers, convert GIC metadata for the runtime FDT path, map/unmap allocated guest memory with backend-owned cleanup and controlled mapped-memory access, create/destroy one current-thread vCPU handle, define typed HVF exit snapshots, decode candidate MMIO data-abort exits into access metadata, resolve decoded accesses against the internal MMIO registry into owner/offset metadata, classify whole vCPU exits into MMIO, virtual-timer, canceled, or unknown events, build runtime MMIO operations from resolved HVF exits, complete MMIO read exits back into guest GPRs, dispatch a single resolved HVF MMIO access through runtime handlers and complete read results, get/set a narrow set of vCPU registers including MPIDR_EL1 and the virtual timer mask, configure the primary arm64 Linux boot-register state for one vCPU, start a thread-owned runner that sets deterministic primary MPIDR_EL1 affinity, can read MPIDR_EL1 metadata, get/set the virtual timer mask, set/clear GIC PPI pending bits, apply boot-register setup, explicitly dispatch one resolved MMIO access, run once and handle a resulting MMIO exit on the vCPU-owning thread, cancel a single `hv_vcpu_run` step through a cloneable handle, and prepare an internal single-vCPU arm64 boot session that creates VM/GIC state, reads the primary MPIDR, allocates deterministic block and optional serial SPI lines, assembles boot resources, owns a runner-compatible shared MMIO dispatcher, maps guest memory, exposes controlled mapped-memory borrows, configures primary boot registers, can run one boot-session vCPU step with runner-thread MMIO handling, can expose a run-cancellation handle, can run a bounded internal loop that dispatches block notifications between successful MMIO steps and asserts the EL1 virtual timer PPI after virtual timer exits, can own that prepared session as a storable internal HVF handle for process startup wiring with optional serial capture, and can dispatch boot block queue notifications against mapped guest memory while signaling needed block SPI interrupts. It intentionally does not include:

- successful API endpoints beyond `GET /`, `GET /version`, `GET /vm/config`, `GET /machine-config`, pre-boot `PUT /machine-config` configuration storage, pre-boot `PUT /boot-source` configuration storage, pre-boot `PUT /drives/{drive_id}` configuration storage, pre-boot `PUT /metrics` output configuration, pre-boot `PUT /logger` output configuration, owned `InstanceStart` startup with an internal boot run loop across bounded step windows, and runtime `FlushMetrics`
- successful public `/actions` behavior beyond owned `InstanceStart` startup with an internal boot run loop across bounded step windows and runtime `FlushMetrics`
- public `/boot-source` behavior beyond recording valid boot-source configuration or public block-device behavior beyond recording valid `/drives/{drive_id}` configuration
- public command-line or FDT configuration behavior
- complete interrupt delivery, including timer EOI/deactivation-driven unmasking, public configured guest execution beyond internal startup execution across bounded step windows, public run-loop control, HVF runner-loop notification scheduling, public serial output streaming, serial/backend interrupt wiring beyond the internal boot block notification and retained serial capture paths, device-backed feature negotiation, indirect descriptors, device-backed MMIO loops, complete device emulation, full Firecracker metrics counters, periodic metrics flush, full logger integration, multi-vCPU setup, or PSCI behavior

## Process CLI

The `bangbang` executable accepts the first process-lifecycle arguments and starts the API socket server:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

- `--api-sock <PATH>` sets the Unix socket path for the API server. The default is `/tmp/bangbang.socket`.
- `--id <ID>` records the microVM identifier. IDs must be 1 to 64 bytes and contain only ASCII alphanumeric characters or `-`. The default is `anonymous-instance`.
- `--help`, `-h`, `--version`, and `-V` are supported.

`bangbang` binds the configured socket path, serves `GET /`, `GET /version`, `GET /vm/config`, `GET /machine-config`, pre-boot `PUT /machine-config` configuration storage, pre-boot `PUT /boot-source` configuration storage, pre-boot `PUT /drives/{drive_id}` configuration storage, pre-boot `PUT /metrics` output configuration, pre-boot `PUT /logger` output configuration, and process-owned `PUT /actions` startup/metrics actions, and stays running until `SIGINT` or `SIGTERM` requests shutdown. Unsupported Firecracker process options such as `--config-file`, `--no-api`, seccomp, logging and metrics CLI flags, snapshot, MMDS, and PCI flags are rejected instead of ignored.

The API socket is an unauthenticated local control interface. Filesystem
permissions on the socket path and parent directory are the access-control
boundary, so use a private directory or restrictive umask on multi-user hosts.

Query the supported read-only endpoints:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/
```

The instance info response is Firecracker-shaped JSON:

```json
{"app_name":"bangbang","id":"demo-1","state":"Not started","vmm_version":"0.1.0"}
```

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/version
```

The version response body is Firecracker-shaped JSON:

```json
{"firecracker_version":"0.1.0"}
```

Query the current machine configuration:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/machine-config
```

The default machine configuration response is Firecracker-shaped JSON:

```json
{"huge_pages":"None","mem_size_mib":128,"smt":false,"track_dirty_pages":false,"vcpu_count":1}
```

Query the accumulated VM configuration:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/vm/config
```

The full configuration response currently includes the supported subset only:

```json
{"drives":[],"machine-config":{"huge_pages":"None","mem_size_mib":128,"smt":false,"track_dirty_pages":false,"vcpu_count":1}}
```

`boot-source` is included after it is configured. Metrics and logger output
configuration are omitted because they are process observability state, not
guest configuration. Unsupported sections such as network, vsock, and snapshots
are omitted until their models exist.

Record a pre-boot machine configuration:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/machine-config \
  -H 'Content-Type: application/json' \
  -d '{"vcpu_count":2,"mem_size_mib":256}'
```

Successful machine configuration returns `204 No Content`. The values are
stored as configuration only; bangbang does not allocate guest memory or create
vCPUs from them yet.

Record a pre-boot boot source:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/boot-source \
  -H 'Content-Type: application/json' \
  -d '{"kernel_image_path":"/tmp/vmlinux","boot_args":"console=ttyS0 reboot=k panic=1"}'
```

Successful boot-source configuration returns `204 No Content`. During this
request, the paths are stored as configuration only; `InstanceStart` later opens
the files and performs startup.

Record a pre-boot drive configuration:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/drives/rootfs \
  -H 'Content-Type: application/json' \
  -d '{"drive_id":"rootfs","path_on_host":"/tmp/rootfs.ext4","is_root_device":true}'
```

Successful drive configuration returns `204 No Content`. During this request,
the path is stored as configuration only; `InstanceStart` later opens the file
and prepares the initial block device.

Configure metrics output before startup:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/metrics \
  -H 'Content-Type: application/json' \
  -d '{"metrics_path":"/tmp/bangbang.metrics"}'
```

Successful metrics configuration returns `204 No Content` and opens the output
path as per-process observability state. It is not included in `GET /vm/config`.
Duplicate configuration returns `400 Bad Request`.

Configure logger output before startup:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/logger \
  -H 'Content-Type: application/json' \
  -d '{"log_path":"/tmp/bangbang.log","level":"Warning","show_level":true}'
```

Successful logger configuration returns `204 No Content`. All fields are
optional; `Warning` is accepted as `Warn`, and repeated pre-boot requests update
only the fields they include. When `log_path` is provided, bangbang opens the
path as per-process logger state with nonblocking file/FIFO semantics. The
logger configuration is not included in `GET /vm/config`, and full internal log
routing through this sink remains deferred.

Submit an action request:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/actions \
  -H 'Content-Type: application/json' \
  -d '{"action_type":"InstanceStart"}'
```

The request body is parsed and routed to the process VMM owner. `InstanceStart`
validates stored boot-source and state preflight first. Without a stored boot
source, it returns a missing boot-source fault before attempting startup. With
valid pre-boot configuration, it attempts owned HVF arm64 boot-session
preparation with an internal serial MMIO console and bounded capture buffer,
starts a process-owned internal boot run-loop worker across bounded step windows with retained internal active, terminal-outcome, or error status, then returns `204
No Content` only after the worker handle is retained and the instance state
becomes `Running`. Preparation or worker-start failures return `400 Bad
Request` with a `fault_message` body and leave the instance `Not started`.
Public run-loop control, boot smoke output, pause/resume, public
serial streaming, and public runner loop scheduling remain deferred.

After startup, flush the configured metrics output:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/actions \
  -H 'Content-Type: application/json' \
  -d '{"action_type":"FlushMetrics"}'
```

`FlushMetrics` returns `400 Bad Request` before startup and `204 No Content`
after startup. If `/metrics` was configured, it appends one minimal JSON metrics
line. If metrics were not configured, the runtime action succeeds without
writing output. Full Firecracker metrics counters, periodic flush, full logger
integration, and CLI observability flags remain deferred.

## Exit Status

- `0`: help or version completed successfully, or the API server exited without error.
- `153`: startup argument parsing or validation failed. This matches Firecracker's argument-parsing exit code.
- `1`: non-argument process failure, including API socket bind or accept failures.

## Build

Requires the latest stable Rust toolchain.

```sh
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-targets --all-features --locked --exclude bangbang-hvf
cargo test -p bangbang-hvf --lib --all-features --locked
```

On macOS Apple Silicon hosts, `bangbang-hvf` contains real HVF lifecycle,
GIC creation, guest memory mapping, and runner smoke tests in
`crates/hvf/tests/hvf_lifecycle.rs`. The tests are not ignored; run the signed
test wrapper so host or entitlement failures fail the test run:

```sh
scripts/run-hvf-tests.sh
```

Hosted macOS CI may build and sign the HVF tests without executing them when
Hypervisor.framework is unavailable:

```sh
scripts/run-hvf-tests.sh --allow-unsupported
```

Prepare the pinned Firecracker arm64 Linux kernel artifact used by later guest
boot smoke validation work:

```sh
scripts/fetch-firecracker-kernel.sh
```

The script verifies the pinned SHA-256 before reusing or installing the cached
artifact. By default, it stores the kernel under
`.tmp/guest-artifacts/firecracker-ci/v1.15/aarch64/vmlinux-6.1.155`. Set
`BANGBANG_GUEST_ARTIFACTS_DIR` to use a different cache root. This command only
prepares the kernel artifact; it does not start a guest or run the HVF boot
smoke test.

Run the VMM process skeleton and API server:

```sh
cargo run -p bangbang
```
