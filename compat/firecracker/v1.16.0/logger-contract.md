# Firecracker v1.16.0 logger producer contract

This contract is the human policy and source-closure ledger for issue
[#1786](https://github.com/seven332/bangbang/issues/1786). It is pinned to
Firecracker v1.16.0 commit
`d83d72b710361a10294480131377b1b00b163af8` and Bangbang's macOS arm64/HVF
target.

The contract does not copy Firecracker log messages. It maps every upstream
logger invocation to one closed Bangbang semantic class so later producer work
can retain observable outcomes without retaining dynamic source arguments.

## Authority and ownership

Three checked files have distinct owners:

- [`logger-producer-manifest.json`](logger-producer-manifest.json) is
  machine-owned. It records source identities, syntax/source context, Git blob
  inputs, and normalized SHA-256 fingerprints.
- [`logger-producer-audit.json`](logger-producer-audit.json) is human-owned. It
  defines semantic classes and contains one explicit mapping for every source
  identity.
- this Markdown file is human-owned. It explains policy and repeats only
  mechanically checked class-level closure facts.

Regeneration creates only an explicit machine-manifest candidate. It never
generates, carries forward, or rewrites semantic classes, dispositions,
rationales, owners, or evidence.

## Exact pinned source population

The extractor parses all 362 tracked `src/**/*.rs` files. The checked result is
468 invocations in 81 matching files:

| Source fact | Exact count |
| --- | ---: |
| Ordinary calls | 429 |
| Explicit unrestricted calls | 39 |
| `error!` | 180 |
| `warn!` | 138 |
| `info!` | 54 |
| `debug!` | 47 |
| `trace!` | 10 |
| `error_unrestricted!` | 22 |
| `warn_unrestricted!` | 7 |
| `info_unrestricted!` | 10 |
| Production context | 446 |
| Test-only context | 0 |
| Example context | 22 |
| Direct AST invocation | 466 |
| Nonlogger macro-template invocation | 2 |

The two macro-template producers are the block async-engine helper and signal
handler generator. They are real public logger calls in expansion templates
and are counted once at their source coordinates. Firecracker's logger wrapper
definitions contain hidden `__log_*` calls and are not public invocation rows.

An identity is
`logger-invocation:<source-path>:<one-based-line>:<one-based-column>`. Its
fingerprint is SHA-256 over normalized Rust macro tokens. Neither the normalized
tokens nor the raw message/arguments appear in the checked data or mismatch
diagnostics.

## Semantic class rules

Every source identity maps to exactly one class. A class may group multiple
source calls only when subsystem, observable outcome, target fields, delivery,
module/origin behavior, limiter policy, disposition, and delivery owner agree.
There are no path selectors and no `other`, `unknown`, or catch-all class.

The 31 classes contain 5 implemented classes, 19 planned classes, and 7 exact
not-applicable classes. Across source mappings this is 9 implemented, 397
planned, and 62 not-applicable invocations.

`planned` is an explicit intermediate delivery state. The three owners are:

- #1807: logger defaults/configuration, API receipt/result, and startup;
- #1808: host lifecycle, API/VMM worker, snapshot, signal, and observability
  outcomes; and
- #1809: backend, vCPU, transport, and device outcomes.

Delivery validation accepts those named planned classes. Final validation
rejects every planned class.

`logger.api.result` is planned because general closed result/rejection coverage
is not complete. Its metadata nevertheless records the already compiled
`InstanceStart` and `FlushMetrics` events and their exact #1785 evidence. This
does not promote the broader class; it prevents existing compiled behavior from
being lost while #1807 completes the shared source boundary.

## Closed class ledger

`Mappings` is the exact number of source identities assigned to the class.
Owners apply only to planned work.

| Class | Disposition | Owner/reason | Mappings |
| --- | --- | --- | ---: |
| `logger.api-control.outcome` | `planned` | #1807 | 7 |
| `logger.api-worker.outcome` | `planned` | #1808 | 5 |
| `logger.api.request` | `implemented` | parsed method/template receipt | 1 |
| `logger.api.result` | `planned` | #1807; existing minimal actions retained | 5 |
| `logger.backend.outcome` | `planned` | #1809 | 26 |
| `logger.balloon.outcome` | `planned` | #1809 | 28 |
| `logger.block.outcome` | `planned` | #1809 | 34 |
| `logger.boot.time` | `implemented` | bounded boot timing | 1 |
| `logger.entropy.outcome` | `planned` | #1809 | 16 |
| `logger.lifecycle.outcome` | `planned` | #1808 | 24 |
| `logger.limiter.recovery` | `implemented` | bounded suppressed count | 1 |
| `logger.memory-hotplug.outcome` | `planned` | #1809 | 22 |
| `logger.network.outcome` | `planned` | #1809 | 26 |
| `logger.nonapp.example` | `not-applicable` | `example-only` | 22 |
| `logger.nonapp.fuzzing` | `not-applicable` | `developer-instrumentation` | 1 |
| `logger.nonapp.gdb` | `not-applicable` | `developer-instrumentation` | 19 |
| `logger.nonapp.linux-hardening` | `not-applicable` | `linux-kvm-only` | 2 |
| `logger.nonapp.tool` | `not-applicable` | `separate-tool-owner` | 1 |
| `logger.nonapp.tracing` | `not-applicable` | `tracing-owned` | 2 |
| `logger.nonapp.x86` | `not-applicable` | `x86-only` | 15 |
| `logger.observability.outcome` | `planned` | #1808 | 4 |
| `logger.pmem.outcome` | `planned` | #1809 | 18 |
| `logger.process-signal.outcome` | `planned` | #1808 | 5 |
| `logger.process-startup.outcome` | `planned` | #1807 | 5 |
| `logger.process.exit` | `implemented` | fixed terminal category | 3 |
| `logger.process.panic` | `implemented` | fixed emergency record | 3 |
| `logger.serial.outcome` | `planned` | #1809 | 12 |
| `logger.snapshot.outcome` | `planned` | #1808 | 18 |
| `logger.time-identity.outcome` | `planned` | #1809 | 16 |
| `logger.transport.outcome` | `planned` | #1809 | 74 |
| `logger.vsock.outcome` | `planned` | #1809 | 52 |

The outcome classes are semantic, not raw-line mirrors. Their fixed
`device-kind`, `operation`, and `outcome` values distinguish the public result;
the closed outcome selects level. Adding a new operation/outcome requires
deliberate class metadata and compiled-event review, never inheritance from a
source-path selector.

## Allowed fields and redaction

The complete allowed field vocabulary is:

- parsed HTTP method and fixed route template;
- fixed action;
- fixed operation, outcome, device kind, or process category;
- bounded wall/CPU microseconds for the boot timer; and
- bounded rate-limit recovery count.

No class admits a request or response body, fault/panic/raw error text, dynamic
URI or selector, host path, grant reference, descriptor or queue index,
credential, guest byte, packet, address/offset, register/vCPU value, socket,
per-device identity, instance/session identifier, or other cross-session value.
Unsafe source arguments are discarded before a `LoggerEvent` exists; they are
never sanitized after formatting.

Module values are a closed set of fixed Bangbang module identities. Origin is
either the configured normalized source origin, the prepared panic origin, or
not applicable. Dynamic Firecracker module paths are not copied.

## Delivery and limiter policy

Applicable classes use one of four delivery policies:

- `unrestricted-host`: one fixed operator/process outcome submitted without a
  lexical limiter;
- `bounded-host`: ordinary host delivery with a bounded receipt whose result is
  discarded by the functional operation;
- `nonblocking-async`: worker/callback delivery with no wait or sink access;
- `nonblocking-guest`: guest/vCPU/device delivery with filter and limiter before
  one nonblocking queue attempt.

A guest-capable class applies the guest-safe policy to every mapped producer,
including a host-only member of the same semantic class. This is deliberately
conservative: it cannot grant a producer a blocking or unrestricted path.

Every guest-capable class is rate-limited under its own fixed class identity.
The sole exception is `logger.limiter.recovery`, which must be unrestricted to
report a prior admitted recovery without recursively limiting itself. Queue,
receipt, write, flush, and replacement loss never changes the request, VM,
guest, worker, or process result and increments the exact existing loss counter
once.

## Default output and configuration projection

Pinned Firecracker writes to process stdout when no logger path is configured.
The challenged #1786 Plan selects that platform-feasible behavior, mediated by
Bangbang's existing bounded worker. It remains planned under #1807 in this
audit-only change: the current executable is not silently changed by this
contract.

A later explicit direct path or contained write-only sink replaces stdout
through the existing failure-atomic worker transaction. A path-free update
retains the active target. Producers never write synchronously to stdout or any
other sink.

`FullVmConfiguration.logger` remains omitted. The pinned Firecracker ordinary
configuration conversion also returns `logger: None`, and exporting Bangbang's
process-local path, module, grant, delivery, or limiter state would leak host
authority and create a misleading round trip. #1807 owns the final API/schema
evidence for this optional-field decision.

## Update and verification workflow

Ordinary validation uses only checked files:

```text
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
```

Exact upstream comparison requires a clean checkout at the pinned commit:

```text
cargo run -p bangbang-firecracker-capability-audit --locked -- compare --firecracker ../firecracker
```

Create a machine-only candidate with:

```text
cargo run -p bangbang-firecracker-capability-audit --locked -- \
  regenerate-logger-producers --firecracker ../firecracker \
  --output codex-work/tmp/logger-producer-manifest.candidate.json
```

The command refuses existing destinations and lexical or canonical aliases of
all checked JSON files. Review identity, syntax, context, input, count, and
fingerprint changes before deliberately updating the checked manifest. Resolve
every missing/stale human mapping separately.

This audit slice does not change `capabilities.json`: all eleven #1786 records
remain `audit-required` until their owning behavior and aggregate evidence are
complete.
