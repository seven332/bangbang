# Testing Guide

This document defines how to add and run tests in bangbang. Prefer tests that
exercise project behavior through the narrowest public boundary that still
proves the change.

## Test Layers

Use unit tests for small, deterministic logic. Place them next to the code they
exercise under each crate's `src/` tree with Rust's built-in `#[test]`
framework. Unit tests are the right fit for parsers, error formatting, state
transitions, range checks, request validation, and backend-neutral helpers.
The `clippy.toml` test exceptions allow `expect`, `unwrap`, `panic`, and
indexing in `#[test]` bodies, but they do not cover ordinary helper functions in
integration-test crates. If an integration test needs those test-only patterns
in helpers, add a file-scoped allow at the top of that test file:

```rust
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]
```

Keep these allows scoped to test files, and do not use them in production code.

Use normal Rust integration tests when behavior crosses a crate or process
boundary but does not require Hypervisor.framework entitlements. Put these under
the owning crate's `tests/` directory. A PR may start by adding a new
integration test to pin the intended behavior before changing implementation,
especially for CLI, API, filesystem, or cross-crate workflows. The final PR
must leave the new test passing in the documented command set.

Use process-level executable tests when the behavior depends on the real
`bangbang` binary, process arguments, Unix-socket publication, signal handling,
HTTP-over-socket API mutation, or process-owned cleanup but does not enter HVF.
These tests live under `crates/bangbang/tests/` and run in the normal unsigned
workspace test command. They should start `env!("CARGO_BIN_EXE_bangbang")`, use
unique temporary resources, wait on explicit process or socket readiness
signals, and shut the child down with normal signals when testing owned cleanup.

Keep tests that require a signed executable or real HVF execution in separate
Cargo test targets from unsigned tests. Do not hide signing or HVF requirements
behind `#[ignore]` in a normal test target. Mark the dedicated target with
`test = false` in that crate's `Cargo.toml` so `--all-targets` does not run it
accidentally, then run it explicitly from the signed integration runner.

Use HVF crate integration tests for behavior that creates HVF VMs, vCPUs, GIC
state, mapped guest memory, signed test binaries, or guest boot execution
through the `bangbang-hvf` crate. These tests live in `crates/hvf/tests/` and
must run through
`scripts/run-integration-tests.sh` so the binaries are signed with the
`com.apple.security.hypervisor` entitlement. Do not add real HVF tests to the
unsigned workspace test path.

For virtio-pmem changes, unit tests should cover MMIO registration, FDT
metadata, config-space `start`/`size`, deterministic multi-device layout,
queue parsing/completion, shadow writeback, and cleanup/error paths. Signed HVF
coverage validates startup assembly and mapped pmem ownership through the
lifecycle target; the signed `guest_boot` target should cover guest-side pmem
discovery, backing reads, guest writes, and queue-driven flush behavior.

## What To Cover

For CLI and API changes, cover successful requests, unknown options or fields,
empty values, duplicate values, malformed inputs, exit codes, HTTP status codes,
and Firecracker-shaped response bodies.

For host filesystem paths, cover missing paths, directories, unsupported file
types, redacted error messages, cleanup ownership, and failure atomicity. A
failed operation should not partially mutate accepted configuration, guest
memory, or host resources.
For deferred-open paths such as serial output, also cover that parsing stores
configuration without opening the path, and that startup wiring opens or writes
through the selected sink with redacted errors.
For boot-source payload failures, cover both request/API fault formatting and
config-file startup failure paths. Use a test starter that invokes runtime boot
resource assembly when the behavior does not need real HVF execution; keep real
signed executable/HVF coverage in dedicated integration targets.

For guest memory, address, and range logic, cover exact-fit success, one-past
failure, overflow failure, overlapping ranges, and no-partial-mutation behavior.

For process, socket, and multi-bangbang behavior, cover unique resource names,
stale socket handling, shutdown cleanup, replacement races, and concurrent runs
where practical.

For periodic process behavior, test scheduler and timeout paths directly. Do
not wait for real production intervals such as the 60-second metrics flush
period.

