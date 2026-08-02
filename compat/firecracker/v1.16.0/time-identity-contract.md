# Firecracker v1.16.0 time and identity contract

This ledger is the checked terminal record for the aarch64 PL031 RTC, VMGenID,
VMClock, public live/capture-ready PVTime, native-v2 portable time/clone
restore, and production recapture portions of exactly one aggregate identity:
`semantic.device:rtc-vmclock-vmgenid-and-pvtime`. That identity is
`implemented-and-verified` through the exact
[Wave 6 snapshot contract](snapshot-wave6-contract.md). The required native-v2
time component is carried unchanged from exact 2.3 through Full 2.12 and Diff
2.13 and composes with every current optional-device product.

## Evidence keys

- **Typed ABI and codecs** — `crates/runtime/src/vmclock.rs` models and
  validates the complete 112-byte little-endian VMClock v1 ABI, and
  `crates/runtime/src/snapshot_device.rs` captures it into the bounded
  `BANGDEV\0` 1.1.0 profile while retaining 1.0.0 load compatibility.
  `crates/{runtime,hvf}/src/{snapshot_format_v2,snapshot_v2}.rs` advance the
  native-v2 writer to `2.3.0` and append singleton `BANGTM2\0` kind 6 after
  every vCPU, with exact PL031/PVTime/VMGenID/VMClock policies and no source
  VMGenID, pointer, `Instant`, or absolute time anchor.
- **Capture and preparation** — `crates/hvf/src/startup.rs` captures the live
  page only inside a completed paused ownership boundary. Native-v2 capture
  sandwiches owner state and topology-ordered PVTime between stable lifecycle
  observations, verifies VMClock/PVTime guest bytes and VMGenID owner
  agreement, and creates a fresh memory binding only after portable state
  validation. `crates/runtime/src/startup.rs` requires a valid even sequence,
  exact encoded-page agreement, canonical identity destinations, and fresh
  destination VMGenID preparation.
- **Restore transactions** — `crates/runtime/src/vmclock.rs` publishes odd
  sequence, release fence, incremented disruption and generation counters,
  release fence, and even sequence. Native-v1 retains its public ordered
  restore. `crates/hvf/src/snapshot_v2_platform.rs` additionally preflights the
  complete v2 graph, guest ABI/PVTime bytes, runners, SPI lines, and signaler;
  reconstructs PL031 and PVTime; completes VMGenID replacement/notification,
  then VMClock update/notification; imports lifecycle state; and only then
  publishes the focused destination `Paused`.
- **Failure policy** —
  `crates/hvf/src/{startup,snapshot_restore,snapshot_v2_platform}.rs` separates
  mutation-free failures from committed guest-memory or notification failures.
  Only a completely cleaned, precommit destination is retryable; every failure
  after the first committed VMGenID write, or at any later identity/lifecycle/
  publication stage, is terminal and the destination never runs.
- **RTC policy** — `crates/runtime/src/snapshot_device.rs` reconstructs a fresh
  PL031 against destination wall clock and verifies its match, control, mask,
  raw-status, and masked-status registers are zero. The aarch64 FDT intentionally
  supplies no RTC interrupt, matching the pinned Firecracker shape.
- **PVTime ABI and placement** — `crates/runtime/src/pvtime.rs` owns the exact
  64-byte little-endian revision-0/attributes-0 stolen-time structure and plans
  one aligned, nonoverlapping record per topology-ordered vCPU. Ordinary boot
  initializes those records below VMGenID and retains their exact IPAs. No FDT
  node is required by the standard SMCCC discovery contract.
- **HVF measurement, accounting, and firmware gate** — the HVF `ffi`, `pvtime`,
  `runner`, and `psci` modules runtime-resolve the macOS 11
  `hv_vcpu_get_exec_time` primitive and sample it with monotonic wall time on
  the permanent owner thread. Each admitted runnable run publishes the prior
  value, then commits saturated `wall - execution` time; canceled and virtual-
  timer-idle windows are discarded. A retained aligned atomic writer performs
  dirty-aware little-endian release stores. Exact 64-bit `PV_TIME_FEATURES` and
  `PV_TIME_ST` dispatch is enabled only after complete topology configuration;
  missing measurement support remains fail-closed and 32-bit aliases remain
  unsupported.
- **Capture continuity** — a completed pause barrier publishes and returns only
  topology-ordered cumulative per-vCPU values. No source clock or execution
  baseline crosses the boundary, so each native-v2 destination starts a fresh
  run window and cannot charge snapshot downtime. A restored paused owner can
  recapture into a fresh `2.3.0` memory/state identity and restore again.
- **Focused and signed validation** — runtime and HVF unit tests cover ABI
  bytes, validation, wrapping counters, partial writes, legacy decode, encoded
  memory mismatch, destination RTC reconstruction, ordering, and retryability.
  `crates/bangbang/tests/executable_hvf_e2e.rs` restores the same immutable pair
  into fresh signed HVF processes; guest code observes both VMGenID halves
  change, a stable even VMClock sequence with changed disruption/generation
  counters, and a destination RTC value no earlier than its captured value.
  Signed `hvf_lifecycle` tests additionally prove the public HVF execution-time
  symbol on Apple Silicon and owner-thread cumulative measurements across real
  guest execution. Its three-vCPU native-v2 case proves repeated immutable
  loads receive distinct VMGenIDs, guest acknowledgement orders VMGenID before
  VMClock, PL031 uses destination time, PVTime continuity excludes downtime,
  ordinary progress waits for explicit resume, and a recaptured clone restores
  again. Signed `guest_boot` certification proves Linux emits
  `stolen time PV`, aggregate `/proc/stat` steal ticks become nonzero and
  monotonic under a hidden real-delay contention probe, stay unchanged after
  the probe is disabled, and topology capture values stay unchanged across a
  completed pause interval. The fixed-production
  `normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci` matrix
  invokes `assert_production_snapshot_time_identity_transition` in every
  rooted/rootless x MMIO/PCI cell: each retained source and Paused recapture
  pair is canonical and independently memory-bound, keeps the exact
  profile/machine/topology/transport/device graph after normalizing only
  numeric limiter ages and retry countdowns, keeps their presence/type and
  every non-VMClock time fact, and has nonzero unequal VMGenIDs plus a changed
  exact 112-byte VMClock fingerprint without logging either value.

