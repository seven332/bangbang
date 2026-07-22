# Firecracker v1.16.0 serial closure contract

This ledger is the checked closure record for #1479, the fifth delivery slice
of #1440 under #1348. It covers exactly six directly owned Firecracker v1.16.0
serial identities. Five API operation, path, property, and schema identities
are `implemented-and-verified`. Exactly
`semantic.device:serial-stdin-stdout-rx-and-restore` remains `audit-required`
because its complete upstream claim includes optional-device artifact encoding,
endpoint reconstruction, and restore, which Wave 6 owns.

The generated source manifest remains 381 identities, the overlay retains 37
local semantic identities and 418 total records, and this reconciliation moves
the global disposition counts from 186/212/3/17 to 191/207/3/17.

## Evidence keys

- **API/model** — strict `PUT /serial` parsing in `crates/api/src/http.rs`,
  transactional preboot configuration and contained output-grant ownership in
  `crates/bangbang/src/vmm.rs`, and the runtime model in
  `crates/runtime/src/serial.rs`.
- **Runtime** — nonblocking shared-lifetime process stdio, configured file/FIFO
  output, TX limiting, the bounded 64-byte UART RX FIFO, typed interrupt and
  input-ready intents, metrics, and redacted capture-ready state in
  `crates/runtime/src/serial.rs`; selected-owner traversal and startup resource
  transfer in `crates/runtime/src/startup.rs`.
- **HVF** — one run-loop wakeup monitor for serial readiness and existing
  device wakeups, capacity-bounded reads, full-FIFO deregistration and drain
  rearming, EOF/error detachment, retained interrupt delivery, paused capture
  exclusion, and owner cleanup in `crates/hvf/src/startup.rs`.
- **Focused validation** — route/model/controller tests in
  `crates/api/src/http.rs` and `crates/bangbang/src/{api_server,vmm}.rs`, stdio
  descriptor/terminal restoration and UART state tests in
  `crates/runtime/src/serial.rs`, and run-loop readiness/backpressure/interrupt
  tests in `crates/hvf/src/startup.rs`.
- **Signed public validation** —
  `crates/bangbang/tests/executable_hvf_e2e.rs` proves default stdout, a
  greater-than-FIFO stdin transfer, configured-output stdin exclusion, TX
  limiting, pause/capture/resume, EOF survival, concurrent-process isolation,
  metrics, and clean shutdown.
- **Signed production validation** —
  `crates/launcher/tests/production_bundle_e2e.rs` proves the same default
  stdin/stdout ownership across the production launcher and App Sandbox worker
  boundary, including greater-than-FIFO flow, EOF, termination, and exact
  socket/session cleanup.

## Exact six-record ledger

