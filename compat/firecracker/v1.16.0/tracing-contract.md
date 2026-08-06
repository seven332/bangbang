# Firecracker v1.16.0 developer-tracing contract

This is the terminal compatibility contract for issue
[#1791](https://github.com/seven332/bangbang/issues/1791). It is pinned to
Firecracker v1.16.0 commit
`d83d72b710361a10294480131377b1b00b163af8` and Bangbang's supported macOS
arm64/Hypervisor.framework target.

Firecracker's tracing corpus provides optional function entry/exit diagnostics,
per-thread nested paths, RAII exit on ordinary return or unwind, and runtime
level/module filtering. Bangbang implements those observable developer
properties through an explicit consumer-side feature and fixed scope macro. It
does not claim Firecracker's source-rewrite tooling, generated instrumentation,
or implementation-mechanism identity.

## Admission and record shape

The `tracing` Cargo feature is opt-in and absent from every default feature
set. The whole logger expression and guard declaration are behind the
consuming crate's `cfg(feature = "tracing")`; default and default-release
executables therefore do not evaluate tracing expressions or contain the fixed
`trace module=` record marker. Standalone snapshot tools additionally require
`BANGBANG_TRACE=*` or a nonempty module prefix. An absent, empty, non-Unicode,
or nonmatching value preserves the ordinary tool result and diagnostics.

One admitted scope emits at most two newline-terminated records using the
existing 512-byte logger encoder:

```text
[level=Trace ] [origin=<normalized-source>:<line> ] trace module=<literal> thread=<opaque-Rust-ThreadId> scope=<literal[::literal...]> phase=<enter|exit>
```

The checked dynamic vocabulary is exactly `module`, opaque `thread`, nested
`scope`, and `phase`. Module and scope arguments must be source literals. Host
paths, environment values, payloads, guest values, identities, credentials,
addresses, selectors, descriptors, registers, timestamps, and formatted errors
are forbidden. Optional origin uses the existing normalized source-origin
encoder and debug output redacts the configured module filter.

## Nesting, filtering, and delivery

Each thread owns a fixed 32-entry stack. Level and module filters run before
stack mutation. Entry snapshots the complete literal path; the non-`Send`
guard emits the corresponding exit during normal return or unwind. Drop uses
fallible thread-local access and cannot propagate a logger failure. Closing an
outer scope clears any forgotten inner guard, so later scopes start cleanly.
Different threads cannot form a shared nested path.

API and VMM scopes reuse bounded host delivery. Device scopes use the existing
nonblocking guest producer. Standalone tools reuse the logger worker with an
eight-batch queue and 100 ms receipt/replacement/emergency bounds; they never
write an unbounded raw trace directly to stderr. Filtering, queue pressure,
disconnection, worker failure, and receipt timeout can lose diagnostics and are
loss-accounted, but cannot replace an API, VMM, device, snapshot, or process
result. Delivery is best effort, not durable.

## Exact production scope set

[`tracing-audit.json`](tracing-audit.json) and the AST validator admit exactly
these eight literal production calls:

| Owner | Module | Scope | Delivery |
| --- | --- | --- | --- |
| API request | `bangbang::api_server` | `handle_request_bytes_with_limit` | bounded host |
| process VMM | `bangbang::vmm` | `handle_action` | bounded host |
| runtime controller | `bangbang_runtime::controller` | `handle_action` | bounded host |
| virtio-MMIO device | `bangbang_runtime::device::virtio_mmio` | `read_access` | nonblocking guest |
| virtio-MMIO device | `bangbang_runtime::device::virtio_mmio` | `write_access` | nonblocking guest |
| snapshot tool | `bangbang_snapshot_tools::command` | `execute_rebase` | bounded tool |
| snapshot tool | `bangbang_snapshot_tools::command` | `execute_snapshot_info` | bounded tool |
| snapshot tool | `bangbang_snapshot_tools::command` | `execute_snapshot_register_removal` | bounded tool |

Adding, deleting, dynamically naming, or reclassifying a production macro call
fails the checked tracing gate until the authority, rationale, evidence, and
privacy review change together.

## Performance evidence

`scripts/report-tracing-overhead.sh` builds separate default and tracing release
executables, rejects the trace marker in the default binary, requires it in the
feature binary, reports their byte sizes and delta, and runs a descriptive
release diagnostic for disabled, filtered, and enabled nonblocking scopes.
Timing is environment-dependent evidence only; this contract deliberately sets
no portable nanosecond threshold.

## Terminal certification

Run:

```console
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --tracing-final
```

The terminal gate composes the terminal logger foundation with the exact
tracing audit, exact eight-call AST scan, feature and limit checks, anchored
evidence, the single #1791 ownership row, and the terminal `corpus:tracing`
capability. Focused tests cover default expression removal, nested ordering,
filtering before stack work, unwind and forgotten-scope recovery, thread
isolation, depth and record bounds, delivery loss/result preservation, debug
redaction, API/VMM nesting, device value exclusion, and runtime-opted-in tool
diagnostics. It does not certify source rewriting, durable delivery, tracing
enabled by default, or platform-independent timing.
