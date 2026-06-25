# bangbang

bangbang is a Rust VMM project for macOS hosts. The public control plane is intended to stay compatible with the Firecracker HTTP API over a Unix domain socket, while the VM backend is built on Apple's Hypervisor.framework.

This repository is currently a scaffold. It defines crate boundaries, Firecracker-compatible API endpoint names, an initial process startup CLI, a backend trait, and the smallest Hypervisor.framework VM create/destroy wrapper.

See [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) for the intended compatibility target and current limitations.

## Layout

```text
crates/api        Firecracker-compatible API endpoint names
crates/runtime    Backend-neutral VM trait and error type
crates/hvf        Hypervisor.framework backend skeleton
crates/bangbang   VMM process entrypoint and startup CLI
```

## Current Scope

The first target is Apple Silicon macOS. The current scaffold intentionally does not include:

- an API server
- API socket binding or listener cleanup
- JSON request/response models
- guest memory mapping
- vCPU creation or a run loop
- kernel loading

## Process CLI

The `bangbang` executable accepts the first process-lifecycle arguments:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

- `--api-sock <PATH>` records the intended Unix socket path. The default is `/tmp/bangbang.socket`.
- `--id <ID>` records the microVM identifier. The default is `anonymous-instance`.
- `--help`, `-h`, `--version`, and `-V` are supported.

These arguments are parsed and validated only. `bangbang` does not bind the socket or serve the API yet. Unsupported Firecracker process options such as `--config-file`, `--no-api`, seccomp, logging, metrics, snapshot, MMDS, and PCI flags are rejected instead of ignored.

## Exit Status

- `0`: help, version, or parser-only startup completed successfully.
- `153`: startup argument parsing or validation failed. This matches Firecracker's argument-parsing exit code.
- `1`: reserved for future non-argument process failures; the current scaffold has no such failure path.

## Build

```sh
cargo check
cargo test
```

Run the VMM process skeleton:

```sh
cargo run -p bangbang
```
