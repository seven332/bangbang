# bangbang

bangbang is a Rust VMM project for macOS hosts. It aims to keep the public
control plane compatible with the Firecracker HTTP API over a Unix domain
socket, while the VM backend is built on Apple's Hypervisor.framework.

The repository is still a scaffold. Use the documentation below as the source of
truth for detailed capability status, compatibility limits, security boundaries,
and test rules:

- [Firecracker Compatibility Scope](docs/firecracker-compatibility.md)
- [macOS Host Security Model](docs/security.md)
- [Testing Guide](docs/testing.md)
- [Pull Request Review Guidelines](docs/review-guidelines.md)

## Layout

```text
crates/api        Firecracker-compatible API request and response surface
crates/runtime    Backend-neutral VM model, memory, MMIO, boot, and device helpers
crates/hvf        Hypervisor.framework backend and signed integration tests
crates/bangbang   VMM process entrypoint and startup CLI
```

## Process CLI

Run the VMM process skeleton and API server:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

- `--api-sock <PATH>` sets the Unix socket path. The default is
  `/tmp/bangbang.socket`.
- `--id <ID>` records the microVM identifier. The default is
  `anonymous-instance`.
- `--metrics-path <PATH>` configures the same per-process metrics sink as
  `PUT /metrics` before the API socket is served.
- `--log-path <PATH>`, `--level <LEVEL>`, `--module <MODULE>`,
  `--show-level`, and `--show-log-origin` configure the same per-process
  logger state as `PUT /logger` before the API socket is served.
- `--help`, `-h`, `--version`, and `-V` are supported.

The API socket is an unauthenticated local control interface. Filesystem
permissions on the socket path and parent directory are the access-control
boundary, so use a private directory or restrictive umask on multi-user hosts.

Start with metrics and logger output configured:

```sh
cargo run -p bangbang -- \
  --api-sock /tmp/bangbang.socket \
  --id demo-1 \
  --metrics-path /tmp/bangbang.metrics \
  --log-path /tmp/bangbang.log \
  --level Info \
  --show-level
```

## API Examples

Query the instance info endpoint:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/
```

Example response:

```json
{"app_name":"bangbang","id":"demo-1","state":"Not started","vmm_version":"0.1.0"}
```

Query the accumulated VM configuration:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/vm/config
```

Record a pre-boot boot source:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/boot-source \
  -H 'Content-Type: application/json' \
  -d '{"kernel_image_path":"/tmp/vmlinux","boot_args":"console=ttyS0 reboot=k panic=1"}'
```

Record a pre-boot drive:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/drives/rootfs \
  -H 'Content-Type: application/json' \
  -d '{"drive_id":"rootfs","path_on_host":"/tmp/rootfs.ext4","is_root_device":true}'
```

Record a pre-boot network interface:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/network-interfaces/eth0 \
  -H 'Content-Type: application/json' \
  -d '{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"12:34:56:78:9a:bc"}'
```

Record a pre-boot vsock configuration:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/vsock \
  -H 'Content-Type: application/json' \
  -d '{"guest_cid":3,"uds_path":"./v.sock"}'
```

Configure metrics output before boot:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/metrics \
  -H 'Content-Type: application/json' \
  -d '{"metrics_path":"/tmp/bangbang.metrics"}'
```

Configured metrics output records a minimal JSON line for successful runtime
`FlushMetrics` actions.

Configure logger output before boot:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/logger \
  -H 'Content-Type: application/json' \
  -d '{"log_path":"/tmp/bangbang.log","level":"Info","show_level":true}'
```

Configured logger output records minimal successful `InstanceStart` and
`FlushMetrics` action events. Full internal log routing remains deferred.

Submit an `InstanceStart` action:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/actions \
  -H 'Content-Type: application/json' \
  -d '{"action_type":"InstanceStart"}'
```

See [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) for
the full endpoint matrix, implemented behavior, and deferred Firecracker
features.

## Build And Test

Requires the latest stable Rust toolchain.

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-targets --all-features --locked --exclude bangbang-hvf
cargo test -p bangbang-hvf --lib --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
```

Run signed HVF integration tests on macOS Apple Silicon:

```sh
scripts/run-integration-tests.sh
```

Prepare the pinned Firecracker arm64 Linux kernel artifact used by guest boot
validation work:

```sh
scripts/fetch-firecracker-kernel.sh
```

Run only the minimal guest boot integration test on macOS Apple Silicon:

```sh
scripts/run-integration-tests.sh --test guest_boot
```

Hosted macOS CI may build and sign integration tests without executing HVF:

```sh
scripts/run-integration-tests.sh --allow-unsupported
```

See [Testing Guide](docs/testing.md) for test layering, signed integration-test
rules, guest boot artifact caching, and local verification expectations.

## Exit Status

- `0`: help or version completed successfully, or the API server exited without
  error.
- `153`: startup argument parsing or validation failed. This matches
  Firecracker's argument-parsing exit code.
- `1`: non-argument process failure, including API socket bind or accept
  failures.
