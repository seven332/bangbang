# Specification benchmark observations

Bangbang provides one strict Apple Silicon collector for repeatable,
environment-labelled observations of its signed production VMM:

```sh
scripts/specification-benchmark.py collect \
  --config scripts/specification-benchmark-config.example.json \
  --output .tmp/bangbang-specification-report.json
scripts/specification-benchmark.py validate \
  --report .tmp/bangbang-specification-report.json
```

The output path must not exist. Collection prepares the checked guest artifacts,
builds one locked default-feature-free release binary, signs it for
Hypervisor.framework, performs the configured warmups and samples, completely
cleans every private process/FIFO/socket/file session, and only then publishes
one mode-0600 canonical report without replacing an existing path. The source
tree must be clean, and collection fails on any non-Apple-Silicon host or
unavailable HVF execution boundary. There is no unsupported-host success mode.

This is an observation tool, not a performance acceptance test. It emits no
numeric pass/fail threshold, Firecracker parity decision, percentage delta, or
regression verdict.

## Configuration

The checked example is a complete version-1 configuration:

```json
{
  "host_label": "apple-silicon-lab",
  "iterations": 3,
  "schema_version": 1,
  "timeouts": {
    "artifact_seconds": 600,
    "build_seconds": 900,
    "guest_seconds": 60,
    "network_seconds": 60,
    "request_seconds": 5,
    "startup_seconds": 30,
    "terminate_seconds": 5
  },
  "tracing": "disabled",
  "warmups": 1
}
```

Documents use sorted, two-space-indented ASCII JSON with one final newline.
Duplicate, unknown, missing, noncanonical, oversized, unsafe, boolean-as-number,
or out-of-range values are rejected. Iterations must be odd and between 3 and
31; warmups must be between 0 and 10. The host label is a caller-chosen,
privacy-safe comparison identity. It is not derived from the hostname.

Tracing is exactly `disabled`. The collector builds:

```sh
cargo build -p bangbang --release --locked --no-default-features \
  --target aarch64-apple-darwin
```

It verifies that the signed binary contains no fixed tracing marker. Build,
artifact preparation, signing, and warmups happen outside retained samples.

## What one retained iteration measures

Each iteration is one transaction containing two independent signed VM
sessions. A value is retained only after both sessions exit, their process
groups are reaped, their API sockets disappear, all file descriptors close,
and their private directories are removed.

The workload session boots the checked read-only 512-MiB direct-rootfs image
with one vCPU and 256 MiB through the public Unix-socket API. The guest program
`/bangbang-specification-benchmark` writes the production boot-timer magic,
emits its versioned ready record, and blocks on the exact serial byte `R`. While
the guest is blocked, the collector samples whole-process RSS. Only then does it
release the guest to run the fixed compute loop and fixed sequential 16-MiB
root-drive read. The guest uses `CLOCK_MONOTONIC` for both durations, consumes
fixed checksums, emits a closed transcript, and requests poweroff.

The telemetry session boots the same workload independently and configures a
real nonblocking metrics FIFO. The collector:

1. drains and parses exactly one valid initial metrics line;
2. fills the same FIFO through a separate nonblocking writer with a fixed
   sentinel until `EAGAIN`;
3. issues exactly one public `FlushMetrics` action and requires the typed
   `failed to flush metrics: WouldBlock` HTTP failure;
4. drains all filler and any partial failed publication bytes;
5. retries exactly once, requires `204 No Content`, and parses the new complete
   line; and
6. requires `logger.missed_metrics_count == 1` before releasing and cleaning
   the guest.

Pipe pressure therefore cannot contaminate the timed workload session, and a
pipe capacity alone is never treated as loss evidence.

The exact core series are:

| Name | Unit | Producer or method |
| --- | --- | --- |
| `process_startup_wall_us` | microseconds | first production metrics line from a pre-spawn `CLOCK_MONOTONIC` baseline |
| `process_startup_cpu_us` | microseconds | first production metrics line from the zero child-process CPU baseline |
| `whole_process_rss_kib` | KiB | `/bin/ps -o rss= -p PID` at the ready barrier |
| `guest_init_wall_us` | microseconds | production `Guest-boot-time` record |
| `guest_init_cpu_us` | microseconds | production `Guest-boot-time` record |
| `guest_compute_duration_ns` | nanoseconds | guest monotonic fixed-loop clock |
| `guest_storage_duration_ns` | nanoseconds | guest monotonic fixed-read clock |
| `metrics_fifo_filled_bytes` | bytes | fixed sentinel written until `EAGAIN` |
| `metrics_fifo_drained_bytes` | bytes | bytes drained after the failed flush |
| `metrics_missed_count` | count | exact successful replay counter |

Every series retains all raw nonnegative integer observations and derives only
integer `count`, `min`, `median`, and `max`. The odd sample count makes median
unambiguous without floating-point arithmetic.

The collector passes each VMM the Firecracker-shaped `--start-time-us` value
sampled immediately before spawn and `--start-time-cpu-us 0`, so the production
startup stores measure elapsed wall time and the new child process's CPU time.
Both observations must be positive; an unconfigured/default zero is rejected.

