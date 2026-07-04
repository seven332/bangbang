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

Use HVF integration tests for behavior that creates HVF VMs, vCPUs, GIC state,
mapped guest memory, signed test binaries, or guest boot execution. These tests
live in `crates/hvf/tests/` and must run through
`scripts/run-integration-tests.sh` so the binaries are signed with the
`com.apple.security.hypervisor` entitlement. Do not add real HVF tests to the
unsigned workspace test path.

## What To Cover

For CLI and API changes, cover successful requests, unknown options or fields,
empty values, duplicate values, malformed inputs, exit codes, HTTP status codes,
and Firecracker-shaped response bodies.

For host filesystem paths, cover missing paths, directories, unsupported file
types, redacted error messages, cleanup ownership, and failure atomicity. A
failed operation should not partially mutate accepted configuration, guest
memory, or host resources.

For guest memory, address, and range logic, cover exact-fit success, one-past
failure, overflow failure, overlapping ranges, and no-partial-mutation behavior.

For process, socket, and multi-bangbang behavior, cover unique resource names,
stale socket handling, shutdown cleanup, replacement races, and concurrent runs
where practical.

For HVF and FFI code, cover resource creation and destruction, platform gating,
error translation, unsupported exits or registers, cancellation, and cleanup
after partial setup failure.

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
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
```

Run signed HVF integration tests on macOS Apple Silicon without
`--allow-unsupported`:

```sh
scripts/run-integration-tests.sh
```

Run one signed integration test target when the change is narrower:

```sh
scripts/run-integration-tests.sh --test hvf_lifecycle
scripts/run-integration-tests.sh --test guest_boot
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

Hosted macOS CI may use:

```sh
scripts/run-integration-tests.sh --allow-unsupported
```

That option is for CI-style build/sign validation on runners that cannot
execute HVF. Local Apple Silicon verification should omit it so unsupported or
misconfigured hosts fail.

## Guest Boot Artifacts

Guest boot tests use the pinned Firecracker arm64 kernel, a deterministic tiny
initrd, and the pinned Firecracker rootfs artifact. The integration runner
prepares them when `guest_boot` is selected. To prepare only the kernel cache,
run:

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

The signed `guest_boot` target also validates a deterministic direct-rootfs
boot. For that scenario, `scripts/run-integration-tests.sh` prepares
`.tmp/guest-artifacts/bangbang/rootfs/ubuntu-24.04-512M-direct-boot-v4.ext4`
after confirming the host can execute HVF. The generated image is an ext4 copy
of the pinned Firecracker rootfs with a test-specific
`/bangbang-direct-rootfs-init` script added before image creation. The test
boots without the tiny initrd, attaches that ext4 image as a read-only root
drive, passes `init=/bangbang-direct-rootfs-init`, and expects deterministic
serial markers plus Ubuntu os-release content from `/etc/os-release`. This
proves the kernel mounted the virtio-block root drive as `/`; it does not claim
that bangbang can boot an arbitrary distro image through its default init.

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
