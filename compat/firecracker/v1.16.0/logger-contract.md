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

The 31 classes contain 24 implemented classes, no planned classes, and 7 exact
not-applicable classes. Across source mappings this is 402 implemented and 66
not-applicable invocations. The four additional not-applicable mappings are
i8042 producers: the source module is shared upstream, but construction, PIO,
API/runtime ownership, and observable behavior are x86_64-only.

Delivery and final logger validation now accept the complete overlay. Any
future `planned` class remains an intermediate state and final validation
rejects it.

Issue #1807 closes the configuration-owned classes. API control records cover
server start/stop, discarded connection failure, deprecated requests,
successful pause/resume and snapshot completion, and parse rejection. Every
successfully parsed dispatch has one closed result record; parse rejection has
one control record and no result. Process startup has one fixed `running`
outcome after the no-API owner is ready or the API socket is bound and before
readiness is announced.

Issue #1808 closes the host lifecycle and live-device controls, API/VMM worker
status, snapshot create/load results, metrics-worker failures, host signals,
guest power convergence, cancellation, and orderly/abnormal shutdown. These
records use typed fixed vocabularies, discard payloads before encoding, and
preserve the functional result when logger delivery fails.

Issue #1814 closes the cross-cutting backend and generic device-transport
classes. One controller-local `GuestLogger` snapshot shares the existing
worker, metrics, immutable filter/prefix state, and independent backend and
transport limiter identities. Normal and native-v2 assembly install it before
HVF, MMIO, or product-PCI work, and every run-loop wrapper must explicitly
forward the capability. Typed owners emit fixed outcomes before an error is
formatted; filtered, limited, full, disconnected, or failed delivery never
changes the backend or transport result. Repeating successful virtual-timer
exits are coalesced to one debug record per logger state so they cannot consume
the budget required by a later terminal vCPU outcome. Expected device
rate-limiter rejection remains exact in metrics and is a debug record rather
than a warning.

Issue #1815 closes block, pmem, network/MMDS, and vsock outcomes. Device-owned
observers consume the final typed queue result shared by MMIO and product PCI,
then emit at most one record for each nonzero summary category rather than one
record per request, packet, descriptor, connection, or byte. Failure and
rejection categories are admitted before success categories. High-fanout vsock
summaries use one fixed-capacity delivery batch after MMIO or product-PCI
interrupt handling, so logging cannot extend the device-to-guest publication
window. Narrow HVF supplements cover packet-provider acquisition and MMIO
interrupt delivery; product-PCI endpoint owners cover MSI delivery. MMDS
detours and transactional token-key rotation are fixed outcomes, and neither
token nor key material can enter a record.

Issue #1816 closes balloon, virtio-mem, entropy, serial, and time/identity
outcomes. Balloon, virtio-mem, and entropy observers consume complete typed
queue summaries shared by MMIO and product PCI. Virtio-mem configuration
commit and interrupt delivery remain separate outcomes when a committed PCI
update is followed by endpoint failure. Serial input is coalesced once per
run-loop dispatch or restored-continuation reconciliation while output and
limiter failures stay at the owning wrapper. RTC, VMGenID, VMClock, PVTime,
platform publication, capture, and
ordered restore retain fixed component and aggregate commit classifications.
Each class has an independent limiter identity. Raw errors, paths, device
identities, descriptors, byte/page counts, time values, and guest data are
discarded before encoding.
The signed aggregate remaining-device workload reaches all five classes over
MMIO and product PCI. Production-bundle logger-grant cases additionally require
at least one fixed balloon, virtio-mem, entropy, serial, and time/identity
record, retain opened-object authority after pathname replacement, and reject
fixture, grant, guest-marker, and class-specific forbidden values.

## Closed class ledger

`Mappings` is the exact number of source identities assigned to the class.
The owner/reason column records the implemented fixed boundary or exact
not-applicable reason.

