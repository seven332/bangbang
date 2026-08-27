# Firecracker v1.16.0 Production Host Contract

This contract closes only `corpus:production-host` for #1920. It certifies
complete, reproducible accounting of the pinned Firecracker production-host
recommendations. It does not claim that bangbang performs operator maintenance,
owns host-global policy, carries distribution credentials, or has executed the
external positive vmnet gate.

## Pinned source identity

The canonical
[`production-host-audit.json`](production-host-audit.json) pins Firecracker
v1.16.0 commit `d83d72b710361a10294480131377b1b00b163af8` and the complete
`docs/prod-host-setup.md` blob
`8939b56a965963d8df1c44c583dcd38361197347`.

The authority contains 31 ordered source clauses. They cover kernel and
microcode maintenance, seccomp, serial and log output, host logging, signal
recovery, jailer containment, identities and limits, network and storage
fairness, swap and hardware policy, ARM timer behavior, vulnerability checks,
and Linux 6.1 KVM/cgroup regressions. Order, duplicates, unknown records,
anchors, source identity, and canonical bytes are checked.

## Checked product and platform outcomes

The fixed macOS product implements separately signed App Sandbox/HVF worker
containment, an entitlement-free launcher, validation before mutation, private
runtime namespaces, exact typed resource grants, `RLIMIT_NOFILE` and
`RLIMIT_FSIZE`, bounded serial/log/metrics destinations, block/network token
buckets, cleanup, redaction, crash convergence, and concurrent
noninterchangeability.

Literal Linux/KVM seccomp installation, cgroups, namespaces, arbitrary uid/gid,
configurable chroot, KVM PIT, kernel module parameters, procfs checks, Linux KSM,
and x86 Linux 6.1 mitigations are not macOS aliases. Existing challenged
platform results remain unchanged. The ARM product normalizes timer state
through public HVF behavior without claiming `KVM_CAP_COUNTER_OFFSET` identity.

## Operator, hardware, and deployment boundaries

Host and guest patching, microcode and firmware maintenance, firewall and
egress policy, swap configuration, fleet capacity/admission, output retention,
watchdog/restart policy, hardware and side-channel mitigation, SMT/memory
hardware selection, vendor guidance, and physical-host certification remain
operator-owned outcomes.

The pinned Firecracker signal-handler deadlock is an implementation-specific
hazard, not a requirement to reproduce that unsafe handler design. Developer
ID/notarization and deployment remain independently owned; no credential is
enumerated or claimed.

The positive production vmnet start, packets, connectivity, service failures,
teardown, SIGKILL reclamation, repeat execution, concurrency, and Apple-approved
identity/profile evidence remain exclusively owned by #1378. Sudo or root does
not confer the restricted entitlement. #1930 later proves that direct shared
vmnet and a privilege-dropped owner are feasible without that entitlement, so
`corpus:network-setup` and
`semantic.network:virtio-net-vmnet-policy-and-connectivity` become
`missing-platform-feasible`; it does not change this production-host result.

#1378 now has a credential-free protocol and contained-worker adapter plus a
checked production runner: strict private config and redacted result handling, a retained
digest-pinned fixture exchange, a direct-rootfs-v110 DHCP/TCP guest oracle,
two-package inspection, descriptor-rooted grants, and the fixed 21-case
policy/API/process/death matrix. Portable injection cannot turn fixture state
into authorization, service-error, connectivity, or cleanup evidence, and no
caller-approved restricted-credential execution has been recorded. This
contract's external handoff and inventory totals are therefore unchanged.

## Exact inventory transition

| Capability | Delivery | Disposition |
| --- | --- | --- |
| `corpus:production-host` | #1920 | `implemented-and-verified` |

The predecessor is exactly `382/3/0/33`; the successor is exactly
`383/2/0/33`. A digest pins every unrelated inventory record. Only the owned
corpus row gains the canonical authority, contract, validator, and focused test
evidence. The two #1378 rows remain unchanged.

## Terminal production-host corpus outcome

Every stable normative clause in the pinned source is represented exactly once
as an implemented macOS outcome, an implemented outcome with a terminal literal
mechanism limit, a platform/architecture limit, an operator-owned result, an
implementation-specific nonrequirement, or external evidence. A terminal corpus
therefore means complete checked source accounting, not universal host-policy or
deployment execution.

Validate this scoped result with:

```console
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --production-host-final
```

The global `--final` gate remains intentionally stronger and continues to fail
on the exact two #1378 `missing-platform-feasible` records.
