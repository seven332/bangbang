# Firecracker Validation Matrix

This matrix summarizes bangbang's current Firecracker-facing compatibility
coverage. Detailed endpoint behavior, field policy, platform limits, and
compatibility rationale remain in
[Firecracker Compatibility Scope](firecracker-compatibility.md).

## Status Vocabulary

- `implemented`: the public behavior exists for the documented subset.
- `partial`: an initial subset works, but important Firecracker behavior is
  still tracked by the related issue.
- `recognized unsupported`: the API shape is parsed or recognized before
  returning a Firecracker-shaped fault.
- `deferred`: the behavior needs a larger capability or backend design.
- `platform-limited`: the Firecracker feature depends on Linux-specific
  mechanisms or a host facility that does not map directly to macOS/HVF.

## Validation Layers

- `unit`: crate-local Rust tests for parsers, state, error formatting, and
  backend-neutral helpers.
- `api socket`: in-process API server tests over a real Unix socket.
- `process e2e`: unsigned executable tests in `crates/bangbang/tests/`.
- `signed process`: `scripts/run-signed-process-tests.sh`.
- `signed HVF`: `scripts/run-integration-tests.sh` targets that create HVF
  resources or boot guests.
- `docs`: compatibility, security, testing, or review documentation.

## Matrix

