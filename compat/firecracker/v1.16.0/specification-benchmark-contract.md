# Specification benchmark certification

This versioned ledger owns the terminal Firecracker v1.16.0 interpretation and
evidence for issue #1798. Operational use belongs only in
[`docs/specification-benchmarks.md`](../../../docs/specification-benchmarks.md).

## Pinned upstream references

| Source | Pinned identity | Reference environment and status |
| --- | --- | --- |
| `SPECIFICATION.md` | commit `d83d72b710361a10294480131377b1b00b163af8`, blob `67ede9964f8a2d314b9cad69fe8d5b773e01b1d8` | M5d.metal/M6g.metal, Linux/KVM, sufficient resources; startup, memory and boot figures are reference-conditioned; compute, network and storage outcomes include pending integration coverage |
| `docs/network-performance.md` | commit `d83d72b710361a10294480131377b1b00b163af8`, blob `0be0d8cdd8dec6041286f36d3b33b7d7f8f4f437` | M5d.metal, Amazon Linux 2, VPC, kernel 4.14, ping, background traffic and multiple iperf3 clients |

Pinned numeric text includes startup CPU at most 8 ms, startup wall 6–60 ms
with about 12 ms typical, one-vCPU/128-MiB VMM overhead at most 5 MiB after
excluding guest mappings, boot at most 125 ms, a greater-than-95-percent
bare-metal compute objective, storage around 1 GiB/s at at most 70 percent CPU,
and network throughput/latency figures up to 14.5/25 Gbps and 0.06 ms under the
documented AWS/Linux setups. These are reference claims, not Bangbang
thresholds. The checked JSON authority preserves their pending statuses and
environment labels.

## Bangbang evidence boundary

`scripts/specification-benchmark.py` builds and signs one locked
`aarch64-apple-darwin` release binary with `--no-default-features`, then uses
two independent signed public-API/HVF sessions per iteration. It records exact
production startup metrics from explicit pre-spawn monotonic and zero-process-
CPU baselines, whole-process `ps` RSS at the guest release barrier,
the production boot timer, checked guest-clock compute/storage durations, and
the real metrics-FIFO `EAGAIN` failure/drain/retry replay counter. Raw integers,
integer count/min/median/max, and the complete comparison identity are retained.
There is no numeric verdict.

Network is structurally absent unless an explicit credential-free bounded
fixture is supplied. Its cleanup result is fixture-asserted, its argv and
environment are never recorded, and its output is not evidence for #1378.

The telemetry replay assertion observes `logger.missed_metrics_count` after the
single typed nonblocking flush failure and requires the value to be exactly one.

Portable tests own strict documents, summaries, comparison, fake transaction
cleanup, real portable FIFO behavior, fixture isolation, and absent-only
publication. Real Apple/HVF collection is untracked local evidence and has no
unsupported success path.

## Exact terminal capability set

| Capability | Owner | Disposition |
| --- | --- | --- |
| `corpus:network-performance` | #1798 | `implemented-and-verified` |
| `corpus:specification` | #1798 | `implemented-and-verified` |
| `semantic.specification:performance-resource-and-telemetry-outcomes` | #1798 | `implemented-and-verified` |

The first row certifies the pinned reference interpretation and strict optional
fixture boundary, not a positive network sample. The second certifies the
applicable specification audit. The semantic row certifies reproducible,
environment-labelled observation mechanics and telemetry-loss accounting, not
Firecracker parity or a hardware performance promise.

The #1798 phase endpoint was exactly 371 implemented-and-verified, 14
audit-required, 3 missing-platform-feasible, and 30
proven-platform-impossible. The later source-complete #1799 aggregate changes
only its exact five successor rows to reach 376/9/3/30. Wave 8 then changes
only its exact cross-capability row to reach its historical 377/8/3/30
endpoint. The later jailer uid/gid platform-limit transition changes exactly
those two identities to reach 377/6/3/32. The later configurable-chroot
platform-limit transition changes exactly that one identity to reach
377/5/3/33. The aggregate-jailer transition then changes exactly
`corpus:jailer` and `tool-operation:jailer/run` to reach 379/3/3/33. The
multiprocess-isolation transition then changes its one exact semantic row to
reach 380/3/2/33. The host-resource-authority transition changes its one exact
semantic row to reach the current 381/3/1/33 endpoint. The scoped gate accepts
only those eight exact phases and
derives every identity-checked difference; none of the totals is a delivery
quota.

## Nonclaims

- no AWS, Linux, KVM, Firecracker, bare-metal, CoreMark, fio, ping, or iperf
  parity;
- no portable hardware threshold or regression verdict;
- no claim that whole-process RSS is guest-map-excluding VMM overhead;
- no controlled page cache, privileged host isolation, or production resource
  policy;
- no production network availability, credentials, recovery, concurrency, or
  external cleanup proof; and
- no tracked environment report or replacement of current-head CI and review.
