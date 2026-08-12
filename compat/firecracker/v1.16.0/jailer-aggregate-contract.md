# Firecracker v1.16.0 Aggregate Jailer Contract

This contract certifies the complete observable jailer outcome delivered by
#1912 for the fixed macOS production topology. It owns exactly
`corpus:jailer` and `tool-operation:jailer/run`; the broader production-host,
vmnet, and composite isolation records remain independent.

## Pinned source identity

The authority is pinned to Firecracker commit
`d83d72b710361a10294480131377b1b00b163af8` and these immutable blobs:

| Source | Blob | Role |
| --- | --- | --- |
| `docs/jailer.md` | `fa5e8b4ee769f64ee83a317dce5902ffd0029a1b` | entire public jailer corpus |
| `src/jailer/src/main.rs` | `4f87f2563c6f6ef47cecbb2829fe91bf27a6f603` | parser, sanitization, early commands, entrypoint |
| `src/jailer/src/env.rs` | `cb3261c039cc6b83932c0bcdb9271faece107e0f` | environment construction and ordered run |
| `src/jailer/src/chroot.rs` | `56335c03a747067f81c297ad8b299837bae85d57` | Linux root transition mechanism |

The machine authority checks 13 ordered argument leaves, 16 ordered operation
steps, and seven corpus sections. Missing, duplicate, reordered, or unknown
records fail validation.

## Argument grammar

The four required value arguments are `--id`, `--exec-file`, `--uid`, and
`--gid`. Optional single-value inputs include `--chroot-base-dir`, `--netns`,
`--cgroup-version`, and `--parent-cgroup`; `--cgroup` and `--resource-limit`
are repeatable; `--daemonize`, `--new-pid-ns`, and `--version` are flags. The
upstream defaults `/srv/jailer`, cgroup version `1`, executable-basename parent
cgroup, and `no-file=2048` are explicit authority data.

Bangbang's versioned `--bangbang-jailer-v1` envelope requires the `--`
delimiter. It validates one ID, the fixed absolute embedded worker, current
credentials, repeatable last-value `fsize`/`no-file` limits, optional daemon
mode, and opaque forwarded worker bytes. Duplicates are rejected except for
the two intentionally repeatable classes. Forwarded `id` and launcher-owned
timing singletons are rejected before the worker's own delimiter so the
launcher can inject them exactly once. Exact early help and version commands do
not construct, validate, or start a worker.

The five implemented-and-verified leaves are `id`, `exec-file`,
`resource-limit`, `daemonize`, and `version`. The eight
proven-platform-impossible leaves are `uid`, `gid`, `chroot-base-dir`,
`cgroup`, `cgroup-version`, `parent-cgroup`, `netns`, and `new-pid-ns`. The
latter remain the exact challenged fixed-topology conclusions in
[the isolation contract](isolation-contract.md); aggregate certification does
not relabel them as macOS implementations.

## Ordered operation mapping

The authority preserves the pinned operation order:

1. validate paths and ID before mutation;
2. close inherited file descriptors;
3. clear the inherited environment;
4. create the private runtime root;
5. bind isolated worker code;
6. install resource limits;
7. configure cgroups;
8. enter the contained root;
9. provide network-device authority;
10. provide hypervisor authority;
11. apply root and device ownership;
12. join a network namespace;
13. detach the session and standard streams;
14. create a PID namespace;
15. apply the process identity; and
16. execute the worker with owned and forwarded arguments.

On macOS, validation is fail-closed before grant parsing/preparation, bundle
or provisioning-profile work, private staging, session creation, spawn,
publication, or worker output. Unsupported path-bearing isolation arguments
are classified from their exact option bytes before their values are decoded,
retained, or opened; diagnostics expose fixed names, not values.

The production bundle supplies the applicable observable outcomes:

- one fixed nested worker is separately signed, statically validated, spawned
  suspended, live-code validated, and only then resumed;
- private no-clobber bundle publication plus a random mode-0700 locked runtime
  namespace provides per-run mutable-state isolation and replacement-safe
  cleanup;
- default-close spawn inheritance passes only standard streams and the closed
  lifecycle/grant/broker descriptors, with a marker-only environment;
