# bangbang

bangbang is a Rust VMM project for macOS hosts. The public control plane is intended to stay compatible with the Firecracker HTTP API over a Unix domain socket, while the VM backend is built on Apple's Hypervisor.framework.

This repository is currently a scaffold. It defines crate boundaries, an initial Firecracker-compatible API socket, a process startup CLI, a minimal internal VMM action model, a backend trait, a backend-neutral guest address/layout model, anonymous guest memory allocation and byte access, arm64 boot placement helpers, internal boot-source validation with arm64 kernel/initrd payload loading, an internal Firecracker-shaped drive configuration validation model and host-file backing access layer, minimal arm64 FDT generation and guest-memory writes, internal MMIO region/operation/handler-dispatch groundwork, internal interrupt line/status/trigger groundwork, internal virtio-mmio register/access decoding, feature/status, queue, queue notification, and interrupt status/acknowledgement register state plus a composed register handler with notification drain, device-configuration delegation, and `DRIVER_OK` activation-hook helpers, virtqueue descriptor-chain, available-ring read, and used-ring write groundwork, and the smallest Hypervisor.framework VM, GIC boot metadata, SPI interrupt-line allocation and signaling, guest memory map/unmap ownership, current-thread vCPU lifecycle, typed exit surface with MMIO data-abort decoding, registry resolution, and vCPU exit classification, register wrappers, single resolved HVF MMIO exit dispatch/completion through runtime handlers, explicit runner-thread MMIO handling commands, single-vCPU arm64 boot-register setup, and cancellable vCPU runner skeleton.

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

The first target is Apple Silicon macOS. The current scaffold includes HTTP over a Unix domain socket for `GET /` and `GET /version`, routed through a minimal read-only VMM action model. The runtime crate defines guest physical address, range, and aarch64 DRAM layout primitives, can allocate owned anonymous host memory for validated page-aligned guest memory layouts, can safely read/write byte slices by guest address, exposes the first arm64 kernel/FDT/initrd placement helpers, can internally validate a Firecracker-shaped boot source before loading a supported arm64 Linux `Image` kernel plus optional non-empty initrd into guest memory, can internally validate and normalize a Firecracker-shaped drive configuration subset and access regular host-file backing with bounded positioned reads/writes, can build/write a minimal Firecracker-shaped arm64 FDT from loaded boot metadata and backend-neutral interrupt-controller metadata, can register and look up non-overlapping MMIO region ownership for future device dispatch, can represent bounded MMIO read/write operations after lookup, can dispatch those operations to registered internal handlers, can decode checked runtime MMIO operations into typed virtio-mmio register or device-configuration accesses, can model Firecracker-shaped virtio-mmio identity, feature selector, driver-feature, status, queue, queue notification, and interrupt status/acknowledgement register state, can route those register accesses through a composed runtime handler, can drain recorded queue notifications for future device handlers, can delegate device-configuration accesses to an injected backend-neutral handler, can invoke an injected backend-neutral activation hook when `DRIVER_OK` is accepted, can read and validate backend-neutral virtqueue descriptor chains from guest memory, can pop one available-ring head into a parsed descriptor chain, can publish one used-ring completion element, and can model backend-neutral device interrupt lines, pending status bits, acknowledgements, and trigger signaling through an injected sink. The HVF crate can create/destroy a process VM, create macOS 15+ GIC v3 boot metadata without MSI/ITS, allocate deterministic guest interrupt lines from the validated GIC SPI range, signal validated SPI lines through Hypervisor.framework, convert GIC metadata for the runtime FDT path, map/unmap allocated guest memory with backend-owned cleanup, create/destroy one current-thread vCPU handle, define typed HVF exit snapshots, decode candidate MMIO data-abort exits into access metadata, resolve decoded accesses against the internal MMIO registry into owner/offset metadata, classify whole vCPU exits into MMIO, virtual-timer, canceled, or unknown events, build runtime MMIO operations from resolved HVF exits, complete MMIO read exits back into guest GPRs, dispatch a single resolved HVF MMIO access through runtime handlers and complete read results, get/set a narrow set of vCPU registers including MPIDR_EL1, configure the primary arm64 Linux boot-register state for one vCPU, and start a thread-owned runner that can apply that setup, explicitly dispatch one resolved MMIO access, run once and handle a resulting MMIO exit on the vCPU-owning thread, and cancel a single `hv_vcpu_run` step. It intentionally does not include:

- API endpoints beyond `GET /` and `GET /version`
- public JSON request body models for machine config, boot source, drives, or actions
- public `/boot-source`, `/drives`, or `/actions` behavior
- public command-line or FDT configuration behavior
- complete interrupt delivery, configured guest execution, continuous vCPU run loops, queue notification dispatch, completion dispatch, device-backed feature negotiation, virtio-block config space, real block request handling, real device activation effects, indirect descriptors, device-backed MMIO loops, real device emulation, public startup wiring, multi-vCPU setup, or PSCI behavior

## Process CLI

The `bangbang` executable accepts the first process-lifecycle arguments and starts the API socket server:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

- `--api-sock <PATH>` sets the Unix socket path for the API server. The default is `/tmp/bangbang.socket`.
- `--id <ID>` records the microVM identifier. IDs must be 1 to 64 bytes and contain only ASCII alphanumeric characters or `-`. The default is `anonymous-instance`.
- `--help`, `-h`, `--version`, and `-V` are supported.

`bangbang` binds the configured socket path, serves `GET /` and `GET /version`, and stays running until `SIGINT` or `SIGTERM` requests shutdown. Unsupported Firecracker process options such as `--config-file`, `--no-api`, seccomp, logging, metrics, snapshot, MMDS, and PCI flags are rejected instead of ignored.

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