| Class | Disposition | Owner/reason | Mappings |
| --- | --- | --- | ---: |
| `logger.api-control.outcome` | `implemented` | fixed server/connection/request outcomes | 7 |
| `logger.api-worker.outcome` | `implemented` | fixed worker lifecycle | 5 |
| `logger.api.request` | `implemented` | parsed method/template receipt | 1 |
| `logger.api.result` | `implemented` | closed HTTP result plus retained actions | 5 |
| `logger.backend.outcome` | `implemented` | typed HVF backend, vCPU, interrupt, and timer outcomes | 26 |
| `logger.balloon.outcome` | `implemented` | fixed balloon queue, accounting, discard, and interrupt outcomes | 28 |
| `logger.block.outcome` | `implemented` | fixed block request, queue, async, vhost-user, and interrupt outcomes | 34 |
| `logger.boot.time` | `implemented` | bounded boot timing | 1 |
| `logger.entropy.outcome` | `implemented` | fixed entropy queue, provider, limiter, and interrupt outcomes | 16 |
| `logger.lifecycle.outcome` | `implemented` | fixed VM and live-device lifecycle | 24 |
| `logger.limiter.recovery` | `implemented` | bounded suppressed count | 1 |
| `logger.memory-hotplug.outcome` | `implemented` | fixed virtio-mem request, rollback, configuration, and interrupt outcomes | 22 |
| `logger.network.outcome` | `implemented` | fixed network, provider, MMDS, and interrupt outcomes | 26 |
| `logger.nonapp.example` | `not-applicable` | `example-only` | 22 |
| `logger.nonapp.fuzzing` | `not-applicable` | `developer-instrumentation` | 1 |
| `logger.nonapp.gdb` | `not-applicable` | `developer-instrumentation` | 19 |
| `logger.nonapp.linux-hardening` | `not-applicable` | `linux-kvm-only` | 2 |
| `logger.nonapp.tool` | `not-applicable` | `separate-tool-owner` | 1 |
| `logger.nonapp.tracing` | `not-applicable` | `tracing-owned` | 2 |
| `logger.nonapp.x86` | `not-applicable` | `x86-only` | 19 |
| `logger.observability.outcome` | `implemented` | rate-limited metrics-worker failure | 4 |
| `logger.pmem.outcome` | `implemented` | fixed pmem flush, queue, limiter, and interrupt outcomes | 18 |
| `logger.process-signal.outcome` | `implemented` | fixed signal and shutdown convergence | 5 |
| `logger.process-startup.outcome` | `implemented` | fixed normal startup outcome | 5 |
| `logger.process.exit` | `implemented` | fixed terminal category | 3 |
| `logger.process.panic` | `implemented` | fixed emergency record | 3 |
| `logger.serial.outcome` | `implemented` | fixed serial input, output, limiter, and interrupt outcomes | 12 |
| `logger.snapshot.outcome` | `implemented` | fixed create/load result | 18 |
| `logger.time-identity.outcome` | `implemented` | fixed RTC, VMGenID, VMClock, PVTime, and publication outcomes | 12 |
| `logger.transport.outcome` | `implemented` | typed generic MMIO, virtio, PCI, queue, and interrupt outcomes | 74 |
| `logger.vsock.outcome` | `implemented` | fixed vsock queue, connection, reset, and interrupt outcomes | 52 |

The outcome classes are semantic, not raw-line mirrors. Their fixed
`device-kind`, `operation`, and `outcome` values distinguish the public result;
the closed outcome selects level. Adding a new operation/outcome requires
deliberate class metadata and compiled-event review, never inheritance from a
source-path selector.

The implemented host-owned records have exact fixed shapes:

- API control: `operation=<server|connection|request|request-parse>
  outcome=<closed-outcome>`;
- parsed dispatch result: `action=request
  outcome=<ok|no-content|bad-request|payload-too-large>`;
- normal startup: `operation=process-startup outcome=running`;
- VM lifecycle: `operation=<backend-startup|vm-start|vm-pause|vm-resume|vm-stop>
  outcome=<closed-outcome>`;
- live device control: `device-kind=<block|network|pmem>
  operation=<device-attach|device-update|device-detach>
  outcome=<succeeded|rejected|failed>`;
- HVF backend: `operation=<closed-backend-operation>
  outcome=<closed-backend-outcome>`;
- generic transport: optional
  `device-kind=<balloon|block|entropy|memory-hotplug|network|pmem|serial|vsock>`
  plus `operation=<closed-transport-operation>
  outcome=<closed-transport-outcome>`;
- block, pmem, network/MMDS, and vsock data planes:
  `device-kind=<block|pmem|network|vsock>
  operation=<closed-device-operation> outcome=<closed-device-outcome>`;
- remaining device data planes:
  `device-kind=<balloon|entropy|memory-hotplug|serial|time-identity>
  operation=<closed-device-or-time-operation>
  outcome=<closed-device-or-commit-outcome>`;
- snapshots: `operation=<snapshot-create|snapshot-load>
  outcome=<succeeded|rejected|failed|cancelled>`;
- worker and observability status:
  `operation=<boot-worker|metrics-worker> outcome=<closed-outcome>`; and
- process convergence:
  `operation=<host-signal|cancellation|guest-power|shutdown>
  outcome=<closed-outcome>`.

