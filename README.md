# bangbang

bangbang is a Rust VMM project for macOS hosts. The public control plane is intended to stay compatible with the Firecracker HTTP API over a Unix domain socket, while the VM backend is built on Apple's Hypervisor.framework.

This repository is currently a scaffold. It defines crate boundaries, an initial Firecracker-compatible API socket, machine-configuration API storage, boot-source API storage, a drive configuration path, parser-level actions request support, a process startup CLI, a minimal internal VMM action model, a backend trait, a backend-neutral guest address/layout model, anonymous guest memory allocation and byte access, arm64 boot placement helpers, internal boot-source validation with arm64 kernel/initrd payload loading, an internal Firecracker-shaped drive configuration validation model, a host-file backing access layer, internal configured block-device preparation and MMIO registration helpers, an internal virtio-block config-space capacity model, an internal virtio-block request parser, single-request executor, queue dispatcher, MMIO queue-state bridge, activation state, and notification/interrupt-status dispatch helper, minimal arm64 FDT generation with virtio-mmio device-node descriptors and guest-memory writes, internal MMIO region/operation/handler-dispatch groundwork, internal interrupt line/status/trigger groundwork, internal virtio-mmio register/access decoding, feature/status, queue, queue notification, and interrupt status/acknowledgement register state plus a composed register handler with notification drain, device-configuration delegation, and reset-aware `DRIVER_OK` activation-hook helpers, virtqueue descriptor-chain, available-ring read, and used-ring write groundwork, and the smallest Hypervisor.framework VM, GIC boot metadata, SPI interrupt-line allocation and signaling, guest memory map/unmap ownership, current-thread vCPU lifecycle, typed exit surface with MMIO data-abort decoding, registry resolution, and vCPU exit classification, register wrappers, single resolved HVF MMIO exit dispatch/completion through runtime handlers, explicit runner-thread MMIO handling commands, single-vCPU arm64 boot-register setup, and cancellable vCPU runner skeleton.

See [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) for the intended compatibility target and current limitations.
See [Pull Request Review Guidelines](docs/review-guidelines.md) for the project-specific review standard.

## Layout

```text
crates/api        Firecracker-compatible API request and response surface
crates/runtime    Backend-neutral VM trait and error type
crates/hvf        Hypervisor.framework backend skeleton
crates/bangbang   VMM process entrypoint and startup CLI
```

## Current Scope

The first target is Apple Silicon macOS. The current scaffold includes HTTP over a Unix domain socket for `GET /`, `GET /version`, `GET /machine-config`, pre-boot `PUT /boot-source` configuration storage, pre-boot `PUT /machine-config` configuration storage, pre-boot `PUT /drives/{drive_id}` configuration storage, and parser-level `PUT /actions` request handling. The implemented read endpoints and configuration storage paths route through a minimal VMM action model. Machine-configuration requests are parsed, validated, stored as VM configuration state, and returned by `GET /machine-config`, but the stored values are not applied to guest memory, vCPU creation, or startup yet. Boot-source requests are parsed, validated, and recorded as VM configuration state, but the public API path does not open kernel or initrd paths, load payloads, build an FDT, configure vCPU registers, or start a guest yet. Drive requests are parsed, validated, and recorded as VM configuration state. Actions requests for `InstanceStart` and `FlushMetrics` are parsed with Firecracker-shaped bodies, but the server returns an unsupported fault before any VMM state change; `SendCtrlAltDel` is rejected as unsupported on aarch64. Separate runtime helpers can prepare owned internal block-device resources from validated stored drive configs by opening their backing files, deriving config space, and constructing inactive virtio-block device state, then register those prepared resources as deterministic virtio-mmio regions and handlers in a fresh internal MMIO dispatcher; those helpers are not invoked by the public API path and do not register devices for boot yet. The runtime crate defines guest physical address, range, and aarch64 DRAM layout primitives, can allocate owned anonymous host memory for validated page-aligned guest memory layouts, can safely read/write byte slices by guest address, exposes the first arm64 kernel/FDT/initrd placement helpers, can internally validate a Firecracker-shaped boot source before loading a supported arm64 Linux `Image` kernel plus optional non-empty initrd into guest memory, can internally validate and normalize a Firecracker-shaped drive configuration subset, can access regular host-file backing with bounded positioned reads/writes and flushes, can prepare owned internal virtio-block device resources from stored drive configs and consume prepared resources into MMIO-dispatchable virtio-mmio block handlers, can expose an internal virtio-block config-space capacity model with read-only feature bits, can parse internal virtio-block request descriptor chains, execute one parsed request with Firecracker-shaped status/completion metadata, build an internal virtio-block queue from ready virtio-mmio queue metadata, hold that queue in resettable internal block activation state, drain that queue into used-ring completions with queue-interrupt intent, and dispatch drained queue 0 notifications through the active internal block queue while marking the virtio-mmio queue interrupt status bit when completions need an interrupt, can build/write a minimal Firecracker-shaped arm64 FDT from loaded boot metadata, backend-neutral interrupt-controller metadata, and optional virtio-mmio device metadata, can register and look up non-overlapping MMIO region ownership for future device dispatch, can represent bounded MMIO read/write operations after lookup, can dispatch those operations to registered internal handlers, can decode checked runtime MMIO operations into typed virtio-mmio register or device-configuration accesses, can model Firecracker-shaped virtio-mmio identity, feature selector, driver-feature, status, queue, queue notification, and interrupt status/acknowledgement register state, can route those register accesses through a composed runtime handler, can drain recorded queue notifications for future device handlers, can delegate device-configuration accesses to an injected backend-neutral handler, can invoke an injected backend-neutral activation hook when `DRIVER_OK` is accepted and reset it on virtio-mmio reset, can read and validate backend-neutral virtqueue descriptor chains from guest memory, can pop one available-ring head into a parsed descriptor chain, can publish one used-ring completion element, and can model backend-neutral device interrupt lines, pending status bits, acknowledgements, and trigger signaling through an injected sink. The HVF crate can create/destroy a process VM, create macOS 15+ GIC v3 boot metadata without MSI/ITS, allocate deterministic guest interrupt lines from the validated GIC SPI range, signal validated SPI lines through Hypervisor.framework, convert GIC metadata for the runtime FDT path, map/unmap allocated guest memory with backend-owned cleanup, create/destroy one current-thread vCPU handle, define typed HVF exit snapshots, decode candidate MMIO data-abort exits into access metadata, resolve decoded accesses against the internal MMIO registry into owner/offset metadata, classify whole vCPU exits into MMIO, virtual-timer, canceled, or unknown events, build runtime MMIO operations from resolved HVF exits, complete MMIO read exits back into guest GPRs, dispatch a single resolved HVF MMIO access through runtime handlers and complete read results, get/set a narrow set of vCPU registers including MPIDR_EL1, configure the primary arm64 Linux boot-register state for one vCPU, and start a thread-owned runner that can apply that setup, explicitly dispatch one resolved MMIO access, run once and handle a resulting MMIO exit on the vCPU-owning thread, and cancel a single `hv_vcpu_run` step. It intentionally does not include:

