# Firecracker v1.16.0 entropy closure contract

This checked #1475/#1666 ledger owns exactly seven Firecracker v1.16.0 entropy
identities. All seven are `implemented-and-verified`, including exact
native-v2 2.8 snapshot continuation and containment.

## Evidence keys

- **API/model** — strict request parsing and response serialization in
  `crates/api/src/http.rs`, API conversion in
  `crates/bangbang/src/api_server.rs`, and transactional preboot configuration
  plus public snapshot preflight in `crates/bangbang/src/vmm.rs`.
- **Runtime** — one-queue virtio-rng parsing, host randomness, request capping,
  dual token buckets, retry retention, publication-safe limiter accounting,
  metrics, and detached MMIO/PCI capture state in
  `crates/runtime/src/entropy.rs`; exact queue/limiter/pending/retry encoding
  and restore planning in `crates/runtime/src/snapshot_entropy_v2_8.rs`.
- **HVF** — exact MMIO/PCI placement in
  `crates/hvf/src/snapshot_v2_entropy_platform.rs` and fresh OS source,
  scheduler, notifier, route, endpoint, quiescence-guard, and retry
  reconstruction in `crates/hvf/src/startup.rs`.
- **Focused validation** — route/model/controller tests in
  `crates/api/src/http.rs` and `crates/bangbang/src/{api_server,vmm}.rs`, plus
  queue, source, limiter, retry, failure-order, metric, capture-invariant, and
  redaction tests in `crates/runtime/src/{entropy,metrics}.rs`.
- **Signed owner validation** —
  `crates/hvf/tests/hvf_lifecycle.rs` covers capture-ready traversal, exact
  MMIO/PCI retry restoration, deterministic fresh-source factory counts,
  recapture, rollback, and teardown.
- **Signed public validation** —
  `crates/bangbang/tests/executable_hvf_e2e.rs::signed_executable_certifies_native_v2_entropy_snapshot_continuation`
  proves fresh-process `/dev/hwrng` continuation from a retained request over
  both transports and both product shapes.
- **Signed contained validation** —
  `crates/launcher/tests/production_bundle_e2e.rs::normal_bundle_certifies_native_v2_entropy_snapshot_continuation_and_containment`
  proves the same exact state through the normal launcher/App Sandbox worker,
  typed grants, pathname replacement, recapture, malformed input,
  cancellation, both death orders, and exact cleanup.

## Exact seven-record ledger

| Identity | Final disposition | Exact contract and evidence |
| --- | --- | --- |
| `api-operation:PUT /entropy` | implemented and verified | Strict preboot replacement accepts the optional Firecracker-shaped limiter, rejects malformed or post-start requests without mutation, and attaches exactly one selected MMIO or PCI owner. API/model and signed public validation. |
| `api-path:/entropy` | implemented and verified | Complete strict PUT-only route, method, state, JSON, and error behavior. API route and signed public validation. |
| `api-property:EntropyDevice.rate_limiter` | implemented and verified | Optional bandwidth and operations buckets preserve exact size, one-time burst, and refill time; absent or empty limiting remains unconfigured. Runtime limiting retains one throttled descriptor, schedules the earliest retry, and preserves exact bucket state at capture. Focused and signed throttling validation. |
| `api-property:FullVmConfiguration.entropy` | implemented and verified | Nullable committed entropy configuration appears exactly in `/vm/config` and changes only after a successful preboot transaction. API/controller and signed configuration validation. |
| `api-schema:EntropyDevice` | implemented and verified | Complete strict optional-`rate_limiter` schema with unknown-field/type rejection, exact configuration projection, and selected MMIO/PCI startup execution. API/model and signed validation. |
| `corpus:entropy` | implemented and verified | Strict live behavior plus exact native-v2 2.8 serialization/restoration are complete. Required serial, optional unchanged profile-3 storage, and optional entropy compose over matching MMIO or PCI; signed direct and contained guests prove retained-request continuation, explicit/automatic resume, recapture, immutable clones, fresh destination ownership and metrics, hostile lifecycle cleanup, and redaction. |
| `semantic.device:entropy-queues-limits-metrics-and-state` | implemented and verified | Live queue processing, the 64-KiB request cap, host entropy failures, dual-bucket limiting, retry wakeups, metrics, MMIO/PCI ownership, failure ordering, redaction, and cleanup now compose with exact persisted queue/limiter/pending/retry state. Every destination constructs fresh source/scheduler/notifier/route/endpoint owners and completes the retained request without another guest kick. |

