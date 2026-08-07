# Targeted Formal Verification

Bangbang uses Kani as bounded, complementary evidence for selected pure Rust
boundaries. The checked proof set does not replace unit, integration, signed
HVF, or public-process tests, and it does not claim whole-system correctness.

The machine-readable authority is
[`formal-verification-audit.json`](../compat/firecracker/v1.16.0/formal-verification-audit.json).
It pins Kani, the compiler toolchain, every harness identity and command, each
assumption and bound, the production owner, the claimed invariant, evidence,
and the closed nonclaim set. Delivery validation independently extracts every
tracked workspace production `#[kani::proof]` under `cfg(kani)` and requires
an exact manifest bijection. The Linux runner also compares the manifest with
Kani's compiled per-package JSON list before it executes any proof.

## Pinned setup

Kani execution is supported on Linux. Install the exact toolchain and release:

```console
rustup toolchain install nightly-2025-11-21 --profile minimal
cargo +nightly-2025-11-21 install --locked --version 0.67.0 kani-verifier
cargo kani setup
```

The `kani-0.67.0` release is pinned at
`4feaaad1d6a2378a6ff6caa3b4fc5d6999c7bb5d`. Its verifier compiler is
`nightly-2025-11-21`; the published release setup supplies the matching Kani
bundle. The checked CI lane uses `ubuntu-24.04`, runs sequentially, and has a
45-minute job bound.

From the repository root, run the complete checked workflow:

```console
python3 scripts/run-kani.py
```

The runner first executes `validate --formal-verification-final`, verifies the
exact `cargo kani --version` output and locked Cargo graph, and invokes
`cargo kani list --format json` separately for `bangbang-pager` and
`bangbang-runtime`. Separate package lists are required because Kani's merged
JSON mapping does not preserve package ownership for each source key. Every
reported path must resolve uniquely inside its package, the compiled set must
equal the checked `(package, source, symbol)` set, and then all five canonical
`--harness ... --exact` commands run in manifest order. Commands are argument
arrays and are never evaluated by a shell. The token-bucket command pins the
Kani 0.67.0 bundle's Kissat solver; its bounded symbolic inputs use the smallest
integer widths that still cover every recorded value before widening into the
production `u64`/`u128` helper. This avoids unconstrained high-bit SAT state
without narrowing the checked interval.

Normal macOS builds do not install or invoke Kani. Proof modules are elided by
`cfg(kani)`, while workspace check-cfg policy makes ordinary Rust and Clippy
builds reject misspelled conditional names.

## Proof envelope

| Identity | Production owner | Assumptions and bounds | Claimed invariant |
| --- | --- | --- | --- |
| `pager-limits-admission` | `PagerLimits::new` | No `kani::assume`; full `u32`/`u16` input domains, claim applies to successful construction | Accepted limits have the supported power-of-two page/count/operation/frame contract without overflow. |
| `virtqueue-ranges` | virtqueue geometry, index, and EVENT_IDX helpers | Queue size is a valid nonzero power of two; other `u16` and base `u64` inputs are unrestricted | Ring sizes/ranges and descriptor index admission are exact; empty EVENT_IDX intervals do not notify and the matching next event does. |
| `token-bucket-refill-accounting` | private `token_bucket_refill` used by `replenish_at` | `1 <= size <= 4096`, `budget <= size`, `1 <= refill_time_nanos <= 1000000000`, `elapsed <= refill_time_nanos` | Classification is exact at zero/full boundaries; partial accounting cannot reduce/overfill budget or consume time beyond elapsed/refill bounds. |
| `pager-artifact-ranges` | pager snapshot source range predicates | Supported page shift `12..=21`; both symbolic regions pass the production constructor; offsets/lengths otherwise use full integer domains | Accepted page/removal ranges are aligned, nonempty where required, nonoverflowing and contained; overlap is symmetric. |
| `virtio-mmio-status-transitions` | `is_valid_status_transition` | No `kani::assume`; full `u32` current/requested pairs | Every accepted non-reset transition is exactly one ordered status advance, preserves prior bits, adds one required bit, and introduces no unknown bit. |

The JSON authority is canonical when exact strings or command arguments are
needed. This table is the human index, not a second configurable authority.

## What the result means

A green lane means Kani 0.67.0 compiled the current production owners, found
exactly the five checked harnesses, and proved the assertions for their stated
domains and assumptions. It also means ordinary unit evidence remains present;
it does not mean unmodeled components were proved indirectly.

The proof set deliberately makes no claim about:

- unrestricted or whole-system correctness;
- FFI/HVF behavior, Apple framework semantics, guest execution, or signed
  process behavior;
- guest-memory contents or descriptor-chain traversal;
- wall-clock, `Instant`, timer, retry, persistence, or rollback behavior;
- threads, atomics, scheduling, concurrency, liveness, or deadlock freedom;
- filesystems, sockets, transports, external artifacts, or peer behavior; or
- performance, memory use, resource ceilings, or production availability.

Those surfaces remain owned by the repository's ordinary, property, process,
signed, and integration tests. In particular, token-bucket tests still own
burst/retry/time-reset/persistence behavior; virtqueue tests own mapped guest
memory and descriptor traversal; pager tests own framing, transport, sessions,
timeouts and cancellation; virtio-MMIO tests own reset and `FAILED` handling.

## Updating a proof

Change the production function and its unit tests first. Keep the proof
adjacent and private. If the symbol, assumption, bound, invariant, or owner
changes, update the single matching manifest record and this human index.
Run ordinary delivery and scoped validation on any host, then run the complete
Linux command above. A missing, extra, unguarded, duplicate, ambiguous,
uncompiled, or failing proof is a hard failure; do not weaken the compiled-list
check or claim a larger domain than the solver actually verifies.

The pinned Firecracker context is recorded in the versioned
[`formal-verification-contract.md`](../compat/firecracker/v1.16.0/formal-verification-contract.md).
