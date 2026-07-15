# Firecracker v1.16.0 capability inventory

This directory is the structural scope authority for bangbang's Firecracker
v1.16.0 compatibility work. The baseline is commit
`d83d72b710361a10294480131377b1b00b163af8`.

The inventory complements, rather than replaces, the detailed behavior in
[`docs/firecracker-compatibility.md`](../../../docs/firecracker-compatibility.md)
and the test-layer summary in
[`docs/firecracker-validation-matrix.md`](../../../docs/firecracker-validation-matrix.md).
Those documents explain behavior; this directory makes omissions and terminal
claims mechanically visible.

## File ownership

- [`source-manifest.json`](source-manifest.json) is machine-owned. It records
  the pinned upstream inputs and exact identities for 26 Swagger paths, 38
  operations, 44 definitions, 152 properties, 23 configured Firecracker
  arguments, three non-Swagger DELETE routes, 14 public-tool operations, 41
  public-tool arguments, and 40 explicit non-Swagger source-corpus items.
- [`capabilities.json`](capabilities.json) is human-owned. Every generated
  identity has exactly one overlay, and additional `semantic.*` records cover
  cross-leaf guest, lifecycle, snapshot, observability, isolation, and
  specification behavior.
- [`process-contract.md`](process-contract.md) is the human-owned semantic
  audit for the 23 configured Firecracker arguments and the composite process
  records. It traces arity, defaults, relationships, observable behavior,
  cross-family ownership, implementation, and executable validation without
  expanding the machine-owned identity extractor.
- [`isolation-contract.md`](isolation-contract.md) records the production
  macOS bundle/worker boundary, its executable evidence, and the remaining
  #1351 isolation/resource/seccomp outcomes without treating them as direct
  Linux jailer parity.

Regeneration may produce a candidate `source-manifest.json`; it must never
create or rewrite a capability disposition, owner, evidence reference,
delivery issue, or Challenge result. A changed generated identity instead
causes a missing or stale overlay validation failure for a reviewer to resolve.

Stable source IDs use `<kind>:<upstream-key>`. Semantic IDs use the lowercase
`semantic.<namespace>:<slug>` form. IDs are scoped to this immutable v1.16.0
baseline. A later Firecracker baseline gets a separate directory and an
explicitly reviewed delta.

## Dispositions

Each capability has exactly one disposition:

- `audit-required` means the exact contract still needs review under the
  strict parent rule. It is allowed while delivery is in progress and is never
  a completion state.
- `missing-platform-feasible` requires a concrete delivery issue. It is never
  a completion state.
- `implemented-and-verified` requires implementation and validation
  references appropriate to the claim. Parser recognition or a stable
  unsupported response is not implementation.
- `proven-platform-impossible` requires the upstream contract, authoritative
  platform evidence, alternatives with rejection reasons, stable behavior,
  focused tests, compatibility and security documentation, and a current
  Challenge result linked as its GitHub issue comment.

The initial inventory is deliberately conservative. Existing prose or issue
closure does not automatically promote a record from `audit-required`.

The #1352 process audit promotes exactly 20 of the 29 process-family records:
18 complete argument leaves plus the complete CLI/readiness and
signal/exit/fd/cleanup semantics. Five incomplete argument leaves, the
snapshot-containing identity/output semantic, the aggregate run operation, and
both broad source corpora remain `audit-required`. The checked
[`process-contract.md`](process-contract.md) records those nine handoffs; a
partially implemented composite is not a terminal claim.

The #1354 production-boundary audit moves exactly three composite isolation
records to `missing-platform-feasible` with #1351 as their delivery owner. It
does not terminally promote them: external resource authority, authenticated
brokerage, vmnet policy, crash coupling, deployment identity, and exact
jailer/seccomp outcome classification remain incomplete. The broad source
corpus records remain `audit-required`. The checked
[`isolation-contract.md`](isolation-contract.md) separates the delivered
package/sandbox/supervisor subset from those handoffs.

The #1365 socket-directory slice adopts the API and vsock directory roles with
an exact safe-child grammar, same-filesystem anchored exclusive publication,
strict ownership records, supplied listeners, and one fixed session-bound
launcher facet for guest-initiated vsock port connections. It adds no worker
entitlement or steady-state helper and does not terminally promote the three
composites: snapshot authority, general dynamic brokerage and hard revocation,
vmnet policy, Linux outcome classification, and deployment identity still
remain under #1351.

## Commands

Validate checked-in delivery state without an upstream checkout:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
```

The final parent gate rejects `audit-required` and
`missing-platform-feasible`:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --final
```

Compare the generated manifest with an explicit clean Firecracker checkout at
the exact pinned commit:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- compare \
  --firecracker /path/to/firecracker
```

Generate a candidate without overwriting either checked-in inventory file:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- regenerate \
  --firecracker /path/to/firecracker \
  --output codex-work/tmp/firecracker-v1.16-source-manifest.candidate.json
```

The comparison command requires a clean Git worktree whose `HEAD` is the exact
pinned commit. It reads only declared regular files below that canonical root.
The local checkout path is not stored in tracked data. Ordinary CI does not
need a sibling checkout.

## Contributor update rule

Every pull request that changes a Firecracker-facing capability must update
all owned overlay records in the same change. Add implementation and validation
evidence only for the exact observable contract proved by that PR. Keep
unreviewed behavior `audit-required`; use `missing-platform-feasible` only with
a delivery issue; and use `proven-platform-impossible` only after the complete
strict evidence and Challenge gate. Keep capability IDs, source references,
evidence references, and exclusion alternatives in canonical sorted order and
free of duplicates. Local evidence must resolve to a tracked regular file
inside the repository; ignored, untracked, symlinked, and escaping paths fail
validation.

Run the focused validator and the repository's normal checks before submission.
The checked-in integration test also validates this inventory through the
ordinary workspace test command. A corpus reference records audit ownership;
it does not by itself prove that every semantic statement is implemented.

The inventory is not evidence by itself. Every terminal compatibility claim
depends on its referenced production behavior and validation.