For HVF and FFI code, cover resource creation and destruction, platform gating,
error translation, unsupported exits or registers, cancellation, and cleanup
after partial setup failure.
For owner-thread aggregate captures, also cover exact field order, every read
failure and retry, forward and reverse admission conflicts, caller abandonment,
closed command and response channels, queued-command destruction, panic, and
shutdown. Pending-interrupt signed tests must set known asymmetric IRQ/FIQ
values without an intervening run, then clear both levels; a run would let HVF
clear the injection levels and invalidate the round trip. GIC ICC signed tests
must create the GIC before the vCPU, write architecturally writable EL1 ICC
values from signed guest code, and assert only fields or masked bits whose
readback is stable; read-only active-priority values remain host-defined.
Translation-register signed tests must leave `SCTLR_EL1.M` clear, write back
the original SCTLR value, and only then write inert TTBR/TCR/attribute/context
values before HVC. AMAIR is implementation-defined: current Apple Silicon reads
zero after a guest write, while other hosts may preserve the written value.
Exception-register signed tests must use an aligned VBAR address, take no
intervening guest exception after changing it, and never claim vector-table
memory is present. AFSR contents are implementation-defined: current Apple
Silicon reads AFSR0 as zero after a guest write while preserving the test's
AFSR1 value.
Execution-control signed tests require macOS 15 for ACTLR. They must write only
the Hypervisor.framework-supported `ACTLR_EL1.EnTSO` bit and baseline
`CPACR_EL1.FPEN`, execute ISB before guest use or exit, and destroy the VM after
capture instead of treating the changed memory model as a restore round trip.
Identification-register signed tests must capture all eleven stable baseline
values twice within one vCPU lifetime and compare MPIDR with the existing
owner-thread getter. They must not hard-code one Apple MIDR/feature model,
include availability-gated or beta-only IDs, or claim that equal raw values are
a sufficient destination compatibility policy.
SVE/SME identification signed tests require macOS 15.2 and must capture ZFR0
and SMFR0 twice from one idle real vCPU. They may assert same-vCPU stability but
must not hard-code one feature model, enable SVE/SME, enter streaming mode,
read vector/predicate/matrix state, run the vCPU, or treat equality as a
destination compatibility policy.
SME PSTATE signed tests must runtime-resolve the macOS 15.2 getter and call it
twice on one idle real vCPU. SME-capable hosts may compare same-vCPU results but
must not assume or log `PSTATE.SM` or `PSTATE.ZA`. A missing pre-15.2 symbol or
the getter's exact raw `HV_UNSUPPORTED` result may be treated as documented
unavailability; every unrelated error must fail. Tests must not call the setter,
change PSTATE, query maximum SVL, read Z/P/ZA/ZT0, run guest code, or treat the
flags as complete or safely restorable SME state.
SME system-register signed tests require macOS 15.2 and must capture `SMCR_EL1`,
`SMPRI_EL1`, and `TPIDR2_EL0` twice from one idle real vCPU. They may compare
same-vCPU results only with fixed failure messages and must verify that `Debug`
redacts all raw values. They must not log or format those values, write the
registers, query maximum SVL, read Z/P/ZA/ZT0, run guest code, or treat stable
readback as a portable or safely restorable SME state.
Cache-selection signed tests must capture CSSELR_EL1 from an idle real vCPU
without hard-coding or validating its architecturally unknown reset value. They
must not write the selector, query CCSIDR, execute ISB or cache maintenance, run
guest code, or treat CSSELR as cache topology or portable restore input.
Hardware-breakpoint signed tests must read `ID_AA64DFR0_EL1.BRPs`, capture only
the reported 1–16 `DBGBVR<n>_EL1` / `DBGBCR<n>_EL1` pairs from an idle real
vCPU, and assert shape rather than reset values. They must not log raw values,
write debug registers, enable breakpoints or monitor debug, change HVF debug-
register trap policy, execute guest/debug instructions, run the vCPU, or treat
the raw controls as safe restore input.
Hardware-watchpoint signed tests must read `ID_AA64DFR0_EL1.WRPs`, capture only
the reported 1–16 `DBGWVR<n>_EL1` / `DBGWCR<n>_EL1` pairs from an idle real
vCPU, and assert shape rather than reset values. They must not log raw values,
write debug registers, enable watchpoints or monitor debug, change HVF debug-
register trap policy, execute guest/debug instructions, run the vCPU, or treat
the raw address and control values as safe restore input.
Debug-control signed tests must remain observation-only: capture MDCCINT_EL1
and MDSCR_EL1 from an idle real vCPU without hard-coding or logging their raw
values. They must not call register or debug-trap setters, run guest debug
instructions, enable monitor debug or software stepping, or treat the two
fields as complete or safely restorable debug state.
Debug-trap signed tests must capture both Hypervisor.framework policy booleans
twice from an idle real vCPU without assuming, comparing, or logging their
values. They must not call either trap setter, run the vCPU, execute guest/debug
instructions, activate debug behavior, or treat host TDE/TDA-equivalent policy
as guest register state or safely restorable configuration.
Physical-timer signed tests require macOS 15 and must create the GIC before the
vCPU. They must keep CNTP disabled and masked, assert writable control bits
separately from derived ISTATUS, and avoid claiming that an absolute CVAL can be
restored without elapsed-time and interrupt-delivery policy.
Pointer-authentication key signed tests must use visibly non-secret sentinels,
must not enable or execute PAC instructions, and must assert that debug output
contains no raw key material. Failure assertions must not format actual key
values. Destroy the VM after capture rather than treating readback as feature
compatibility or a safe restore round trip.

