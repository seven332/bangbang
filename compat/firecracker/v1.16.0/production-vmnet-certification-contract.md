# Production vmnet certification authority

This authority records the terminal #1948 no-Apple production-vmnet result for
the two #1378 capabilities:

- `corpus:network-setup`
- `semantic.network:virtio-net-vmnet-policy-and-connectivity`

It is a successor to the #1930 feasibility authority, the #1942 product
topology, the #1943 least-privileged handoff, and the #1944 certification
consumer. It does not rewrite those historical delivery boundaries.

## Exact merged-main evidence

The canonical checked result is
[`production-vmnet-certification-evidence.json`](production-vmnet-certification-evidence.json).
It was emitted twice from independently prepared immutable packages at merged
commit `b4fe638a7fbbedbb118516285b4d21209c344f4e`, tree
`2ccd3b8ae6f3742be9e102fc2428d2edeb0a7a04`, on macOS 26.6.2 arm64 with SDK
26.5 and HVF support. Both results validate and are byte-identical at SHA-256
`8f3dd5c3669f281e31536f617bc50648e8cf5d40cb38d1b661fec80b4fa87dca`.

Preparation used ordinary-user artifact construction and ad-hoc signing. The
caller authorized only the fixed already-root runner externally. No Apple
signing identity, provisioning profile, restricted vmnet entitlement,
credential discovery, persistent service, root VMM, or arbitrary privileged
path/command was used.

The public schema-v2 result contains only categorical authority,
entitlements, platform, source, ordered cases, cleanup, and verdict. It carries
no account, private path, fixture identity, interface, endpoint, address, port,
nonce, PID, session, packet, digest of private input, or raw process/framework
output.

## Mandatory claim mapping

All 27 mandatory rows passed in both runs:

- `authority-split` proves an ordinary controller and outer, bounded-root
  provider acquisition, irreversibly ordinary sustained owners, and a
  remote-only route;
- networkless, missing/mismatched policy, bridge allowlist, active-interface
  capacity, and provider-free MMDS rows prove policy denial and nonconsumption;
- `shared-connectivity` proves real guest DHCP and nonce-bound bidirectional
  TCP through the contained product route;
- startup remove, runtime hotplug/remove, normal teardown, partial-start,
  pre/post-ready cancellation, clean repeat, and
  `capture-restore-fresh-ownership` prove generation and lifecycle reversal;
- provider, broker, owner, launcher, and worker death plus their five exact
  SIGKILL reclamation rows prove role-specific convergence; and
- `concurrent-noninterchangeability` proves independent policy/session
  authority and survival of the unaffected peer.

Those mandatory rows close the remaining production topology, positive shared
connectivity, service/crash reclamation, fresh restore ownership, repeat,
concurrency, and cleanup obligations for both capabilities. Existing checked
network device, virtio, limiter, metrics, MMDS, snapshot, Provider-v1, and
policy implementation evidence remains part of the composed result.

The full redacted result and repetition facts are recorded in
[#1948 evidence](https://github.com/seven332/bangbang/issues/1948#issuecomment-5747681356).
The result-specific solution challenge is
[#1948 challenge](https://github.com/seven332/bangbang/issues/1948#issuecomment-5747683837).

## Optional nondependencies

Exactly four rows remain visibly `environment-gated`:

- `host-connectivity`
- `bridged-connectivity`
- `not-authorized`
- `sharing-service-busy`

Positive host/bridged traffic depends on caller fixture/interface availability;
the two typed service results depend on externally induced platform state.
Neither capability claims that all four conditions were positively observed.
Host/shared/bridged policy remains implemented and checked through mandatory
policy/allowlist/noninterchangeability paths, while positive applicable
connectivity is the mandatory real shared path. Typed error handling remains
portable checked behavior, and mandatory startup, cancellation, partial
cleanup, death, and SIGKILL cases cover the claimed production lifecycle.
No capability implementation or validation list uses an optional row as
required evidence.

## Terminal production vmnet outcome

The exact inventory transition is:

```text
383 implemented / 0 audit-required / 2 missing-platform-feasible / 33 impossible
385 implemented / 0 audit-required / 0 missing-platform-feasible / 33 impossible
```

Only `corpus:network-setup` and
`semantic.network:virtio-net-vmnet-policy-and-connectivity` move from
`missing-platform-feasible` to `implemented-and-verified`. Their families and
source identities do not change. Every other capability record is protected by
the audit's unrelated-inventory digest.

The #1930 feasibility record retains its original `383/0/2/33` transition and
nonclaims. Earlier Wave 7, Wave 8, jailer, isolation, containment, and
production-host authorities likewise retain their original counts and accept
this result only as the exact terminal successor.

The optional Apple-authorized schema-v1 path remains supported but is not a
completion dependency. The terminal product claim is the no-Apple schema-v2
path described here.

## Checked validation

`production-vmnet-certification-audit.json` binds the evidence path/digest,
two byte-identical executions, source/platform identity, result and challenge
comments, exact optional set, closed claim mappings, counts, transitions, and
unrelated inventory.

The capability audit rejects unknown fields, noncanonical bytes, changed
evidence, wrong source/platform/authority/entitlements, missing/reordered or
changed cases, optional cases used as required evidence, incomplete cleanup or
verdict, repetition/challenge drift, stale local references, an incomplete
capability record, wrong terminal identities/counts, and unrelated inventory
drift. The scoped gate is:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- \
  validate --production-vmnet-final
```

Global `validate --final` must pass at the exact terminal phase.
