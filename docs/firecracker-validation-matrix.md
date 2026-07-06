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
| Drives and virtio-block | partial | unit, api socket, process e2e, signed HVF | #539 | Initial drives, cache policy handling, block read/write, root-drive boot args, and basic guest-visible block behavior are covered. Runtime update/hotplug remains deferred. |
| Network and MMDS | partial | unit, api socket, process e2e, signed HVF | #540 | Initial network config, vmnet mode selection, process-local MMDS, internal guest-visible MMDS packet handling, and signed executable guest MMDS v1 plus v2 token-flow fetches are covered. Broader public packet movement, rate limiting, and runtime updates remain deferred. |
| Virtio-vsock | partial | unit, api socket, process e2e, signed HVF | #541 | Initial config, startup listener ownership, connection setup, bounded buffering, reset/shutdown cleanup, executable startup paths, signed executable guest-initiated and host-initiated EOF cleanup, signed executable guest-initiated and host-initiated multi-payload connection exchanges, and narrow signed executable guest-initiated and host-initiated multi-stream exchanges are covered. Full throughput-oriented streaming semantics, half-close behavior, credit accounting, and broader socket lifecycle parity remain deferred. |
| Observability: logger, metrics, serial | partial | unit, api socket, process e2e, signed process, signed HVF | #542 | Minimal logger output with level, origin, and module-prefix filtering, metrics, startup timing, immediate startup metrics flushing, explicit and 60-second periodic metrics flushes, selected GET, PUT, PATCH including network, memory hotplug, and pmem API request counters, `/actions` API request counters, selected deprecated HTTP API usage counter, minimal `logger.missed_metrics_count` and `logger.missed_log_count`, and serial output paths are covered. Full Firecracker counters and full log routing remain deferred. |
| VM lifecycle and run-loop control | partial | unit, api socket, process e2e, signed HVF | #537 | `InstanceStart`, Running transition, retained boot worker status, unsupported pause/resume request routing, guest PSCI `SYSTEM_OFF`/`SYSTEM_RESET` process exits, and non-success terminal process failures are covered. Public pause/resume, reboot-in-place, and fine-grained guest error exit-code parity remain deferred. |
| Snapshots and restore | recognized unsupported | unit, api socket, process e2e | #543 | Snapshot create/load request shapes are parsed, malformed bodies fail at the parser, and valid bodies route through VMM state/action policy before returning state-specific or snapshot-specific unsupported faults. Real memory, vCPU, and device-state persistence remains deferred. |
| Remaining Firecracker devices | recognized unsupported | unit, api socket, process e2e | #544 | Balloon, pmem, entropy, and memory hotplug request shapes are recognized or parsed before returning device-specific unsupported faults. Balloon, entropy, pmem, and memory hotplug valid requests now route through the VMM state/action policy; process e2e covers representative valid requests. Device implementation needs separate designs. |
| macOS isolation and platform limits | partial | docs, process e2e | #545 | Security docs cover current socket, host-path, entitlement, and multi-process boundaries. macOS sandboxing, launcher/resource broker, and stricter host-path policy need follow-up work. |
| Validation matrix maintenance | implemented | docs | #546 | Future capability PRs should update this matrix when support status or validation layers change. Full upstream Firecracker test-suite mapping remains deferred. |

## Update Rule

When a PR changes Firecracker-facing behavior, update this matrix if it changes
support status, adds or removes a validation layer, or moves work between
implemented, partial, deferred, recognized unsupported, or platform-limited
states.
