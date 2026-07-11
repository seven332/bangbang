# bangbang

bangbang is a Rust VMM project for macOS hosts. It aims to keep the public
control plane compatible with the Firecracker HTTP API over a Unix domain
socket, while the VM backend is built on Apple's Hypervisor.framework.

The repository is still a scaffold. Use the documentation below as the source of
truth for detailed capability status, compatibility limits, security boundaries,
and test rules:

- [Firecracker Compatibility Scope](docs/firecracker-compatibility.md)
- [Firecracker Validation Matrix](docs/firecracker-validation-matrix.md)
- [Snapshot Feasibility](docs/snapshot-feasibility.md)
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

The HVF runner currently exposes owner-thread capture building blocks for
general registers, plus ordered nontransactional restore of the same typed
X0-X30/PC/CPSR value, raw core system registers plus ordered nontransactional
restore of their typed SP_EL0/SP_EL1/ELR_EL1/SPSR_EL1 value, raw EL1 exception
registers plus ordered nontransactional restore of their typed
AFSR0/AFSR1/ESR/FAR/PAR/VBAR value, raw EL1 execution controls plus ordered
nontransactional restore of their typed ACTLR/CPACR value, raw thread-context
registers plus ordered nontransactional restore of their typed
TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 value, raw EL1 translation registers plus
ordered nontransactional restore of their typed
SCTLR/TTBR0/TTBR1/TCR/MAIR/AMAIR/CONTEXTIDR value, baseline SIMD/FP registers
plus ordered nontransactional restore of their typed Q0-Q31/FPCR/FPSR value,
baseline and optional SVE/SME guest-visible processor identification metadata,
mutable SME PSTATE flags, raw SME system registers with redacted `Debug`,
conditional maximum-width streaming Z0-Z31 contents with
redacted `Debug`, conditional maximum-derived streaming P0-P15 predicates with
redacted `Debug`, conditional maximum-SVL-square ZA contents with redacted
`Debug`, conditional fixed-size SME2 ZT0 contents with redacted `Debug`, raw
system-context registers with redacted `Debug` plus ordered nontransactional
restore of their typed SCXTNUM_EL0/SCXTNUM_EL1 value, raw cache-selection plus
ordered nontransactional restore of its typed CSSELR_EL1 value,
hardware-breakpoint,
hardware-watchpoint, debug-control plus ordered nontransactional restore of its
typed MDCCINT_EL1/MDSCR_EL1 value, raw Hypervisor.framework debug-trap policy
plus ordered nontransactional restore of its complete two-Boolean value,
pointer-authentication key state with redacted `Debug` plus ordered
nontransactional restore of the complete APIA/APIB/APDA/APDB/APGA value, raw
physical and virtual timer state, CPU-level IRQ/FIQ pending injection
levels plus ordered nontransactional restore of their complete typed value,
opaque GIC device state plus runner-owned pre-first-run reapply, and raw EL1
GIC ICC CPU-interface registers plus ordered pre-first-run restore of their nine
mutable values with derived RPR validation.
A separate no-handle query exposes the maximum SME streaming vector length used
for the Z-, P-, and ZA-register allocations. These are internal snapshot
feasibility primitives only. bangbang now has a bounded native-v1 outer state
envelope, read-only version inspection, and internal handle-level guest-memory
image/binding primitives that stream full GPA ranges into or out of anonymous
memory. No process or API path emits those artifacts. Public snapshot
create/load, no-clobber publication, complete restore, a composite VM-state
payload schema, remaining vCPU state, EL2 GIC CPU-interface state,
effective-SVL interpretation, SME/SME2 feature and destination policy,
remaining setters and transition ordering, cache-topology manifests, and
complete emulated device state remain unsupported.

## Process CLI

Run the VMM process skeleton and API server:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

Supported value-taking options accept either `--name value` or `--name=value`.
Value-less flags, such as `--no-api`, do not accept an attached value.

- `--api-sock <PATH>` sets the Unix socket path. The default is
  `/tmp/bangbang.socket`.
- `--boot-timer` enables Firecracker-compatible guest boot-time logging. During
  startup, bangbang registers a pseudo-MMIO boot timer at Firecracker's aarch64
  boot timer address; a guest write of byte value `123` at offset `0` logs the
  elapsed wall and process CPU time when logger output is configured.
- `--config-file <PATH>` reads a Firecracker-shaped JSON configuration for the
  supported startup subset from a readable regular file up to 1 MiB, starts the
  VM, then serves the API socket unless `--no-api` is set.
- `--http-api-max-payload-size <BYTES>` sets the maximum accepted HTTP API
  request body size declared by `Content-Length`. The default is `51200` bytes;
  request-head bytes are bounded separately by the parser.
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
  logger state as `PUT /logger` before the API socket is served. Current
  minimal logger events use module paths `bangbang_runtime::api_server`,
  `bangbang_runtime::vmm_action`, and `bangbang_runtime::boot_timer`.
- `--no-api` requires `--config-file <PATH>`, starts from that configuration
  without publishing an API socket, and exits cleanly on `SIGINT` or `SIGTERM`.
