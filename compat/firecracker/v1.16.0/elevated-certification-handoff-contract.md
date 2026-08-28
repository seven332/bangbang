# Elevated Certification Handoff Contract

## Scope and inventory position

This contract owns #1943, foundation child 2 of #1941 under #1348. It supplies
the least-privileged process-control bridge between an ordinary future
production-vmnet certification controller and the caller-authorized exact-root
provider bootstrap. It does not execute the canonical guest matrix, publish a
result, promote a capability, or replace the direct #1930/#1942 evidence.

The live inventory remains exactly `383 implemented / 0 audit-required / 2
missing-platform-feasible / 33 proven-platform-impossible`. The retained rows
are `corpus:network-setup` and
`semantic.network:virtio-net-vmnet-policy-and-connectivity`.

## Ordinary immutable preparation

Preparation requires Darwin arm64, an ordinary uid/gid, a clean Git index,
worktree, and untracked-file set, and an absolute absent destination named
`bangbang-elevated-vmnet-handoff`. It records the exact HEAD commit and tree,
builds `Bangbang.app` through the normal production bundle command with its
default ad-hoc identity and networkless worker profile, and independently
checks:

- the fixed outer/provider/worker paths and identifiers;
- Hardened Runtime on all three signed objects;
- empty outer and provider entitlements;
- exactly App Sandbox plus Hypervisor worker entitlements;
- no embedded provisioning profile; and
- no test-feature product or Apple-authorized input.

The canonical manifest binds the source, four fixed implementation files, and
every relative bundle directory/file name, kind, mode, size, and SHA-256. All
regular nodes are single-link and mode `0444` or `0555`; all directories are
nonwritable. Links, special files, hard links, writable nodes, unknown residue,
source drift, replacement, and a pre-existing output fail. Publication uses
`renamex_np(RENAME_EXCL)` and never replaces a destination.

## Root invocation authority

The public root entry accepts only the prepared absolute package and canonical
nonzero u32 target uid/gid. It closes stdin. It does not invoke an authorization
frontend or accept/discover an executable, alternate bundle, environment, cwd,
fixture, result, account, credential, profile, interface, or socket.

Caller authorization of the fixed repository entry is the trust decision.
Implementation hashes prove that package/source and the authorized entry are
coherent and reject a stale package; a program digest checked by the same
authorized program is not claimed as authentication against modified code.

Root validates the target-owned package, copies it without following links into
a fresh `/private/var/tmp` root stage, normalizes root ownership and immutable
modes, revalidates source and staged manifests, repeats every signature and
entitlement check, then changes only the outer stage to traversal mode. The
staged `bangbang-vmnet-provider` is the only root product image that can be
spawned.

## Guardian, supervisor, and controller

The one-shot root entry divides into a guardian and protocol supervisor with
reciprocal liveness. The supervisor forks exactly one controller. Before any
controller loader or future private input is parsed, the child:

1. redirects stdin/stdout/stderr to `/dev/null` and closes unrelated fds;
2. clears supplementary groups;
3. calls `setgid(target)` and then `setuid(target)`;
4. attests real/effective/saved uid and gid, Darwin's exact target-gid-only
   `effective-only` group-access-list postcondition, parent, executable, and
   process birth identity; and
5. proves attempts to restore uid 0, gid 0, and a root group all fail without
   changing that identity.

The supervisor independently attests the same dropped child before sending a
welcome. Only the controller may later load private config, fixture, API/HVF,
guest-matrix, or result code.

Group attestation uses Darwin's bounded live-process `getgroups` ABI directly,
not the system Python deployment-target account-directory variant that ignores
`setgroups` changes.

## Fixed descriptor protocol

The controller and supervisor use a connected `AF_UNIX/SOCK_DGRAM` socketpair.
Both ends set and read back `SO_SNDBUF` and `SO_RCVBUF` at no less than 8192
bytes before traffic; target macOS defaults are insufficient for one atomic
record. Every record is exactly 4096 bytes:

- 96-byte big-endian prefix with magic/version, role/kind, descriptor count,
  sequence, request correlation, 32-byte random session, handle/value, payload
  length, and zero reserved bytes;
- 32-byte SHA-256 of the prefix plus live payload;
- a bounded payload; and
- an all-zero unused tail.

Per-role sequences start at one and advance exactly. Responses bind one request
sequence. Replay, skip, reordering, wrong role/session/correlation, unknown
kind/handle, malformed length/digest/reserved/tail, truncation, ancillary
truncation, and post-terminal traffic fail the session.

The graph contains only welcome/ready, spawn/spawned, poll, bounded wait,
running/exited, TERM, KILL, close/closed, finish/finished, and categorical
failure. Controller requests carry no descriptors. Spawn success alone carries
exactly two distinct read-only pipe descriptors; wrong, duplicate, excess,
mixed, non-pipe, writable, or truncated descriptors are closed and rejected.

The spawn payload is a bounded byte-preserved argument vector. Root validates
only its structure and never emits or semantically interprets it. Root prepends
the exact staged provider, `--bootstrap-v1`, and target ids, and fixes `/` cwd,
`LANG/LC_ALL=C`, null stdin, closed unrelated descriptors, and a fresh process
group. The provider itself pins its sibling outer/worker and preserves its
drop-before-broad-parse contract.

## Lifecycle and cleanup

Each process handle supports poll, bounded wait, exact process-group TERM/KILL,
and close. Output pumps start immediately, retain only a bounded prefix, and
make overflow, reader failure, or stuck EOF terminal. Partial pipe creation,
partial spawn, response-transfer failure, timeout, protocol failure, controller
loss, and normal close retire and reap every owned group.

On success the supervisor sends `complete` but stays alive. The guardian then
proves the controller and every exact staged provider/launcher/worker image
absent using only `pid/ppid/state/comm`, removes the stage, and sends `ack`.
Only then may the supervisor exit successfully. Supervisor loss makes the
guardian the cleanup owner; guardian loss makes the supervisor the cleanup
owner. Forced cleanup or identity/absence uncertainty fails even if residue is
eventually removed. Simultaneous destruction of both root actors is an external
administrator/kernel event and would require a persistent service, which this
contract deliberately excludes.

## Fixed probes and handoff

The #1943 real gate uses the normal bundle for two clean version completions and
two live ordinary API-process cases, one terminated and one killed. Fixed
private API-grant material is created only inside the dropped controller and is
removed before finish. Public output is categorical and contains no private
path, id, PID, session, arguments, interface, address, packet, nonce, or raw
tool/process value.

Portable tests own canonical package/tamper behavior, every record region,
argument bounds, configured datagram plus rights transfer, sequence/replay/
cross-session rejection, descriptor confusion, credential order and
irreversibility, partial spawn closure, output overflow, reciprocal loss, and
cleanup acknowledgment. The target gate requires caller-arranged exact root
and ad-hoc signing only; it has no unsupported-success or Apple-authorization
fallback.

#1944 may import the ordinary `ControllerProxy`/`RemoteProviderProcess` seam and
adapt the canonical process factory after drop. It alone owns private plan
parsing, guest/provider concurrency, canonical result publication, capability
promotion, and parent completion.