## Exact one-record ledger

| Identity | Current disposition | Exact contract and remaining handoff |
| --- | --- | --- |
| `semantic.device:rtc-vmclock-vmgenid-and-pvtime` | `implemented-and-verified` | PL031 startup/metrics/destination-wall-clock reconstruction, no-alarm policy, VMGenID startup and fresh post-restore replacement/notification, complete VMClock startup/capture/codec/restore/notification, public per-vCPU PVTime measurement/accounting/publication/discovery, exact native-v2 2.3 introduction through Full 2.12 and Diff 2.13 carriage, repeated immutable clone restore, recapture, failure classification, redaction, signed multi-vCPU guest observation, and the four-cell fixed-production before/after assertion are terminal under the Wave 6 contract. |

## VMClock state and version contract

- `VmClockAbi` owns every field and exact offset in Firecracker's pinned
  112-byte `vmclock_abi`: magic, 4-KiB size, version, counter/time identifiers,
  sequence, disruption marker, flags, status/leap metadata, counter/time values,
  and VM generation counter. Decode rejects unsupported arm64 counter IDs,
  unknown or missing required flags, invalid enumerations, nonzero padding, and
  an odd sequence. Diagnostics expose only non-sensitive structural and
  generation metadata.
- New native-v1 capture writes nested `BANGDEV\0` version 1.1.0 and appends the
  exact validated ABI after the existing VMClock placement/SPI metadata. The
  outer native-v1 format and its memory binding are unchanged. Decode accepts
  both exact 1.1.0 and legacy 1.0.0; other versions and trailing bytes reject.
- A 1.1.0 load verifies that the encoded ABI equals the corresponding bytes in
  the independently integrity-checked memory image. A legacy 1.0.0 load derives
  the typed ABI from that memory page, so old local artifacts keep their prior
  meaning without inventing state. Every new capture reads the live page while
  vCPU execution and auxiliary publishers are quiesced.
- Native-v2 `2.3.0` stores the same exact ABI inside `BANGTM2\0`, together
  with portable placement, notification, policy, and per-vCPU PVTime state.
  Exact `2.4.0` through Full `2.12.0` and Diff `2.13.0` retain that component
  unchanged before their versioned device graph. Structural readers still
  admit valid `2.2.x` containers, but the complete typed HVF platform decoder
  requires kind 6 and therefore at least minor 3.

## Restore ordering and terminality

The destination first constructs and validates all native-v1 resources, loads
memory, maps it, creates the never-run runner, restores aggregate CPU/GIC/device
state, and preflights both time/identity interrupts and mapped memory. It then
performs this guest-visible sequence:

1. Generate a fresh nonzero VMGenID distinct from the captured value, write the
   complete 16-byte buffer, commit retained metadata, and assert its SPI.
2. Write an odd VMClock sequence, publish it with a release fence, increment
   disruption and generation counters with wrapping arithmetic, publish them
   with a release fence, write the next even sequence, and assert its SPI.
3. Assemble and commit the process session as `Paused`; only a later explicit or
   requested ordinary resume may run the vCPU.

Randomness, runner, signaler, line, or mapped-memory preflight failures precede
all writes and may be retried after complete cleanup. VMGenID write completion,
either device notification attempt, or any successful prefix of the VMClock
update makes the destination committed. Such a failure is terminal even when
resource cleanup succeeds, because retrying could expose two identities or an
odd/partially advanced clock page. No partial destination is returned or run.

The native-v2 path performs the same two identity steps only after complete
graph/memory preflight, architecture reconstruction, fresh PL031 registration,
and topology-ordered PVTime configuration. It then imports fresh paused
lifecycle tokens and publishes the focused owner. The first committed VMGenID
write defines the same terminal boundary; lifecycle or publication failure
after that point cannot make the destination retryable.

## PL031 destination policy

PL031 has no serialized mutable register payload in this profile. Install
constructs a new device whose data register is based on destination
`SystemTime`, so elapsed snapshot downtime is reflected naturally. Alarm match,
control, interrupt-mask, raw interrupt status, and masked interrupt status start
at zero. This is the complete supported Firecracker aarch64 no-interrupt subset;
it is not a claim of alarm delivery or source-wall-clock freezing.

## Certification boundary

This ledger does not claim KVM's ARM steal-time device attribute or arbitrary
cross-host time-source portability. Public native-v2 create/load/describe and
version selection are active through current Diff 2.13, and every optional
product retains the same required time component. The checked evidence proves
deterministic destination policy and repeated same-host cross-process clones;
it contains zero tested distinct-physical-host success pairs. Explicit future
CPU/host/fleet pair selection remains owned by
[#1491](https://github.com/seven332/bangbang/issues/1491), without reopening
this terminal time/identity producer.
