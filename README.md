# bangbang

bangbang is a Rust VMM project for macOS hosts. The public control plane is intended to stay compatible with the Firecracker HTTP API over a Unix domain socket, while the VM backend is built on Apple's Hypervisor.framework.

This repository is currently a scaffold. It defines crate boundaries, an initial Firecracker-compatible API socket, a process startup CLI, a backend trait, and the smallest Hypervisor.framework VM create/destroy wrapper.

See [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) for the intended compatibility target and current limitations.

## Layout

```text
crates/api        Firecracker-compatible API request and response surface
crates/runtime    Backend-neutral VM trait and error type
crates/hvf        Hypervisor.framework backend skeleton
crates/bangbang   VMM process entrypoint and startup CLI
```

## Current Scope

The first target is Apple Silicon macOS. The current scaffold includes HTTP over a Unix domain socket for `GET /version` only. It intentionally does not include:

- API endpoints beyond `GET /version`
- JSON request body models
- guest memory mapping
- vCPU creation or a run loop
- kernel loading

## Process CLI

The `bangbang` executable accepts the first process-lifecycle arguments and starts the API socket server:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

- `--api-sock <PATH>` sets the Unix socket path for the API server. The default is `/tmp/bangbang.socket`.
- `--id <ID>` records the microVM identifier. The default is `anonymous-instance`.
- `--help`, `-h`, `--version`, and `-V` are supported.

`bangbang` binds the configured socket path, serves `GET /version`, and stays running. Unsupported Firecracker process options such as `--config-file`, `--no-api`, seccomp, logging, metrics, snapshot, MMDS, and PCI flags are rejected instead of ignored.

Query the first supported endpoint:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/version
```

The response body is Firecracker-shaped JSON:

```json
{"firecracker_version":"0.1.0"}
```

## Exit Status

- `0`: help or version completed successfully, or the API server exited without error.
- `153`: startup argument parsing or validation failed. This matches Firecracker's argument-parsing exit code.
- `1`: non-argument process failure, including API socket bind or accept failures.

## Build

```sh
cargo check
cargo test
```

Run the VMM process skeleton and API server:

```sh
cargo run -p bangbang
```