Levels are selected from the closed outcome: normal operation is `Info`,
unchanged VM state and an orderly stopped worker are `Debug`, snapshot
cancellation and deprecation are `Warn`, and rejection, failure, or abnormal shutdown are
`Error`. These records use fixed modules and do not carry a URI,
request/response body, fault text, status integer, path, selector, device ID,
MAC address, or guest value.

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
Backend, transport, balloon, block, entropy, memory-hotplug, network, pmem,
serial, time-identity, and vsock use their corresponding
`logger-rate.<class>.outcome` identities; the repeating asynchronous
metrics-worker class independently uses `logger-rate.observability-worker`.
The sole exception is `logger.limiter.recovery`, which must be unrestricted to
report a prior admitted recovery without recursively limiting itself. Queue,
receipt, write, flush, and replacement loss never changes the request, VM,
guest, worker, or process result and increments the exact existing loss counter
once. That exact boundary ends at the writer configured in the logger worker.
For default stdout, the configured writer is the internal nonblocking pipe: a
successful receipt confirms whole-record pipe admission, not completion of the
later stdout-forwarder write. Downstream progress is deliberately non-durable
and does not retroactively assign already admitted bytes to per-record loss.

## Default output and configuration projection

Pinned Firecracker writes to process stdout when no logger path is configured.
Normal Bangbang execution now installs a process-owned stdout adapter before
the first logger-capable startup step. The adapter feeds the existing bounded
worker and normal status output into one close-on-exec, nonblocking internal
pipe. One process-owned forwarder drains that pipe to a writable close-on-exec
stdout duplicate without changing stdout's shared status flags. A blocked real
stdout can therefore stall only that one forwarder; it cannot block producers,
alter serial standard-stream ownership, or accumulate forwarding threads.
The forwarder waits and retries when the shared target is temporarily
nonblocking and full, advances progress only after a complete chunk, and exits
on a terminal target error. Later pipe writes then observe the closed reader;
already admitted unread bytes remain outside per-record accounting. Normal
convergence waits at most one second for already accepted adapter bytes. It
never joins the sole forwarder, which may remain blocked until process exit,
and a stalled or failed output cannot replace the process result.
Runtime/library controllers remain silent unless an executable explicitly
installs an output. An unavailable or unwritable stdout leaves the logger
unconfigured and cannot change process readiness or results. Version/help/
snapshot-inspection short commands remain exact and exit before normal VMM
logger setup.

A later explicit direct path or contained write-only sink replaces stdout
as the logger target through the existing failure-atomic worker transaction.
Because the adapter never mutates real stdout flags, default serial output can
independently capture and restore its standard-stream lifetime before or after
that commit. Logger, status, and default serial bytes may share process stdout,
but no cross-producer byte or record ordering is promised; consumers that need
an isolated serial or logger protocol must configure a separate target. Failed
or path-free updates retain the logger target; the process keeps the same
bounded adapter for best-effort status output. Producers never perform sink
I/O.

`FullVmConfiguration.logger` remains omitted. The pinned Firecracker ordinary
configuration conversion also returns `logger: None`, and exporting Bangbang's
process-local path, module, grant, delivery, or limiter state would leak host
authority and create a misleading round trip. Startup configuration accepts the
strict logger section before actions, and its provided values override the
matching earlier CLI logger fields and target in one ordered startup
transaction; omitted values retain their prior/default meaning.

## Update and verification workflow

Ordinary validation uses only checked files:

```text
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
```

Terminal logger certification also uses only checked files:

```text
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --logger-final
```

That scoped command validates the complete capability inventory in delivery
mode, the complete logger producer overlay in final mode, and all eleven
#1786 operation/path/schema/property/corpus/semantic rows as terminal. Global
`validate --final` remains unchanged and is reserved for a repository snapshot
with no unresolved capability family.

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

Issue #1807 promotes the nine concrete `/logger` operation, path, schema,
property, and full-configuration records in `capabilities.json`. Issue #1816
closes the last producer classes, leaving 24 implemented and seven exact
not-applicable classes, 402 implemented and 66 not-applicable mappings, and no
planned work. Issue #1810 certifies that unchanged source closure together
with focused runtime/API/panic tests, ordinary process tests, signed direct
MMIO/PCI tests, and ordinary-production/App Sandbox grant tests. It therefore
promotes `corpus:logger` and
`semantic.observability:logger-delivery-filtering-loss-and-redaction`; all
eleven #1786 rows are `implemented-and-verified`. The terminal claim remains
best effort and process local: it does not promise durable sink delivery,
fold the separately terminal #1791 developer-tracing feature into ordinary
logger producer classes, or complete unrelated tooling rows.