## Stability Rules

Avoid arbitrary sleeps, fixed polling delays, and timeout increases that hide
races. Prefer explicit state, bounded channels, owned handles, temporary
directories, and public completion signals.

Tests must not share fixed global paths. Use unique temporary files or
directories and verify cleanup when ownership matters. Multiple tests and
multiple `bangbang` processes should not interfere unless the test is explicitly
checking conflict behavior.

Do not ignore HVF tests on hosts that support HVF. If an HVF test cannot run on
hosted CI, use the signed integration runner with `--allow-unsupported` so CI
still validates artifact preparation, compilation, and signing before skipping
execution on unsupported runners.

## Running Tests

Run the standard workspace checks before opening or updating a PR:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-targets --all-features --locked --exclude bangbang-hvf
cargo test -p bangbang-hvf --lib --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy -p bangbang --test executable_hvf_e2e --all-features --locked --target aarch64-apple-darwin -- -D warnings
cargo clippy -p bangbang-hvf --test hvf_lifecycle --all-features --locked --target aarch64-apple-darwin -- -D warnings
cargo clippy -p bangbang-hvf --test guest_boot --all-features --locked --target aarch64-apple-darwin -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
```

The explicit clippy commands cover signed integration targets declared with
`test = false`; ordinary `--all-targets` commands intentionally do not select
them.

Run signed HVF integration tests on macOS Apple Silicon without
`--allow-unsupported`:

```sh
scripts/run-integration-tests.sh
```

Run one signed integration test target when the change is narrower:

```sh
scripts/run-integration-tests.sh --test hvf_lifecycle
scripts/run-integration-tests.sh --test guest_boot
scripts/run-integration-tests.sh --test executable_hvf_e2e
```

Run only the process-level executable e2e test when the change is limited to
the `bangbang` process boundary:

```sh
cargo test -p bangbang --test process_e2e --all-features --locked
```

Run the same process-level e2e test against a signed `bangbang` executable:

```sh
scripts/run-signed-process-tests.sh
```

This builds and signs a temporary `bangbang` executable, then sets
`BANGBANG_PROCESS_E2E_BIN` so `process_e2e` launches that signed binary instead
of Cargo's default test binary. The script verifies process startup, API socket
serving, configuration requests, multi-process socket isolation, and clean
shutdown. It requires macOS Apple Silicon because the signed executable target
is `aarch64-apple-darwin`, but it does not start HVF or send `InstanceStart`.

Build a signed `bangbang` executable artifact for future HVF-backed process e2e
tests without running it:

```sh
scripts/build-signed-bangbang.sh --output .tmp/signed-bangbang/bangbang
```

This requires macOS `codesign` and the `aarch64-apple-darwin` Rust target. The
command only builds and signs the executable; HVF execution remains the job of
the signed integration runner.

Run executable-level HVF e2e through the signed integration runner:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e
```