| Identity | Final disposition | Exact contract and evidence |
| --- | --- | --- |
| `api-operation:PUT /serial` | implemented and verified | Strict preboot replacement accepts the optional path and Firecracker-shaped token bucket, rejects malformed or post-start requests without mutation, and selects configured output or default process stdio at startup. API/model and signed validation. |
| `api-path:/serial` | implemented and verified | Complete strict PUT-only route, method, state, JSON, and error behavior. API route and signed validation. |
| `api-property:SerialDevice.rate_limiter` | implemented and verified | Missing or null is unconfigured. Valid size, optional one-time burst, and refill time wrap either configured output or default stdout; exhaustion drops TX bytes without blocking or failing the guest write and records the exact drop count. Focused and signed validation. |
| `api-property:SerialDevice.serial_out_path` | implemented and verified | Missing or null selects default process stdout plus supported stdin RX. A configured direct file/FIFO or contained write-only regular-file grant selects only that output and disables stdin; preparation, replacement, one-time transfer, path redaction, and cleanup are transactional. Focused and signed validation. |
| `api-schema:SerialDevice` | implemented and verified | Complete strict optional-`serial_out_path` and optional-`rate_limiter` schema with unknown-field/type rejection, preboot-only mutation, and startup execution. API/model and signed validation. |
| `semantic.device:serial-stdin-stdout-rx-and-restore` | audit required | Default stdout, configured output, terminal/FIFO stdin, bounded RX, TX limiting, metrics, Running/Paused behavior, MMIO ownership, complete capture-ready UART/config state, descriptor restoration, and cleanup are implemented. **[Wave 6 #1490](https://github.com/seven332/bangbang/issues/1490)** owns optional-device encoding, artifact integration, fresh host-endpoint reconstruction, UART restore, migration/clone behavior, portability policy, and signed restored-guest outcomes. |

## Observable stdio, run-loop, and capture-ready contract

- With no `serial_out_path`, the production VMM duplicates stdout with
  close-on-exec ownership, makes the shared open-file description nonblocking,
  and applies the optional serial limiter. It attaches stdin only when stdin is
  a terminal or FIFO/pipe; closed, invalid, and ordinary nonpollable stdin are
  ignored without disabling TX. A configured direct or contained output keeps
  the existing output path/grant semantics and attaches no stdin.
- Terminal stdin is placed into raw mode for byte-exact guest input. FIFO/pipe
  stdin and stdout retain their access modes. Original terminal attributes and
  input/output status flags are restored only after the final shared stdio
  owner drops, so splitting output and input ownership cannot restore a live
  endpoint early. Diagnostics expose neither descriptors nor paths.
- Readiness joins the existing owner run-loop monitor; there is no serial side
  thread. Each dispatch reads at most the UART's current capacity and at most
  64 bytes. Filling the FIFO disarms stdin readiness. Guest drain publishes one
  coalesced input-ready intent, whose owner re-arms the descriptor. EOF detaches
  cleanly; any non-readiness host-input failure records `uart.error_count` and
  detaches.
  Would-block and interrupted reads preserve the endpoint without spinning.
- Accepted bytes update `uart.input_count`; rejected injection would update
  `overrun_count`, but the host dispatcher never reads beyond capacity. RX data
  ready, overrun, interrupt-identification, FIFO clear, receive-interrupt, and
  retry intent behavior remain owned by the backend-neutral UART. An interrupt
  intent is removed only after successful GIC delivery, so delivery failure is
  retryable rather than lossy.
- Only Running run-loop windows consume host input. Paused sessions retain the
  endpoint and queued kernel bytes without reading them; resume continues
  delivery. A paused capture transaction uses the supervisor/quiescence owner,
  excludes concurrent UART MMIO/run-loop mutation, and validates the exact
  selected serial owner before any snapshot publication.
- Capture-ready state pairs the reconstructible external `SerialConfig` with
  complete guest-visible UART registers, RX bytes, line/interrupt status, and
  pending receive/input-ready intents. Live stdout/stdin descriptors, terminal
  state, host pipe buffers, TX bytes, counters, locks, and wakeup handles never
  enter the value. EOF or input failure unregisters and stops reading stdin
  immediately; its shared restoration handle remains alive with TX until
  session teardown. Teardown drops both endpoint halves, restores process
  descriptor state, and retains no per-session worker ownership.
- Production daemon mode intentionally supplies `/dev/null` standard streams;
  default serial TX is therefore discarded and no stdin RX endpoint is
  attached. This follows the launcher's explicit daemon process policy rather
  than introducing an ambient serial endpoint.

## Signed delivery proof

The generated serial-RX initrd opens `/dev/ttyS0`, installs an explicit raw
termios configuration, announces readiness, and verifies one exact 104-byte
payload across short reads. The signed executable cases prove a 64-byte first
fill plus drain-driven remainder, queued input across pause/capture/resume,
configured-output exclusion, limiter drops, EOF with a still-live API, two
isolated concurrent VMM processes, exact UART metrics, and orderly teardown.
The signed production-bundle case repeats the greater-than-FIFO stdin/stdout
protocol through the fixed launcher-to-sandbox-worker standard streams and
then proves worker, socket, and session cleanup. These tests use marker-driven
coordination and no timing-based input-capacity assumptions.

## Explicit Wave 6 handoff

This closure intentionally creates no new serial byte encoding or compatibility
version. Bangbang-native v1 continues to encode only its legacy six UART
register bytes, requires representable live UART state, reconstructs default
output with empty metrics, and does not preserve a configured path, limiter
budget, RX FIFO, pending input/interrupt intents, host endpoint, terminal mode,
pipe buffer, or TX bytes.

Wave 6 must integrate the complete detached UART/config value into an
optional-device artifact, define versioning and validation, restore guest UART
state before execution, select fresh default or configured host endpoints under
destination authority, re-establish readiness and pending interrupt work,
define endpoint/terminal/FIFO portability and migration/clone policy, and prove
restored Linux RX/TX, limiting, pause/resume, EOF, isolation, and cleanup.
Inheriting or serializing source-process descriptors, terminal state, host pipe
buffers, or undisclosed guest bytes is not an acceptable reconstruction.
