# bangbang

bangbang is a Rust VMM project for macOS hosts. The public control plane is intended to stay compatible with the Firecracker HTTP API over a Unix domain socket, while the VM backend is built on Apple's Hypervisor.framework.

This repository is currently a scaffold. It defines crate boundaries, an initial Firecracker-compatible API socket, a process startup CLI, a minimal internal VMM action model, a backend trait, a backend-neutral guest address/layout model, anonymous guest memory allocation and byte access, arm64 boot placement helpers, and the smallest Hypervisor.framework VM, guest memory map/unmap ownership, current-thread vCPU lifecycle, typed exit surface, register wrappers, and cancellable vCPU runner skeleton.

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

The first target is Apple Silicon macOS. The current scaffold includes HTTP over a Unix domain socket for `GET /` and `GET /version`, routed through a minimal read-only VMM action model. The runtime crate defines guest physical address, range, and aarch64 DRAM layout primitives, can allocate owned anonymous host memory for validated page-aligned guest memory layouts, can safely read/write byte slices by guest address, and exposes the first arm64 kernel/FDT/initrd placement helpers. The HVF crate can create/destroy a process VM, map/unmap allocated guest memory with backend-owned cleanup, create/destroy one current-thread vCPU handle, define typed HVF exit snapshots, get/set a narrow set of vCPU registers, and start a thread-owned runner that can cancel a single `hv_vcpu_run` step. It intentionally does not include:

- API endpoints beyond `GET /` and `GET /version`
- JSON request body models
- configured guest execution, continuous vCPU run loops, MMIO/device emulation, or boot register setup
- kernel loading

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
guest memory mapping, and runner smoke tests in
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