This target runs the dedicated `executable_hvf_e2e` Cargo test target. It builds
and signs a temporary `bangbang` executable, prepares the pinned Firecracker
kernel, deterministic tiny initrd, and generated direct-boot ext4 rootfs,
starts `bangbang` as a child process, configures the VM through the Unix-socket
API or a Firecracker-shaped config file depending on the scenario, and waits for
the guest to write deterministic markers to host-observable outputs. The
tiny-initrd scenarios write `BANGBANG_BLOCK_WRITE_OK` to scratch block backing
files and include API/config-file coverage for configured serial output files.
The API-request, API-enabled config-file, and no-api config-file scenarios
verify vsock listener binding during startup and owned vsock listener cleanup
on shutdown. The API-request and API-enabled config-file scenarios verify
metrics and logger outputs after runtime `FlushMetrics`. The config-file guest
stop scenarios boot the tiny initrd's `/poweroff-init` or `/reboot-init`, which
invoke Linux reboot syscalls so the kernel issues PSCI `SYSTEM_OFF` or
`SYSTEM_RESET`, and verify that API-enabled and no-api `bangbang` processes
exit successfully. The
direct-rootfs scenarios boot the generated ext4 rootfs without an initrd. They
include a public `/serial` scenario that waits for
`BANGBANG_DIRECT_ROOTFS_BOOT_OK` in the configured serial output file, plus
scratch-drive scenarios that write `BANGBANG_DIRECT_ROOTFS_BLOCK_OK` through a
second writable drive. A boot-timer scenario starts the signed executable with
`--boot-timer`, boots the Firecracker rootfs-provided `/usr/local/bin/init`
wrapper, and waits for `Guest-boot-time` in the configured logger output after
that wrapper writes the Firecracker magic byte to the boot-timer MMIO address.
This verifies the public process/API/config-file/HVF path, including public
serial output redirection and minimal observability output. The executable HVF
e2e target also includes direct-rootfs MMDS v1 and v2 token-flow scenarios that
configure a `vmnet:shared` network interface, configure MMDS for that
interface, fetch a deterministic MMDS value from the guest through
`169.254.169.254`, and write host-observable markers to unique scratch drives.
It also includes a direct-rootfs entropy scenario that configures `/entropy`,
checks that the guest selected `virtio_rng` as the current hardware RNG, reads
from `/dev/hwrng`, and writes a host-observable marker only after a non-empty
read succeeds.
It also includes a direct-rootfs balloon scenario that configures `/balloon`,
checks that the guest bound a virtio-balloon driver, exercises the minimal
hinting start/stop command-state APIs, and writes a host-observable marker after
the driver path is visible.
Runtime `PATCH /balloon` target-size updates are covered by unit, API socket,
and process-session tests that verify stored config updates, active config-space
generation changes, and config interrupt signaling; they do not require a
separate signed guest scenario until periodic polling, host reclaim, or
reporting behavior is added. Guest-reported statistics queue records are covered
by runtime unit, API response, and process-session tests. Runtime statistics
interval updates are covered by unit, API socket, and process-session tests
because they update stored/active interval state without timer-driven guest
polling. Hinting queue guest-command acknowledgement, automatic host DONE
acknowledgement, and active-run range descriptor validation/recording are
covered by runtime unit and MMIO handler tests.
It also includes a direct-rootfs writeback block scenario that configures a
non-root data drive with `cache_type=Writeback`, writes through `/dev/vdb`,
calls `fsync` on the block-device file descriptor, and writes a host-observable
marker only after that flush returns.
It also includes a direct-rootfs pmem scenario that configures `/pmem/pmem0`
through the public API, waits for `BANGBANG_PMEM_READ_FLUSH_OK` in a scratch
drive, and then verifies the guest-written pmem marker in the host backing
file.
Because every configured network interface is bound to MMDS in these scenarios,
startup uses the process-local MMDS-only packet path and does not require
external vmnet packet movement.

Hosted macOS CI may use:

