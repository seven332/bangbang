# Formal Verification Contract

## Source and interpretation

The immutable Firecracker v1.16.0 source baseline is
`d83d72b710361a10294480131377b1b00b163af8`. Its complete
`docs/formal-verification.md` corpus treats formal methods as complementary,
assumption-bounded evidence for selected high-risk logic. It does not establish
unrestricted or whole-system correctness.

Bangbang applies that outcome rather than copying Firecracker harnesses. The
checked authority is
[`formal-verification-audit.json`](formal-verification-audit.json), and the
operator/maintainer guide is
[`docs/formal-verification.md`](../../../docs/formal-verification.md). The lane
pins Kani 0.67.0 (`kani-0.67.0`, release commit
`4feaaad1d6a2378a6ff6caa3b4fc5d6999c7bb5d`) and its
`nightly-2025-11-21` compiler boundary.

## Exact bounded proof ledger

| Harness identity | Bangbang production owner | Bounded outcome |
| --- | --- | --- |
| `pager-limits-admission` | pager handshake limit construction | Full scalar input domains prove all successfully admitted page/count/operation/frame limits. |
| `virtqueue-ranges` | virtqueue range/index/EVENT_IDX helpers | A valid power-of-two queue-size assumption proves exact scalar geometry, index and notification boundaries without guest-memory traversal. |
| `token-bucket-refill-accounting` | private refill calculation used by the runtime bucket | Explicit small capacity/refill bounds plus the valid-budget invariant prove partial/full/unchanged accounting; timers and persistence remain tested, not modeled. |
| `pager-artifact-ranges` | peer-owned snapshot source range predicates | Admitted supported-page regions prove request containment and overlap symmetry without source/transport/concurrency claims. |
| `virtio-mmio-status-transitions` | pure virtio status transition gate | Full `u32` pairs prove exactly the four non-reset ordered advances; reset and `FAILED` are separate tested branches. |

Source validation scans all tracked workspace production Rust sources and
requires every `#[kani::proof]` to be under exact `cfg(kani)` and bijective
with this authority. The Linux runner calls Kani's compiled list separately per
package, rejects any package/source/symbol mismatch, and executes each manifest
command with `--exact`. Ordinary tests remain required.

## Terminal corpus mapping

| Capability identity | Owner | Disposition | Evidence |
| --- | --- | --- | --- |
| `corpus:formal-verification` | #1797 | `implemented-and-verified` | Five manifest-bijective harnesses, pinned source/compiled-list validation, exact Linux execution, retained unit/integration evidence, and this bounded contract. |

The disposition covers only the applicable bounded outcome above. FFI/HVF,
guest-memory and descriptor traversal, wall-clock/timers, threads/atomics and
concurrency/liveness, filesystem/transport behavior, performance/resource
bounds, guest/process integration, and whole-system correctness are explicit
nonclaims. They cannot become proved merely because a pure helper called by
those systems has a successful harness.