- `--snapshot-version` prints the supported bangbang-native snapshot envelope
  version (`v1.0.0`) and exits before startup.
- `--describe-snapshot <PATH>` reads a bounded regular native state file,
  validates its complete envelope and CRC, prints its embedded version, and
  exits before startup. It does not accept Firecracker state files.
- `--help`, `-h`, `--version`, and `-V` are supported.

The API socket is an unauthenticated local control interface. bangbang restricts
the published socket inode to owner-only permissions; the parent directory is
still part of the access-control boundary, so use a private directory on
multi-user hosts.

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
the same metrics output includes Firecracker-style
`api_server.process_startup_time_us` and
`api_server.process_startup_time_cpu_us` elapsed values. `--start-time-us` is
subtracted from the sampled monotonic clock, `--start-time-cpu-us` is
subtracted from the sampled process CPU clock, and `--parent-cpu-time-us`
contributes to the CPU value without being serialized as a separate field. If a
provided start timestamp is later than the sampled clock value, the elapsed
component saturates at zero. The current
Firecracker-shaped API request metrics subset also reports selected GET counters
under `get_api_requests`; parsed core
configuration, MMDS, observability, memory hotplug, pmem, and `/actions`
counters under `put_api_requests`; parser failures, including malformed bodies
and path/body ID mismatches, for those PUT endpoints with matching
Firecracker-style fields in the matching
`put_api_requests` count/fail counters; and selected PATCH counters including
memory hotplug and pmem under `patch_api_requests`, including parser failures
for those PATCH endpoints. bangbang also records
bangbang-specific `balloon_count` API request counters for parsed balloon GET,
PUT, and PATCH routes, plus `balloon_fails` counters for parsed balloon PUT and
PATCH failures and identifiable malformed balloon PUT/PATCH parser failures,
because Firecracker does not expose matching balloon API request metric fields.
Runtime metrics flushes can also include a top-level aggregate `block` object
and non-empty per-drive `block_{drive_id}` objects for implemented virtio-block
queue activity, read/write latency aggregates, backing update counters, and
failures; a top-level aggregate `pmem` object and non-empty per-device
`pmem_{id}` objects for implemented virtio-pmem queue activity and failures;
top-level aggregate `net` and non-empty per-interface
`net_{iface_id}` objects for implemented virtio-net RX/TX queue activity,
packet counts, byte counts, and failures; a top-level `mmds` object for
implemented guest MMDS packet detour and response queue activity; a top-level
`vsock` object for implemented virtio-vsock RX/TX queue activity, packet
counts, byte counts, connection cleanup counters, and classifiable queue/event
failures; a top-level `entropy` object with Firecracker-shaped counters for
implemented virtio-rng request, byte, host-randomness failure, and event-failure
activity; a
top-level `uart` object with Firecracker-shaped serial counters for implemented
TX writes, missed writes, output errors, and rate-limiter drops; a top-level
`signals` object with `sigpipe` counts for handled non-terminating `SIGPIPE`;
plus a top-level `balloon` object for implemented virtio-balloon activity and
failures.
Parsed deprecated HTTP API
usage is counted under `deprecated_api.deprecated_http_api_calls` for supported
deprecated machine `cpu_template`, MMDS V1 config, `vsock_id`, and snapshot-load
field forms.
After a metrics write failure, API request logger write failure, action logger
write failure, or boot-timer logger write failure, later successful metrics
output includes the minimal
Firecracker-shaped `logger.missed_metrics_count` and `logger.missed_log_count`
counters.

Configure logger output before boot:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/logger \
  -H 'Content-Type: application/json' \
  -d '{"log_path":"/tmp/bangbang.log","level":"Info","module":"bangbang_runtime","show_level":true,"show_log_origin":true}'
```

Configured logger output records minimal successfully parsed API request
method/path lines without request bodies, plus successful `InstanceStart` and
`FlushMetrics` action events. `show_level` adds `level=Info`, and
`show_log_origin` adds the callsite as `origin=<file>:<line>`.
`module` filters these minimal logger events by prefix against
`bangbang_runtime::api_server`, `bangbang_runtime::vmm_action`, or
`bangbang_runtime::boot_timer`. When `--boot-timer` is enabled, boot-time log
events use the boot-timer module path.
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
- `152`: startup configuration failed before the process entered runtime,
  including config-file, metadata, startup logger, and startup metrics
  configuration failures. This matches Firecracker's bad-configuration exit
  code.
- `153`: startup argument parsing failed before process configuration began.
  This matches Firecracker's argument-parsing exit code.
- `148`, `149`, `150`, `151`, `154`, `156`, `157`: Firecracker-compatible
  fatal or restricted host signal exits for `SIGSYS`, `SIGBUS`, `SIGSEGV`,
  `SIGXFSZ`, `SIGXCPU`, `SIGHUP`, and `SIGILL`.
- `1`: process failure, including API socket bind, shutdown signal handling, API
  accept failures, or process-owned runtime failures.