```sh
scripts/run-integration-tests.sh --allow-unsupported
```

That option is for CI-style build/sign validation on runners that cannot
execute HVF. Local Apple Silicon verification should omit it so unsupported or
misconfigured hosts fail.

## Guest Boot Artifacts

Guest boot and executable HVF e2e tests use the pinned Firecracker arm64 kernel,
a deterministic tiny initrd, and rootfs artifacts where their scenarios require
them. The integration runner prepares the relevant artifacts when `guest_boot`
or `executable_hvf_e2e` is selected. To prepare only the kernel cache, run:

```sh
scripts/fetch-firecracker-kernel.sh
```

The default cache lives under `.tmp/guest-artifacts`. Set
`BANGBANG_GUEST_ARTIFACTS_DIR` to use a different cache root. By default,
`scripts/fetch-firecracker-kernel.sh` stores the pinned kernel at
`.tmp/guest-artifacts/firecracker-ci/v1.15/aarch64/vmlinux-6.1.155`; when a
custom cache root is configured, the same relative path is used under that
root. The script verifies the pinned SHA-256 before reusing or installing the
cached kernel.

The `guest_boot` runner also generates a deterministic tiny initrd under
`.tmp/guest-artifacts/bangbang/guest-boot/` by default. That initrd contains its
own `/init`, so a rootfs drive is not required for the minimal guest boot
integration test. The test succeeds when the guest emits `BANGBANG_BOOT_OK` on
the internal serial console. The same signed target also includes a raw
virtio-block read scenario: the test configures one temporary drive whose first
sector contains `BANGBANG_BLOCK_READ_OK`, mounts `devtmpfs` from the tiny
`/init`, reads `/dev/vda`, and expects the marker to appear on serial. It also
mounts procfs and writes `/proc/cmdline` to serial between deterministic markers
so a root-drive scenario can verify guest-visible `root=/dev/vda ro` arguments.
A writable virtio-block scenario writes `BANGBANG_BLOCK_WRITE_OK` from the
guest to `/dev/vda`, and the host-side test verifies the marker in a scratch
backing file. A rootfs artifact scenario attaches the cached Firecracker
squashfs as a read-only root drive, mounts it from the tiny initrd, reads
`/mnt/etc/os-release`, and expects `BANGBANG_ROOTFS_READ_OK` plus stable Ubuntu
os-release content on serial. This verifies guest-visible rootfs access through
virtio-block.

The pinned Firecracker CI rootfs artifact can be prepared separately:

```sh
scripts/fetch-firecracker-rootfs.sh
```

By default this stores and verifies
`.tmp/guest-artifacts/firecracker-ci/v1.15/aarch64/ubuntu-24.04.squashfs` and
prints its path. The script verifies the pinned SHA-256 before reusing or
installing the cached squashfs. The upstream Firecracker artifact is a
read-only squashfs; do not mutate it in tests. The signed `guest_boot`
integration target uses this cached squashfs directly for its read-only rootfs
access scenario.

To prepare a local ext4 image from that squashfs, install the local tools and
request ext4 output:

```sh
brew install squashfs e2fsprogs
scripts/fetch-firecracker-rootfs.sh --format ext4
```

Homebrew's `e2fsprogs` package is keg-only, so `mkfs.ext4` is not normally on
`PATH`. The script first looks for `mkfs.ext4` on `PATH`, then checks
`$(brew --prefix e2fsprogs)/sbin/mkfs.ext4`. Set `BANGBANG_MKFS_EXT4` to
override the tool path. The generated ext4 image is stored under
`.tmp/guest-artifacts/bangbang/rootfs/`; tests that need writable rootfs state
should use a scratch copy of that image.

The ext4 preparation path intentionally does not require `sudo`. Files copied
into the generated ext4 image keep the local extraction ownership rather than
Firecracker's root-owned demo ownership. This is suitable for local development
artifacts and is not a substitute for a production rootfs build process.

