# Firecracker v1.16.0 Jailer, Seccomp, and macOS Containment Contract

This contract closes only
`semantic.isolation:jailer-seccomp-and-macos-containment-outcomes` for #1918.
It composes existing terminal capabilities and current tracked evidence; it does
not add a runtime mechanism, entitlement, credential, privileged service, or
deployment claim.

## Pinned source identity

The canonical
[`jailer-seccomp-containment-audit.json`](jailer-seccomp-containment-audit.json)
pins Firecracker v1.16.0 commit
`d83d72b710361a10294480131377b1b00b163af8` and five complete source blobs:

| Source | Manifest identity | Blob |
| --- | --- | --- |
| `docs/design.md` | `corpus:design` | `143fef76410e4f7e45b32d3986e0d78eedf5175a` |
| `docs/jailer.md` | `corpus:jailer` | `fa5e8b4ee769f64ee83a317dce5902ffd0029a1b` |
| `docs/prod-host-setup.md` | `corpus:production-host` | `8939b56a965963d8df1c44c583dcd38361197347` |
| `docs/seccomp.md` | `corpus:seccomp` | `0611fd8d602a08deaa3e5174a4b32953427c9dc9` |
| `docs/seccompiler.md` | `corpus:seccompiler` | `50f44097cece19d2538e054ee7e3b6ba457c7a55` |

`corpus:design` is mandatory: the checked Wave 7 threat-containment mapping
assigns that source to this composite. The authority contains 46 ordered source
clauses. Each clause fixes its source, normalized anchor, outcome, and one or
more evidence profiles; order, duplicates, unknown records, and blob drift are
rejected.

## Checked composition

The implemented macOS outcome is the fixed production topology:

- an entitlement-free launcher and a separately signed App Sandbox plus
  Hypervisor worker with exact static and live code validation;
- authenticated pre-launch policy, validation before mutation, suspended spawn
  and resume, a marker-only environment, default-close descriptor inheritance,
  and a locked private runtime namespace;
- exact worker `RLIMIT_NOFILE` and `RLIMIT_FSIZE`, typed grants, bounded
  lifecycle and broker protocols, no ambient path fallback, and redacted
  diagnostics;
- deterministic cancellation, reap, replacement-safe cleanup, both process
  death orders, and concurrent same-ID noninterchangeability.

The portable seccompiler compiles the pinned JSON contract into bitcode/BPF
artifacts and publishes outputs transactionally. This does not claim that macOS
installs Linux seccomp filters.

The literal Linux seccomp, cgroup, network namespace, PID namespace, arbitrary
`uid`/`gid`, and configurable chroot identities retain their existing
`proven-platform-impossible` dispositions. App Sandbox, code signing, rlimits,
HVF, vmnet admission, grants, and supervision are not renamed as those Linux
mechanisms.

## External and operator boundaries

At the #1918 transition, three audit-required records remained exact external
dependencies:

- `corpus:network-setup` and
  `semantic.network:virtio-net-vmnet-policy-and-connectivity` retain #1378
  ownership of approved Apple credentials and positive production vmnet
  start, packets, connectivity, teardown, and reclamation;
- `corpus:production-host` retains deployment signing/notarization, host
  firewall and egress, capacity and admission, monitoring, output retention,
  maintenance, and other aggregate operator policy.

Canonical vmnet admission and networkless fail-closed denial are composed here;
positive vmnet connectivity is not. General dynamic resource brokerage and hard
revocation, caller-defined runtime sandbox policy, cross-filesystem atomic
publication, global cross-launcher allocation, private Seatbelt policy,
setuid/privileged helper topology, automatic restart or a long-lived service,
malicious same-bundle sibling isolation, and Developer ID/notarization or
deployment are explicit nonclaims or independent outcomes.

## Exact inventory transition

| Capability | Delivery | Disposition |
| --- | --- | --- |
| `semantic.isolation:jailer-seccomp-and-macos-containment-outcomes` | #1918 | `implemented-and-verified` |

The predecessor is exactly `381/3/1/33`; the successor is exactly
`382/3/0/33`. A SHA-256 digest pins every unrelated capability record. Only the
#1918 row repairs its source set, clears delivery ownership, and gains terminal
evidence. Those three audit-required rows and all 33 platform exclusions were
unchanged by #1918. #1920 later certifies only `corpus:production-host`,
producing the exact `383/2/0/33` successor while preserving the two #1378 rows.

## Terminal jailer/seccomp containment outcome

The fixed macOS containment outcome, portable seccompiler outcome, terminal
Linux mechanism limits, external dependencies, operator responsibilities,
residual classifications, and nonclaims above are complete for this one
semantic identity. No narrower evidence is promoted into positive vmnet,
credential, deployment, general broker, hard-revocation, or mechanism-parity
claims.

Validate the scoped terminal result with:

```console
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --jailer-seccomp-containment-final
```

The global `--final` gate remains intentionally stronger and continues to fail
while the two independently owned #1378 audit-required records remain open.
