# Firecracker v1.16.0 entropy closure contract

This ledger is the checked closure record for #1475, the third delivery slice
of #1440 under #1348. It covers exactly seven directly owned Firecracker
v1.16.0 entropy identities. Five API operation, path, property, and schema
identities are `implemented-and-verified`. Exactly `corpus:entropy` and
`semantic.device:entropy-queues-limits-metrics-and-state` remain
`audit-required` because their complete upstream claims include optional-device
snapshot serialization and restore, which Wave 6 owns.

The generated source manifest remains 381 identities, the overlay retains 37
local semantic identities and 418 total records, and this reconciliation moves
the global disposition counts from 181/217/3/17 to 186/212/3/17.

## Evidence keys

- **API/model** — strict request parsing and response serialization in
  `crates/api/src/http.rs`, API conversion in
  `crates/bangbang/src/api_server.rs`, and transactional preboot configuration
  plus public snapshot preflight in `crates/bangbang/src/vmm.rs`.
- **Runtime** — one-queue virtio-rng parsing, host randomness, request capping,
  dual token buckets, retry retention, publication-safe limiter accounting,
  metrics, and detached MMIO/PCI capture state in
  `crates/runtime/src/entropy.rs`; selected-owner traversal in
  `crates/runtime/src/startup.rs`.
- **HVF** — exact MMIO/PCI owner, transport-placement, quiescence-guard, and
  retry-scheduler reconciliation in `crates/hvf/src/startup.rs`.
- **Focused validation** — route/model/controller tests in
  `crates/api/src/http.rs` and `crates/bangbang/src/{api_server,vmm}.rs`, plus
  queue, source, limiter, retry, failure-order, metric, capture-invariant, and
  redaction tests in `crates/runtime/src/{entropy,metrics}.rs`.
- **Signed owner validation** —
  `crates/hvf/tests/hvf_lifecycle.rs::capture_ready_entropy_traverses_signed_mmio_and_pci_owners`.
- **Signed public validation** —
  `crates/bangbang/tests/executable_hvf_e2e.rs` proves throttled repeated
  `/dev/hwrng` reads, pause/capture-ready traversal/resume, metrics, and clean
  shutdown through both MMIO and product PCI.

## Exact seven-record ledger

| Identity | Final disposition | Exact contract and evidence |
| --- | --- | --- |
| `api-operation:PUT /entropy` | implemented and verified | Strict preboot replacement accepts the optional Firecracker-shaped limiter, rejects malformed or post-start requests without mutation, and attaches exactly one selected MMIO or PCI owner. API/model and signed public validation. |
| `api-path:/entropy` | implemented and verified | Complete strict PUT-only route, method, state, JSON, and error behavior. API route and signed public validation. |
| `api-property:EntropyDevice.rate_limiter` | implemented and verified | Optional bandwidth and operations buckets preserve exact size, one-time burst, and refill time; absent or empty limiting remains unconfigured. Runtime limiting retains one throttled descriptor, schedules the earliest retry, and preserves exact bucket state at capture. Focused and signed throttling validation. |
| `api-property:FullVmConfiguration.entropy` | implemented and verified | Nullable committed entropy configuration appears exactly in `/vm/config` and changes only after a successful preboot transaction. API/controller and signed configuration validation. |
| `api-schema:EntropyDevice` | implemented and verified | Complete strict optional-`rate_limiter` schema with unknown-field/type rejection, exact configuration projection, and selected MMIO/PCI startup execution. API/model and signed validation. |
| `corpus:entropy` | audit required | All applicable live API/device behavior, host randomness, limiting, retry, metrics, exact owner traversal, and detached state are implemented. **[Wave 6 #1490](https://github.com/seven332/bangbang/issues/1490)** owns optional-device state encoding, artifact integration, restore construction, migration/clone behavior, portability policy, and signed restored-guest outcomes. |
| `semantic.device:entropy-queues-limits-metrics-and-state` | audit required | Live queue processing, the 64-KiB request cap, host entropy failures, dual-bucket limiting, retry wakeups, metrics, MMIO/PCI ownership, capture-ready state, failure ordering, redaction, and cleanup are implemented. **[Wave 6 #1490](https://github.com/seven332/bangbang/issues/1490)** owns serialized/restored entropy state and aggregate artifact/portability certification. |

## Observable live, metrics, and capture-ready contract

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
  `entropy_rate_limiter_throttled`, and `rate_limiter_event_count`. Counts are
  per device, saturating, and reported through the ordinary metrics pipeline.
- Detached state contains external configuration, available and negotiated
  features, activation, exact one-queue geometry/ranges/cursors, limiter
  configuration and redacted budget/burst/refill-age state, the single pending
  descriptor, and a host-time-free retry disposition. Capture rejects feature,
  activation, queue, mapping, cursor, external-limiter, pending-descriptor, and
  scheduler disagreement. No random bytes, guest-memory borrow, lock, endpoint,
  host handle, or `Instant` escapes.
- A paused process-supervisor transaction quiesces the entropy retry publisher
  and requires exactly one configured MMIO or PCI owner. MMIO captures under
  dispatcher ownership; PCI captures device and canonical transport under one
  endpoint lock. The result retains MMIO region/IRQ or PCI SBDF/BAR placement.
  Native-v1 creation performs this preflight before optional-profile rejection
  and artifact publication but intentionally writes no entropy bytes yet.
- Signed Linux guests prove the same marker-gated protocol over both transports:
  a first `/dev/hwrng` read, host-controlled continuation, observable limiter
  throttling, paused capture-ready traversal, resume, repeated nonempty reads,
  retry metrics, and clean shutdown.

## Explicit Wave 6 handoff

This closure intentionally creates no entropy byte encoding or compatibility
version. Wave 6 must integrate the detached value into an optional-device
artifact, define versioning and validation, reconstruct MMIO/PCI live owners,
restore exact queue/limiter/pending-retry state against a fresh host clock,
re-establish the scheduler, reconcile external configuration, and prove
restored Linux entropy reads and limiting behavior. Only after those outcomes
may the two retained aggregate records become terminal. Firecracker artifact
compatibility, cross-host token-clock identity, preservation of random bytes,
and deterministic entropy output are not implied by this live closure.