- successful API endpoints beyond `GET /`, `GET /version`, `GET /machine-config`, pre-boot `PUT /machine-config` configuration storage, pre-boot `PUT /boot-source` configuration storage, and pre-boot `PUT /drives/{drive_id}` configuration storage
- successful public `/actions` behavior beyond parser-level validation and fixed unsupported faults
- public `/boot-source` behavior beyond recording valid boot-source configuration or public block-device behavior beyond recording valid `/drives/{drive_id}` configuration
- public command-line or FDT configuration behavior
- complete interrupt delivery, configured guest execution, continuous vCPU run loops, public startup or HVF runner-loop wiring for block queue notifications, backend interrupt signaling, device-backed feature negotiation, indirect descriptors, device-backed MMIO loops, real device emulation, multi-vCPU setup, or PSCI behavior

## Process CLI

The `bangbang` executable accepts the first process-lifecycle arguments and starts the API socket server:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

- `--api-sock <PATH>` sets the Unix socket path for the API server. The default is `/tmp/bangbang.socket`.
- `--id <ID>` records the microVM identifier. IDs must be 1 to 64 bytes and contain only ASCII alphanumeric characters or `-`. The default is `anonymous-instance`.
- `--help`, `-h`, `--version`, and `-V` are supported.

`bangbang` binds the configured socket path, serves `GET /`, `GET /version`, `GET /machine-config`, pre-boot `PUT /machine-config` configuration storage, pre-boot `PUT /boot-source` configuration storage, pre-boot `PUT /drives/{drive_id}` configuration storage, and parser-level `PUT /actions` unsupported faults, and stays running until `SIGINT` or `SIGTERM` requests shutdown. Unsupported Firecracker process options such as `--config-file`, `--no-api`, seccomp, logging, metrics, snapshot, MMDS, and PCI flags are rejected instead of ignored.

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

Successful boot-source configuration returns `204 No Content`. The paths are
stored as configuration only; bangbang does not open kernel or initrd files,
load payloads, build an FDT, configure vCPU registers, or start a guest yet.

Record a pre-boot drive configuration:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/drives/rootfs \
  -H 'Content-Type: application/json' \
  -d '{"drive_id":"rootfs","path_on_host":"/tmp/rootfs.ext4","is_root_device":true}'
```

Successful drive configuration returns `204 No Content`. The path is stored as
configuration only; bangbang does not open the file or attach a block device yet.

Submit a parser-level action request:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/actions \
  -H 'Content-Type: application/json' \
  -d '{"action_type":"InstanceStart"}'
```

The request body is parsed, but action execution is not implemented yet and
currently returns `400 Bad Request` with a `fault_message` body.

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

Run the VMM process skeleton and API server:

```sh
cargo run -p bangbang
```
