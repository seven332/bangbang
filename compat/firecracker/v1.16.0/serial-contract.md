# Firecracker v1.16.0 serial closure contract

This ledger is the final checked closure record for the six directly owned
Firecracker v1.16.0 serial identities. #1479 originally closed the five
API/model identities and handed the semantic snapshot aggregate to Wave 6.
[#1652](https://github.com/seven332/bangbang/issues/1652) now closes that
handoff through the public bangbang-native `2.7.0` Full/File lifecycle.

All six identities are `implemented-and-verified`. The generated source
manifest remains 381 identities, the overlay retains 37 local semantic
identities and 418 total records, and this reconciliation moves the current
global disposition counts from 235/163/3/17 to 236/162/3/17.

## Evidence keys

- **API/model** — strict `PUT /serial` parsing in `crates/api/src/http.rs`,
  transactional preboot configuration and contained output-grant ownership in
  `crates/bangbang/src/vmm.rs`, and the runtime model in
  `crates/runtime/src/serial.rs`.
- **Exact 2.7 state** — `crates/runtime/src/snapshot_serial_v2_7.rs` defines
  the bounded endpoint intent, limiter configuration, complete UART registers,
  receive FIFO, line/interrupt status, and pending-work value.
  `crates/hvf/src/snapshot_v2.rs` binds it to the complete machine/platform
  graph, with optional unchanged profile-3 block/pmem storage.
- **Fresh destination ownership** —
  `crates/bangbang/src/{snapshot_restore_resources,snapshot_serial_restore}.rs`
  prepares one direct or contained storage-plus-serial authority transaction.
  `crates/hvf/src/{snapshot_v2_platform,startup}.rs` reconstructs the UART and
  re-establishes receive/input-ready/interrupt work before first execution.
- **Focused validation** — runtime codec and cross-field tests, exact resource
  role/access/alias/cancellation tests, endpoint/terminal/FIFO lifecycle tests,
  construction/controller/completion/cleanup fault injection, private signed
  HVF reconstruction, recapture, and frozen native-v1 plus exact
  2.3/2.4/2.5/2.6 dispatch fixtures.
- **Signed direct validation** —
  `signed_executable_certifies_native_v2_serial_continuation_over_fresh_stdio`
  and
  `signed_executable_reopens_configured_serial_snapshot_file_and_fifo_destinations`
  in `crates/bangbang/tests/executable_hvf_e2e.rs`.
- **Signed production validation** —
  `normal_bundle_certifies_native_v2_serial_snapshot_continuation_and_containment`
  in `crates/launcher/tests/production_bundle_e2e.rs`, together with the
  established live stdio isolation and snapshot grant/death-order matrices.

## Exact six-record ledger

| Identity | Final disposition | Exact contract and evidence |
| --- | --- | --- |
| `api-operation:PUT /serial` | implemented and verified | Strict preboot replacement accepts the optional output selector and Firecracker-shaped token bucket, rejects malformed or post-start requests without mutation, and selects configured output or default process stdio at startup. |
| `api-path:/serial` | implemented and verified | Complete strict PUT-only route, method, state, JSON, and error behavior. |
| `api-property:SerialDevice.rate_limiter` | implemented and verified | Missing or null is unconfigured. Valid size, optional one-time burst, and refill time wrap configured output or default stdout; exhaustion drops TX bytes without blocking the guest and records exact destination-local metrics. Exact 2.7 retains the configuration while a fresh destination starts a fresh budget and metrics set. |
| `api-property:SerialDevice.serial_out_path` | implemented and verified | Missing or null selects fresh destination process stdout plus supported stdin RX. A configured direct file/FIFO or contained write-only regular-file grant selects only that fresh output and disables stdin. Preparation, replacement, one-time transfer, redaction, and cleanup are transactional. |
| `api-schema:SerialDevice` | implemented and verified | Complete strict optional-`serial_out_path` and optional-`rate_limiter` schema with unknown-field/type rejection, preboot-only mutation, startup execution, exact 2.7 serialization, and destination reconstruction. |
| `semantic.device:serial-stdin-stdout-rx-and-restore` | implemented and verified | Live default/configured output, terminal/FIFO/pipe input, bounded RX, TX limiting, metrics, Running/Paused behavior, capture, exact 2.7 artifact integration, fresh endpoint authority, complete UART restoration, serial-only and MMIO/PCI-storage continuation, repeated clone loads, recapture, redaction, and cleanup are implemented and signed. |

## Exact 2.7 snapshot value

Exact 2.7 introduced one required serial component and admitted either a
serial-only product or the same serial component plus the optional profile-3
regular-file block/pmem graph introduced by exact 2.6. Current 2.10 retains
that component unchanged while independently permitting entropy, balloon, and
virtio-mem. The serial component contains:

- destination endpoint intent: default process stdio or one bounded configured
  selector;
- the optional public rate-limiter configuration;
- divisor latches, interrupt enable/identification, line control/status, modem
  control/status, and scratch registers;
- at most the 64-byte UART receive FIFO in exact order; and
- receive-interrupt and input-ready continuation intent.

Cross-field validation rejects impossible FIFO/status/interrupt combinations,
noncanonical flags, malformed endpoint selectors, incompatible outer versions,
trailing bytes, and over-limit allocations before a value is exposed.

The artifact never contains a source descriptor, terminal attributes, host
pipe/FIFO buffer, TX buffer, metric counters, limiter clock/budget, lock,
wakeup handle, or absolute host deadline. State and File/COW memory stay
immutable across repeated loads. Writable external block/pmem prefixes retain
their separately documented shared-storage policy.

## Destination endpoint and continuation policy

- Default restoration duplicates the destination process stdout and attaches
  only that destination's supported stdin. Terminal stdin is made raw for the
  lifetime of the shared destination owner; terminal attributes and descriptor
  flags are restored after the last owner drops. FIFO/pipe input stays
  nonblocking and capacity bounded.
- Configured restoration resolves the artifact selector under destination
  authority. Direct mode opens the destination path without following an
  inherited source handle. Contained mode claims exactly one
  `SerialSink`/`WriteOnly` grant in the same complete transaction as any
  storage backings and never falls back to an ambient pathname.
- The restored UART is built before guest execution. Full FIFO state,
  data-ready status, interrupt identification, receive-interrupt intent, and
  input-ready rearm work survive. GIC delivery remains retryable until
  successful. Only Running windows consume destination input; Paused load and
  recapture do not.
- EOF detaches destination input without disabling TX. Output failure remains
  a destination-local metric/error outcome and cannot terminate the process
  through SIGPIPE. Teardown restores stdio state and releases endpoint,
  monitor, storage, controller, and authority ownership exactly once.

## Signed delivery proof

The shared bare-arm64 test image programs deliberately nondefault valid UART
registers, fills the complete 64-byte RX FIFO, and waits for VMGenID
replacement without draining it. The source pipe also receives a distinct
40-byte source-only suffix. After the source launcher/process is terminated, a
fresh destination supplies a different 40-byte suffix. The restored guest
checks every programmed register, status/interrupt state, the exact retained
prefix, and the fresh suffix before emitting success and powering off.
Destination metrics count only the fresh 40 bytes, proving that the source
pipe tail was not serialized or inherited.

The signed direct matrix covers serial-only, profile-3 MMIO storage, and
profile-3 PCI storage; paused recapture; explicit and automatic resume;
immutable pair reuse; independent metrics; EOF; and shutdown. Separate direct
regular-file and named-FIFO cases rename the source output and create a fresh
destination endpoint at the persisted selector, proving destination reopening
and source-endpoint exclusion.

The normal production/App Sandbox matrix repeats the serial-only stdio
continuation across launcher and nested worker pipes, then repeats configured
output with storage over MMIO and PCI. Source and destination manifests use
the same logical selector but distinct already-opened write-only files.
Pathname replacement, exact grant redaction, launcher termination, worker
cleanup, socket removal, and session-namespace restoration are asserted.
Established snapshot matrices retain concurrent isolation and both
worker-first and launcher-first death-order coverage.

## Compatibility and limits

Frozen native-v1 and exact native-v2 2.3, 2.4, 2.5, and 2.6 remain immutable
readers. Exact 2.7 is bangbang-native and does not claim Firecracker snapshot
bytes. Direct named FIFO output and destination pipe/FIFO input are signed;
destination terminal raw-mode/restoration is focused-tested because the
current signed harness supplies pipes rather than a PTY. This is a precise
supported-host continuation contract, not a claim that arbitrary terminal
devices, filesystem paths, or host timing sources are portable across hosts.
Native-v2 Uffd, Diff/merge, artifact authentication/encryption, and other
optional-device profiles remain outside this serial closure.