## Observable live, metrics, and exact 2.8 contract

- Every writable request is capped at 64 KiB and filled from the host operating
  system entropy source. Source failure completes the descriptor with zero
  bytes and records a distinct host failure; guest memory never receives stale
  or partially prepared host data.
- Optional operations and bandwidth buckets are evaluated together. A
  throttled descriptor is returned to the available ring, retained exactly
  once, and retried without another guest notification at the earliest required
  deadline. Buffer allocation, completed-length, guest-write, and used-ring
  failures restore the exact pre-consumption limiter snapshot. A completed
  zero-length host-source failure remains a consumed request.
- The `entropy` metrics object exposes `activate_fails`, `entropy_event_fails`,
  `entropy_event_count`, `entropy_bytes`, `host_rng_fails`,
  `entropy_rate_limiter_throttled`, and `rate_limiter_event_count`. Event count
  advances when a descriptor is popped, before parsing or limiter admission,
  and is retained across throttle undo and later queue errors. Host RNG and its
  paired event failure advance only when filling that popped request fails; a
  pre-dispatch source-provider acquisition failure remains an internal
  diagnostic. Counts are per device, saturating, owner-snapshot coherent, and
  reported through the ordinary metrics pipeline. MMIO, PCI, and restored
  devices attach the fresh session owner before publication, so activation
  failures never leak between source, destination, or reused device owners.
- Detached state contains external configuration, available and negotiated
  features, activation, exact one-queue geometry/ranges/cursors, limiter
  configuration and redacted budget/burst/refill-age state, the single pending
  descriptor, and a host-time-free retry disposition. Capture rejects feature,
  activation, queue, mapping, cursor, external-limiter, pending-descriptor, and
  scheduler disagreement. No random bytes, guest-memory borrow, lock, endpoint,
  host handle, metric value, or `Instant` escapes. Exact native-v2 2.8 encodes
  this state after required kind 8 serial and optional unchanged kind 7
  profile-3 storage.
- A paused process-supervisor transaction quiesces the entropy retry publisher
  and requires exactly one configured MMIO or PCI owner. MMIO captures under
  dispatcher ownership; PCI captures device and canonical transport under one
  endpoint lock. The result retains MMIO region/IRQ or PCI SBDF/BAR placement.
  Native-v1 creation still performs this preflight before optional-profile
  rejection and writes no entropy component; exact 2.3 through 2.7 readers
  retain their historical profiles unchanged.
- On load, retained guest queue, limiter buckets, pending descriptor, retry
  intent, and transport placement bind to the immutable state/memory pair.
  The destination creates a fresh host OS source, empty metrics set, scheduler,
  notifier, route, endpoint, and host-clock-relative deadline. Delayed retry
  completes the already outstanding Linux request without another queue kick.
- Signed Linux guests prove the same marker-gated protocol over both transports
  and both entropy-only-relative-to-serial and storage-plus-entropy products:
  a first nonempty `/dev/hwrng` read, observable dual-bucket throttling, source
  termination, fresh explicit and automatic destinations, restored nonempty
  completion, retry metrics, recapture, immutable clones, and clean shutdown.

## Explicit compatibility boundary

Exact 2.8 is bangbang-native. It does not serialize random output, the host
source or its identity, metrics, scheduler/notifier handles, route objects,
endpoint ownership, or absolute host time. The deterministic source factory is
test evidence only and adds no artifact or public API field. Firecracker
artifact-byte compatibility, deterministic entropy output, native-v2
Uffd/Diff/editing, external authentication or encryption, and broad cross-host
token-clock or OS-source portability are not claimed.