## Report identity and comparison

The report records only the stable facts required to decide whether two sample
sets are comparable:

- the caller host label and bounded macOS version/build/kernel fields;
- Apple CPU architecture, public brand/model, and logical CPU count;
- clean Git commit and tree, Cargo.lock digest, toolchain versions, target,
  release profile, empty feature set, and disabled tracing;
- signed binary digest and size, ad-hoc HVF signing class, HVF/MMIO backend,
  vCPU count, and memory size;
- kernel, rootfs, recipe, and workload-source digests and sizes, fixed boot
  arguments, operation/byte/block counts, checksums, and protocol version;
- warmups, iterations, every timeout, uncontrolled page-cache policy,
  publication policy, RSS method, telemetry method, and every metric
  name/method/unit; and
- when present, the strict network fixture document and executable digests plus
  its backend/workload/method/unit labels.

The report never records hostname, username, repository/cache/session paths,
codesign identity details, environment variables, or network fixture argv. A
SHA-256 comparison key is recomputed from the complete identity/policy/metric
definition envelope. Raw values and summaries are deliberately excluded from
that key.

Compare two already validated reports with:

```sh
scripts/specification-benchmark.py compare \
  --previous /path/to/previous.json \
  --current /path/to/current.json
```

Comparison refuses any key mismatch. When identities match, it prints canonical
previous/current summaries and nothing that interprets whether a number is
good, bad, passing, failing, or equivalent to another VMM.

Reports are local evidence and are not checked in. A source commit or binary
digest change intentionally requires a new collection rather than comparison
with the old build.

## Optional network fixture

The report has no `network` member by default. Network collection requires an
explicit canonical fixture document with this closed shape:

```json
{
  "argv": ["/absolute/path/to/operator-fixture"],
  "backend": "vmnet-shared",
  "credential_mode": "none",
  "method": "fixed-transfer-v1",
  "schema_version": 1,
  "timeout_seconds": 60,
  "unit": "bytes-per-second",
  "workload": "operator-network-v1"
}
```

The executable must be an absolute regular executable. It runs directly,
without `shell=True`, in an owned process group, private working directory,
fixed minimal environment, bounded time, and bounded capture. Its timeout may
not exceed the configuration's `network_seconds` policy. Its identity is checked
before and after every execution. It must write exactly one canonical document
matching the configured labels:

```json
{
  "backend": "vmnet-shared",
  "cleanup": "complete",
  "method": "fixed-transfer-v1",
  "schema_version": 1,
  "unit": "bytes-per-second",
  "value": 12345,
  "workload": "operator-network-v1"
}
```

`cleanup: complete` is the fixture assertion that its external resources were
released; Bangbang cannot independently prove those resources. The collector
passes no credentials and rejects diagnostic output, mismatched labels,
timeouts, overflow, executable replacement, or incomplete cleanup. Only the
fixture/executable digests, labels, raw integers, and summary enter the report.

This adapter does not prove production vmnet availability, credentials,
connectivity, service recovery, concurrency, or cleanup. Those remain owned by
the external network gate in issue #1378.

## Firecracker reference figures

The pinned Firecracker v1.16.0 specification describes results and targets from
AWS bare-metal Linux/KVM environments with sufficient resources. Its detailed
network document uses M5d.metal, Amazon Linux 2, VPC networking, ping, and
multiple iperf3 clients. The reference set includes:

- API startup CPU time up to 8 ms and wall time commonly around 12 ms within a
  broader 6–60 ms range;
- up to 5 MiB VMM memory overhead for a one-vCPU/128-MiB VM after excluding
  guest mappings and workload-dependent additions;
- up to 125 ms from `InstanceStart` to `/sbin/init` for its minimal boot setup;
- a greater-than-95-percent bare-metal compute objective marked pending;
- storage and network throughput/latency objectives whose cited mechanisms use
  fio, cache dropping, TAP/SSH, ping, and iperf3, with several outcomes marked
  pending; and
- a full nonblocking metrics FIFO may lose output and must account for that
  loss.

These facts are pinned reference claims, not Apple/HVF limits. Bangbang does not
claim AWS/KVM parity, the same resource model, or reproduction of those numbers.

## Interpretation and nonclaims

- `whole_process_rss_kib` includes the complete Bangbang process and guest
  mappings. It is not Firecracker guest-map-excluding VMM overhead and must not
  be compared with the 5-MiB reference.
- The fixed guest loop is not CoreMark or a bare-metal CPU ratio. The sequential
  read is not fio, uncached storage throughput, durability, or a device limit.
- Host page-cache state is recorded as uncontrolled. The collector performs no
  privileged cache drop or resource isolation.
- Optional network values are not iperf/ping equivalence and do not close the
  external gate; the adapter does not certify #1378.
- Normal CI verifies schemas, parsers, summary math, process/FIFO/fixture
  boundaries, cleanup, publication, and the checked static authority. It does
  not enforce a hardware number.

Before closing a delivery issue that depends on these observations, synchronize
and clean `main`, collect a fresh networkless report without an unsupported
bypass, validate it, and confirm that its commit/tree/binary identities describe
that merged tree.