The signed `guest_boot` and executable HVF e2e targets also validate a
deterministic direct-rootfs boot. For those scenarios,
`scripts/run-integration-tests.sh` prepares
`.tmp/guest-artifacts/bangbang/rootfs/ubuntu-24.04-512M-direct-boot-v28.ext4`
after confirming the host can execute HVF. The generated image is an ext4 copy
of the pinned Firecracker rootfs with a test-specific
`/bangbang-direct-rootfs-init` script added before image creation. The test
boots without the tiny initrd, attaches that ext4 image as a read-only root
drive, and passes `init=/bangbang-direct-rootfs-init`. The `guest_boot` target
expects deterministic serial markers plus Ubuntu os-release content from
`/etc/os-release`; one direct-rootfs executable HVF e2e scenario configures
public `/serial` output and waits for `BANGBANG_DIRECT_ROOTFS_BOOT_OK` in the
host output file. Most other direct-rootfs executable HVF e2e scenarios observe
guest success through a second writable scratch drive, using markers such as
`BANGBANG_DIRECT_ROOTFS_BLOCK_OK`, because they do not configure a public serial
output path. When the boot args also include `bangbang.mmds-fetch=1`, the same
init script configures the
first non-loopback guest interface with a link-local address, runs a bounded
`curl` request for `/meta-data/bangbang-marker`, and writes
`BANGBANG_MMDS_GUEST_FETCH_OK` to the scratch drive only after the expected
MMDS value is returned. With
`bangbang.mmds-v2-fetch=1`, it first requests a v2 token from
`/latest/api/token`, then fetches the same marker with the token header and
writes `BANGBANG_MMDS_V2_GUEST_FETCH_OK`. The init script emits only static
success or failure markers for this path; it must not print generated tokens or
metadata values. When the boot args include `bangbang.entropy-read=1`, the same
init script checks `/sys/class/misc/hw_random/rng_current` for `virtio_rng`,
reads bytes from `/dev/hwrng`, and writes
`BANGBANG_ENTROPY_GUEST_READ_OK` only after the read returns non-empty data.
When the boot args include `bangbang.balloon-check=1`, the same init script
checks the virtio bus for a device bound to the `virtio_balloon` driver and
writes `BANGBANG_BALLOON_GUEST_CHECK_OK` only after that driver binding is
visible.
When the boot args include `bangbang.memory-hotplug-check=1`, the same init
script checks the virtio bus for a device bound to `virtio_mem`, writes
`BANGBANG_MEMORY_HOTPLUG_GUEST_READY`, follows `dmesg` for the runtime
requested-size update, and writes `BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK`
only after the guest observes that update. The host-side e2e waits for the
ready marker, sends `PATCH /hotplug/memory`, verifies the public API requested
size, and then waits for the guest success marker.
When the boot args include `bangbang.rtc-check=1`, the same init script checks
that Linux exposes `/dev/rtc0` as a character device and finds PL031 RTC
evidence in sysfs, procfs, or dmesg before writing
`BANGBANG_RTC_GUEST_CHECK_OK`.
When the boot args include `bangbang.vmgenid-check=1`, the same init script
checks Linux device-tree evidence for `/vmgenid`, verifies the
`microsoft,vmgenid` compatible string and 16-byte `reg` property tuple, and
writes `BANGBANG_VMGENID_GUEST_CHECK_OK`.
When the boot args include `bangbang.vmclock-check=1`, the same init script
checks Linux device-tree evidence for a Firecracker-shaped `amazon,vmclock`
`ptp@...` node, verifies its 16-byte `reg` property tuple, checks that the
guest-visible region size is 4 KiB, and writes
`BANGBANG_VMCLOCK_GUEST_CHECK_OK`.
Startup VMClock restore and interrupt coverage is still intentionally limited:
runtime tests verify the initialized ABI fields, HVF unit tests verify
deterministic SPI allocation, and signed executable coverage proves only guest
visibility at startup. Do not treat this as signed guest VMClock restore or
generation-counter coverage.
When the boot args include `bangbang.block-writeback-flush=1`, the same init
script opens `/dev/vdb`, writes a deterministic pre-flush marker, calls `fsync`
on that block-device file descriptor, and writes
`BANGBANG_BLOCK_WRITEBACK_FLUSH_OK` only after the flush call returns.
When the boot args include `bangbang.pmem-read-flush=1`, the same init script
finds the first `/dev/pmem*` block device, reads a deterministic host marker,
writes a deterministic guest marker at a fixed offset, runs `sync` for the
device path, and emits `BANGBANG_PMEM_READ_FLUSH_OK` only after those steps
complete.
When the boot args include `bangbang.vsock-guest-connect=1`,
the same init script uses the rootfs-provided Python `AF_VSOCK` support to
connect to host CID 2 on the test port, exchange multiple ordered deterministic
guest and host payloads with a host Unix listener at the Firecracker-style
`uds_path_<PORT>` path, and write `BANGBANG_VSOCK_GUEST_CONNECT_OK` only after
every reply matches. The signed e2e also verifies the retained host stream
reports EOF after the guest closes the AF_VSOCK stream. With
`bangbang.vsock-guest-multistream=1`, Python opens two guest-initiated
AF_VSOCK streams to distinct host ports before payload exchange, sends distinct
guest payloads on both streams, waits for distinct host replies, and writes
`BANGBANG_VSOCK_GUEST_MULTISTREAM_OK` only after both streams complete. When
the boot args include `bangbang.vsock-host-connect=1`, Python instead binds and
listens on the test AF_VSOCK port, writes
`BANGBANG_VSOCK_HOST_CONNECT_READY` only after the guest listener is ready,
accepts the host's Firecracker-style `CONNECT <PORT>` request through the main
`uds_path` after the host consumes the `OK <local_port>` response, exchanges
multiple ordered deterministic guest and host payloads over the same stream, and
writes `BANGBANG_VSOCK_HOST_CONNECT_OK` only after every payload matches. The
signed e2e also verifies the retained host stream reports EOF after the guest
closes the accepted AF_VSOCK stream. With `bangbang.vsock-host-multistream=1`,
Python binds two guest AF_VSOCK listeners on distinct ports, reports ready only
after both listeners are active, accepts two host `CONNECT <PORT>` streams
through the main `uds_path`, sends distinct guest payloads on both streams,
waits for distinct host replies, and writes
`BANGBANG_VSOCK_HOST_MULTISTREAM_OK` only after both streams complete. These
checks prove the kernel mounted the virtio-block root drive as `/`, give
executable-boundary MMDS fetch coverage through the process-local MMDS-only
packet path, prove guest-visible virtio-rng reads through `/dev/hwrng`, prove
guest virtio-balloon driver binding, prove guest-visible virtio-mem driver
binding plus the runtime requested-size signal, prove guest-visible PL031 RTC
device discovery, prove guest-visible VMGenID device-tree evidence, prove the
current writeback virtio-block flush path, prove the current virtio-pmem
read/flush path, and cover guest-initiated plus host-initiated virtio-vsock
connection exchange through the signed executable, including narrow
multi-payload stream cases and multi-stream retention in both directions. They
do not claim that bangbang can boot an arbitrary distro image through its
default init, that full networking compatibility is complete, that RTC alarm
interrupts, VMGenID/VMClock restore signaling, VMClock guest e2e observation,
or broader RTC-adjacent time/identity behavior is supported, or that full
block, balloon, memory-hotplug, pmem, and vsock runtime behavior is complete.

bangbang appends Firecracker-style root-drive command-line arguments during
startup resource assembly when a configured drive has `is_root_device=true`.
Root drives with `partuuid` append `root=PARTUUID=<partuuid>`; other root
virtio-block drives append `root=/dev/vda`. Read-only root drives append `ro`,
and writable root drives append `rw`. Rootfs boot tests should still pass the
other boot args they need, for example:

```sh
console=ttyS0 reboot=k panic=1 pci=off
```

Set `is_read_only=true` when attaching the cached squashfs rootfs so the guest
receives `ro`. Use writable root mode only with a scratch copy of the generated
ext4 image.

## PR Expectations

Bug fixes should include a regression test unless the behavior cannot be tested
practically in the current scaffold. New public behavior should be tested
through the public CLI, API, crate, filesystem, or HVF boundary that users or
future code will rely on.

List only verification commands that were actually run on the reviewed head. If
a command is intentionally skipped, explain why it does not add useful signal
for the PR.
