# bangbang

bangbang is a Rust VMM project for macOS hosts. It aims to keep the public
control plane compatible with the Firecracker HTTP API over a Unix domain
socket, while the VM backend is built on Apple's Hypervisor.framework.

The repository is still a scaffold. Use the documentation below as the source of
truth for detailed capability status, compatibility limits, security boundaries,
and test rules:

- [Firecracker Compatibility Scope](docs/firecracker-compatibility.md)
- [Firecracker Validation Matrix](docs/firecracker-validation-matrix.md)
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
- `--config-file <PATH>` reads a Firecracker-shaped JSON configuration for the
  supported startup subset from a readable regular file up to 1 MiB, starts the
  VM, then serves the API socket unless `--no-api` is set.
- `--http-api-max-payload-size <BYTES>` sets the maximum accepted HTTP API
  request size. The default is `51200` bytes.
- `--id <ID>` records the microVM identifier. The default is
  `anonymous-instance`.
- `--start-time-us <MICROS>`, `--start-time-cpu-us <MICROS>`, and
  `--parent-cpu-time-us <MICROS>` accept Firecracker launcher timing values for
  configured minimal startup metrics output, explicit `FlushMetrics`, and
  periodic runtime metrics flushes.
- `--metrics-path <PATH>` configures the same per-process metrics sink as
  `PUT /metrics` before the API socket is served.
- `--mmds-size-limit <BYTES>` sets the maximum serialized MMDS data-store size.
  When omitted, it inherits the HTTP API request-size limit, which defaults to
  `51200` bytes.
- `--log-path <PATH>`, `--level <LEVEL>`, `--module <MODULE>`,
  `--show-level`, and `--show-log-origin` configure the same per-process
  logger state as `PUT /logger` before the API socket is served. The current
  minimal action logs use module path `bangbang_runtime::vmm_action`.
- `--no-api` requires `--config-file <PATH>`, starts from that configuration
  without publishing an API socket, and exits cleanly on `SIGINT` or `SIGTERM`.
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

Start from a configuration file while keeping the API socket enabled:

```sh
cargo run -p bangbang -- \
  --api-sock /tmp/bangbang.socket \
  --config-file /tmp/bangbang-vm.json
```

Start from a configuration file without publishing an API socket:

```sh
cargo run -p bangbang -- \
  --config-file /tmp/bangbang-vm.json \
  --no-api
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
  -d '{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"12:34:56:78:9a:bc","mtu":1500}'
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

Configured metrics output records an initial minimal JSON line when startup
metrics are configured successfully. It also records explicit runtime
`FlushMetrics` actions and periodic runtime metrics flushes every 60 seconds
while the VM is running. After `InstanceStart`, the line also includes a
`boot_run_loop_status` summary such as `running`, `exited`, or `failed` when a
process-owned boot worker exists. When startup timing CLI values are provided,
the same metrics output includes `start_time_us`, `start_time_cpu_us`, and
`parent_cpu_time_us`. The current Firecracker-shaped API request metrics subset
also reports selected GET counters under `get_api_requests`, parsed core
configuration, MMDS, observability, and `/actions` counters under
`put_api_requests`, and selected PATCH counters under `patch_api_requests`.
After a metrics write failure or logger action write failure, later successful
metrics output includes the minimal Firecracker-shaped `logger.metrics_fails`,
`logger.missed_metrics_count`, and `logger.missed_log_count` counters.

Configure logger output before boot:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/logger \
  -H 'Content-Type: application/json' \
  -d '{"log_path":"/tmp/bangbang.log","level":"Info","module":"bangbang_runtime","show_level":true,"show_log_origin":true}'
```

Configured logger output records minimal successful `InstanceStart` and
`FlushMetrics` action events. `show_level` adds `level=Info`, and
`show_log_origin` adds the runtime action callsite as `origin=<file>:<line>`.
`module` filters these minimal action logs by prefix against
`bangbang_runtime::vmm_action`.
Full internal log routing remains deferred.

Submit an `InstanceStart` action:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/actions \
  -H 'Content-Type: application/json' \
  -d '{"action_type":"InstanceStart"}'
```

See [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) for
the full endpoint matrix, implemented behavior, and deferred Firecracker
features. See [Firecracker Validation Matrix](docs/firecracker-validation-matrix.md)
for the support status and validation layer summary.

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

- `0`: help or version completed successfully, the API server exited without
  error, or no-api mode handled `SIGINT`/`SIGTERM`.
- `153`: startup argument parsing failed before process configuration began.
  This matches Firecracker's argument-parsing exit code.
- `1`: process failure, including config-file startup, startup metrics/logger
  configuration, API socket bind, shutdown signal handling, or API accept
  failures.