| Area | Current status | Primary validation | Related issue | Notes |
| --- | --- | --- | --- | --- |
| Process CLI and API socket | partial | unit, api socket, process e2e, signed process | #536, #545 | Firecracker-shaped startup args, API socket binding, config-file startup, no-api mode, payload limits, cleanup, and selected process-local MMDS/startup metrics isolation are covered for the current subset. Linux jailer/seccomp behavior is platform-limited. |
| Instance/version/config reads | implemented | unit, api socket, process e2e | #536 | `GET /`, `GET /version`, `GET /vm/config`, and `GET /machine-config` expose accumulated supported state for the current subset. Unsupported config sections are omitted until modeled. |
| Machine and boot configuration | partial | unit, api socket, process e2e, signed HVF | #538 | Pre-boot machine config, empty/no-op CPU config, boot source, kernel/initrd loading, FDT generation, direct-rootfs boot paths, and current multi-vCPU startup rejection are covered. Non-empty custom CPU templates, multi-vCPU execution, and broader CPU feature behavior remain deferred. |
| Drives and virtio-block | partial | unit, api socket, process e2e, signed HVF | #539, #916 | Initial drives, cache policy handling, block read/write, root-drive boot args, basic guest-visible block behavior, signed executable writeback block flush coverage, and runtime backing refresh for existing drives are covered. Hotplug, removal, and rate limiting remain deferred. |
| Network and MMDS | partial | unit, api socket, process e2e, signed HVF | #540 | Initial network config, runtime no-op PATCH for existing interfaces, vmnet mode selection, process-local MMDS, internal guest-visible MMDS packet handling, and signed executable guest MMDS v1 plus v2 token-flow fetches are covered. Broader public packet movement, configured rate limiting, and real runtime network mutation remain deferred. |
| Virtio-vsock | partial | unit, api socket, process e2e, signed HVF | #541 | Initial config, startup listener ownership, connection setup, bounded buffering, reset/shutdown cleanup, minimal partial shutdown state including same-window receive shutdown ordering, executable startup paths, signed executable guest-initiated and host-initiated EOF cleanup, signed executable guest-initiated and host-initiated multi-payload connection exchanges, and narrow signed executable guest-initiated and host-initiated multi-stream exchanges are covered. Full throughput-oriented streaming semantics, Firecracker's full graceful-shutdown timeout/kill-queue behavior, credit accounting, and broader socket lifecycle parity remain deferred. |
| Observability: logger, metrics, serial | partial | unit, api socket, process e2e, signed process, signed HVF | #542, #918 | Minimal logger output with level, origin, and module-prefix filtering, metrics, startup timing, immediate startup metrics flushing, explicit and 60-second periodic metrics flushes, selected GET, PUT, PATCH including parser failures for endpoints with Firecracker-shaped request metric fields, `/actions` API request counters, selected deprecated HTTP API usage counter, minimal `logger.missed_metrics_count` and `logger.missed_log_count`, and initrd plus direct-rootfs serial output paths are covered. Full Firecracker counters and full log routing remain deferred. |
| VM lifecycle and run-loop control | partial | unit, api socket, process e2e, signed HVF | #537 | `InstanceStart`, Running transition, retained boot worker status, unsupported pause/resume request routing, guest PSCI `SYSTEM_OFF`/`SYSTEM_RESET` process exits, and non-success terminal process failures are covered. Public pause/resume, reboot-in-place, and fine-grained guest error exit-code parity remain deferred. |
| Snapshots and restore | recognized unsupported | unit, api socket, process e2e, docs | #543 | Snapshot create/load request shapes are parsed, malformed bodies fail at the parser, and valid bodies route through VMM state/action policy before returning state-specific or snapshot-specific unsupported faults. Real paused run-loop ownership, guest-memory persistence, HVF vCPU/VM state capture, dirty tracking, snapshot format, and device-state persistence remain deferred until the macOS/HVF design is split into implementation work. |
| Remaining Firecracker devices | partial | unit, api socket, process e2e, signed HVF, docs | #544, #797, #800, #802, #804, #806, #808, #810, #812, #814, #815, #818, #869, #873, #875, #877, #888, #890, #892, #894, #896, #898, #900, #902, #904, #905, #908, #910, #912, #914, #920 | Balloon now has a pre-boot control-plane config model: valid `PUT /balloon` stores public configuration from API and config-file startup paths, `GET /balloon` and `GET /vm/config` expose it, runtime can derive a backend-neutral virtio-balloon identity, feature-bit, queue-metadata, and 12-byte config-space foundation from the stored config, `InstanceStart` can attach the current virtio-mmio/FDT shell, runtime `PATCH /balloon` can update the stored target and active `num_pages` config-space value through the boot run-loop command path with config-generation and config-interrupt signaling, and runtime plus HVF boot-loop notification dispatch can parse inflate and deflate PFN ranges, validate them against mapped guest memory, acknowledge descriptor heads with zero-length used-ring completions, update internal inflated-page accounting for completed descriptors, and signal queue interrupts. Signed executable e2e coverage now proves the public `/balloon` API path can boot a direct-rootfs guest that binds the `virtio_balloon` driver; balloon statistics, hinting, reporting, and host reclaim remain deferred. Valid pre-boot pmem requests without configured rate limiters store Firecracker-shaped configuration from API and config-file startup paths, appear in `GET /vm/config`, make `InstanceStart` open and validate non-zero regular host backing files while retaining handles, mmap those files to 2 MiB-aligned host ranges during startup preparation, assign deterministic non-overlapping 2 MiB-aligned guest physical ranges after the aarch64 MMIO64 gap while skipping current guest RAM, copy prepared mappings into HVF-compatible anonymous shadows, register those shadows with HVF after DRAM using read-only or read/write non-executable permissions, write writable shadows back to the backing file for guest queue-driven flush requests and after clean unmap while skipping writeback after failed unmap cleanup, and attach one virtio-mmio/FDT node per prepared pmem device with a backend-neutral virtio-pmem identity, queue metadata, feature-bit, alignment constant, config-space `start`/`size` values, and flush queue completion handling. Signed executable e2e coverage now proves the public `/pmem/{id}` API path can boot a direct-rootfs guest that reads the host marker and flushes a guest marker back to the pmem backing file; root-device semantics, runtime updates, rate limiting, dirty-range tracking, direct file-backed HVF mapping, and hot-unplug remain deferred. Memory hotplug request shapes are recognized or parsed before returning device-specific unsupported faults. Valid pre-boot entropy requests without configured rate limiters store public configuration, appear in `GET /vm/config`, feed `InstanceStart` so the existing HVF virtio-rng MMIO/FDT device can use the session-owned host OS randomness source, and have signed executable e2e coverage that proves a direct-rootfs guest reads non-empty data from `/dev/hwrng`. Entropy remains outside current API request metrics output because Firecracker does not define entropy request counters. Entropy rate limiting and other remaining device implementations still need separate designs. |
| macOS isolation and platform limits | partial | docs, process e2e | #545 | Security docs cover current socket, host-path, entitlement, and multi-process boundaries. macOS sandboxing, launcher/resource broker, and stricter host-path policy need follow-up work. |
| Validation matrix maintenance | implemented | docs | #546 | Future capability PRs should update this matrix when support status or validation layers change. Full upstream Firecracker test-suite mapping remains deferred. |

## Update Rule

When a PR changes Firecracker-facing behavior, update this matrix if it changes
support status, adds or removes a validation layer, or moves work between
implemented, partial, deferred, recognized unsupported, or platform-limited
states.
