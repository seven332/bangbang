# bangbang

bangbang is a Rust VMM project for macOS hosts. The public control plane is intended to stay compatible with the Firecracker HTTP API over a Unix domain socket, while the VM backend is built on Apple's Hypervisor.framework.

This repository is currently a scaffold. It defines crate boundaries, Firecracker-compatible API endpoint names, a backend trait, and the smallest Hypervisor.framework VM create/destroy wrapper.

See [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) for the intended compatibility target and current limitations.

## Layout

```text
crates/api        Firecracker-compatible API endpoint names
crates/runtime    Backend-neutral VM trait and error type
crates/hvf        Hypervisor.framework backend skeleton
crates/bangbang   VMM process entrypoint skeleton
```

## Current Scope

The first target is Apple Silicon macOS. The current scaffold intentionally does not include:

- an API server
- JSON request/response models
- guest memory mapping
- vCPU creation or a run loop
- kernel loading

## Build

```sh
cargo check
cargo test
```

Run the VMM process skeleton:

```sh
cargo run -p bangbang
```
