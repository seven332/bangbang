# Firecracker v1.16.0 time and identity restore contract

This ledger is the checked closure record for #1477, the sixth delivery slice
of #1440 under #1348. It covers the delivered aarch64 PL031 RTC, VMGenID, and
VMClock portions of exactly one aggregate identity:
`semantic.device:rtc-vmclock-vmgenid-and-pvtime`. That identity remains
`audit-required` because it also owns ARM PVTime, its accounting and
clone/portability policy, and final aggregate certification under #1478, #1480,
and #1481. This slice therefore changes no inventory disposition or global
count.

## Evidence keys

- **Typed ABI and codec** — `crates/runtime/src/vmclock.rs` models and validates
  the complete 112-byte little-endian VMClock v1 ABI, and
  `crates/runtime/src/snapshot_device.rs` captures it into the bounded
  `BANGDEV\0` 1.1.0 profile while retaining 1.0.0 load compatibility.
- **Capture and preparation** — `crates/hvf/src/startup.rs` captures the live
  page only inside the paused supervisor and auxiliary-quiescence ownership
  boundary. `crates/runtime/src/startup.rs` requires a valid even sequence and,
  for 1.1.0, exact agreement between the encoded ABI and loaded guest memory.
- **Restore transaction** — `crates/runtime/src/vmclock.rs` publishes odd
  sequence, release fence, incremented disruption and generation counters,
  release fence, and even sequence. `crates/hvf/src/startup.rs` preflights both
  SPI lines and mapped memory, completes VMGenID replacement/notification, then
  VMClock update/notification after aggregate architecture, vCPU, GIC, ICC,
  timer, pending-interrupt, and device installation and before any vCPU resume.
- **Failure policy** — `crates/hvf/src/{startup,snapshot_restore}.rs` separates
  mutation-free failures from committed guest-memory or notification failures.
  Only a completely cleaned, precommit destination is retryable; every failure
  after VMGenID replacement or the first VMClock write is terminal and the
  destination never runs.
- **RTC policy** — `crates/runtime/src/snapshot_device.rs` reconstructs a fresh
  PL031 against destination wall clock and verifies its match, control, mask,
  raw-status, and masked-status registers are zero. The aarch64 FDT intentionally
  supplies no RTC interrupt, matching the pinned Firecracker shape.
- **Focused and signed validation** — runtime and HVF unit tests cover ABI
  bytes, validation, wrapping counters, partial writes, legacy decode, encoded
  memory mismatch, destination RTC reconstruction, ordering, and retryability.
  `crates/bangbang/tests/executable_hvf_e2e.rs` restores the same immutable pair
  into fresh signed HVF processes; guest code observes both VMGenID halves
  change, a stable even VMClock sequence with changed disruption/generation
  counters, and a destination RTC value no earlier than its captured value.

## Exact one-record ledger

| Identity | Current disposition | Exact contract and remaining handoff |
| --- | --- | --- |
| `semantic.device:rtc-vmclock-vmgenid-and-pvtime` | audit required | PL031 startup/metrics/destination-wall-clock reconstruction, no-alarm policy, VMGenID startup and fresh post-restore replacement/notification, complete VMClock startup/capture/codec/restore/notification, same-host repeated-load behavior, failure classification, redaction, and signed guest observation are implemented and verified. **#1478** owns the HVF ARM PVTime ABI foundation, **#1480** owns PVTime accounting and focused certification, and **#1481** owns final aggregate clone/portability reconciliation and terminal disposition. |

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

## PL031 destination policy

PL031 has no serialized mutable register payload in this profile. Install
constructs a new device whose data register is based on destination
`SystemTime`, so elapsed snapshot downtime is reflected naturally. Alarm match,
control, interrupt-mask, raw interrupt status, and masked interrupt status start
at zero. This is the complete supported Firecracker aarch64 no-interrupt subset;
it is not a claim of alarm delivery or source-wall-clock freezing.

## Explicit remaining handoff

This ledger does not claim KVM's ARM steal-time device attribute, a substitute
HVF ABI, PVTime accounting, or cross-host time-source portability. #1478 must
define and implement a safe HVF-specific shared-page ABI, #1480 must account and
certify its guest-visible time, and #1481 must reconcile repeated clone and
portability outcomes across the complete remaining-device family before the
aggregate inventory record can become terminal.