- worker-local `RLIMIT_NOFILE` and `RLIMIT_FSIZE` are installed and read back
  exactly without raising inherited hard limits; the default is
  `no-file=2048`;
- the launcher injects one ID and monotonic/process timing prefix before opaque
  worker arguments;
- same-code daemon re-exec uses `SETSID`, `/dev/null` standard streams, an
  authenticated bounded Ready/PID/Ack handoff, retained supervision, parent
  loss cancellation, concurrent isolation, signals, reap, and cleanup; and
- the separately signed App Sandbox plus Hypervisor worker executes real HVF
  guests while Linux KVM device-node creation remains inapplicable.

Literal Linux cgroup, mount/chroot, network namespace, PID namespace, device
node, and arbitrary credential-transition mechanisms retain terminal limits.
Network-device authority is not a claim that the still-open vmnet
connectivity/profile gate is complete.

## Whole-corpus observations

The seven checked sections are Disclaimer, Jailer Usage, Jailer Operation,
Example Run and Notes, Observations, Known limitations, and Caveats.

Firecracker places arbitrary jailer paths and the operator in its trusted
computing base. Bangbang narrows that authority: code is fixed and signed;
resource paths after the delimiter are untrusted and must be converted into
bounded no-follow identities before spawn. Firecracker's operator-created
hardlinks/copies become typed startup grants and retained directory anchors.
CPU/NUMA tuning through cgroups remains unavailable rather than being aliased
to Darwin scalar rlimits.

Firecracker leaves jail cleanup to its operator and documents a subscription
race. Bangbang's same-code launcher owns worker supervision and exact-inode,
replacement-safe cleanup. The Firecracker PID-file workaround becomes an
authenticated post-Ready daemon PID response while the supervisor remains
alive. Mount-count slowdown and co-mounted cgroup caveats are inapplicable
because Bangbang does not build Linux mount/cgroup hierarchies.

## Terminal aggregate outcome

The #1912 transition is exactly `377/5/3/33` to `379/3/3/33`. A checked digest
pins every unrelated capability record, so changing an unrelated summary,
source, disposition, ownership marker, or evidence fails even when cardinality
is preserved. At the exact `380/3/2/33` multiprocess successor,
`381/3/1/33` host-resource successor, `382/3/0/33` containment successor, and
`383/2/0/33` production-host successor, the checked digest advances to include
those independently certified rows
while the two #1912 rows and their original transition remain unchanged.

| Capability | Owner | Disposition |
| --- | --- | --- |
| `corpus:jailer` | #1912 | `implemented-and-verified` |
| `tool-operation:jailer/run` | #1912 | `implemented-and-verified` |

At the original #1912 checkpoint, the three remaining audit-required records
were `corpus:network-setup`, `corpus:production-host`, and
`semantic.network:virtio-net-vmnet-policy-and-connectivity`. The multiprocess
and host-resource composites are separately terminal under #1914 and #1916.
The final jailer/seccomp/macOS-containment composite is terminal under #1918;
#1920 then certifies the production-host corpus. Only the two #1378 network
records remain audit-required; no feasible record remains, and the 33 exact
platform exclusions remain unchanged.

This outcome explicitly does not claim Linux jailer mechanism parity, a
literal per-run executable copy, absence of shared read-only code pages,
arbitrary trusted path authority, positive arbitrary credential transition,
positive configurable chroot, Linux cgroup/namespace/device-node behavior,
external vmnet connectivity, production-host deployment, Developer ID or
notarization, or an automatic restart/long-lived service.

## Certification

The scoped terminal gate is:

```console
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --jailer-final
```

It composes delivery validation, the canonical aggregate authority, all 13
terminal leaves, current-tree path-and-anchor evidence, exact inventory
transition, unrelated-record digest, and the two checked contract rows. The
ordinary delivery command also validates the authority structure.

The global `--final` mode remains stronger and intentionally fails while the
two #1378 audit-required records remain. The
signed producer scenarios run through `scripts/run-integration-tests.sh`
without `--allow-unsupported`; no sudo is required by this aggregate slice.
