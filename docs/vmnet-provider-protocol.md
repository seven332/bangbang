# Private vmnet Provider Protocol

The `bangbang-session` crate defines the closed `provider-v1` control and packet
protocol for a split vmnet topology. The `bangbang-vmnet-provider` package now
implements its minimal one-shot root broker and one privilege-dropped process
per interface. Exact-host evidence calls the real vmnet API under explicit root
authority and requires no Apple-approved vmnet entitlement, provisioning
profile, or signing identity.

This is still not a production network path. No launcher-to-root bootstrap,
operator authorization workflow, bundle assembly, sandbox grant/adapter, or
guest-through-provider integration exists in this slice. The contained worker
does not yet consume the transferred packet stream, and neither retained
network capability is promoted.

## Components

- `bangbang-unix-stream` owns deadline-bounded exact byte transfer and
  `SCM_RIGHTS` adoption for already-connected Unix streams. It owns no listener,
  path, process, peer authorization, or application framing.
- `bangbang_session::vmnet_provider` owns identities, policy slots, frame
  encoding, role-specific state, packet bounds, and descriptor correlation.
- `VmnetProviderTransport` composes both layers. A transport failure shuts down
  the stream and poisons that transport permanently.
- `bangbang-vmnet-provider` owns the canonical private bootstrap, bounded policy
  resolution, broker/owner supervision, exact child lifetime, credential
  type-state boundary, and the macOS adapter to the existing
  `SystemVmnetInterfaceBackend`.
- The `bangbang` library exposes only its host-network backend view to this
  package. The privileged entry point does not link the VMM, HTTP API, guest,
  launcher, bundle, grant, or listener modules.

The same shared Unix-stream primitive backs the existing portable vhost-user
frontend without changing its public protocol.

## Broker and owner process boundary

The private broker accepts only fixed inherited connected Unix streams. Its
canonical 128-byte bootstrap contains one nonzero lifecycle session, an exact
nonroot uid/gid target, a one-through-four active-owner limit, and fixed
host/shared/bridge-slot authority. Provider-v1 can select only those slots and
bounded typed request parameters; it cannot supply an executable, path,
command, environment value, raw bridge name, credential, signing value, or
arbitrary string.

For each accepted Start, the broker assigns a nonwrapping generation and
self-spawns the same exact provider image in the fixed owner mode. Darwin
default-close spawn actions retain only `/dev/null` standard streams plus the
fixed supervision and data endpoints. The root-owned, single-link,
non-writable executable is opened and its device, inode, link count, uid, gid,
and mode are pinned. The child starts suspended, the vnode identity of its
kernel-reported first executable mapping is matched directly to that identity,
and only then is it resumed. A mismatch or resume failure terminates and reaps
the exact child before it can call vmnet.

The internal 160-byte supervision family is descriptor-free and closed to
broker-owned Bootstrap/Stop plus owner-owned Ready/Failed/Final records. It has
no packet, readiness, read/write, worker-control, path, or command field. The
owner starts and freezes the existing system vmnet backend while exact root,
then uses the production credential primitive to clear supplementary groups,
call setgid before setuid, prove the irreversible prefix, and re-attest the
configured real/effective identity. Only the resulting `DroppedOwner` type can
enable callbacks or perform packet reads and writes.

Clean data-first completion remains correlated in the broker ledger until the
matching control Stop. Control cancellation retires every owner in deterministic
interface order. Owner death, missing Final, timeout, identity mismatch, or
unprovable stop is terminal cleanup uncertainty; any broker error runs the same
session-wide cleanup before exit.

## Fixed wire contract

Every integer is network byte order. Every session, interface, generation, and
sender sequence is checked before a state transition. Reserved bytes must be
zero.

| Bytes | Field | Rule |
| --- | --- | --- |
| `0..8` | magic | `BBVNETP\0` |
| `8..10` | version | exactly `1` |
| `10` | channel | control `1`, data `2` |
| `11` | kind | closed message discriminant |
| `12..16` | body length | kind-specific and globally bounded |
| `16` | descriptor count | `1` only for control `Started`; otherwise `0` |
| `17..24` | reserved | all zero |
| `24..56` | lifecycle session | nonzero exact 32-byte identity |
| `56..60` | interface | kind-specific nonzero scope or zero |
| `60..64` | reserved | all zero |
| `64..72` | generation | kind-specific nonzero scope or zero |
| `72..80` | sender sequence | nonzero, contiguous, and nonwrapping |

Only six bootstrap-owned policy slots cross the control channel: `host`,
`shared`, and bridge slots zero through three. No pathname, bridge name, raw
selector, command, credential, PID, framework error, or arbitrary string can
be encoded.

## Bounds

| Resource | Maximum |
| --- | ---: |
| Active interfaces per session | 4 |
| Packets in one operation | 200 |
| Aggregate packet bytes | 256 KiB |
| One packet buffer | 65,574 bytes, including an enabled 12-byte virtio header |
| Provider frame buffering | two maximum frames |
| Complete frame deadline | 5 seconds |
| Shared transport descriptors | 32; provider frames use at most 1 |

Realized backend parameters further narrow the packet and read/write batch
limits. Read results may be empty. Writes must be nonempty, preserve packet
order, and acknowledge only a zero-to-requested completed prefix.

## Control lifecycle

The client and broker exchange `Hello`/`HelloAck` before work. Only one start or
stop operation may be pending, while up to four successfully started interface
generations remain active independently. `Started` must introduce a fresh
generation and is the only frame allowed to carry a connected close-on-exec
Unix stream. `StartFailed` creates no ownership. `Stopped(Complete)` retires the
exact active generation; uncertain cleanup is terminal.

Orderly `Shutdown` is legal only after all pending and active ownership is
empty. Duplicate, skipped, reordered, cross-session, cross-interface,
cross-generation, wrong-role, over-capacity, or post-terminal input clears
tracked ownership and poisons the receiving state.

Cancellation has one explicit pending-operation race. After sequenced `Cancel`,
the broker may either suppress the pending result and send `Cancelled`, or send
the already committed exact result before `Cancelled`. The client accepts at
most that one result. A raced `Started` stream is consumed and retired inside
the state transition and is never exposed to the application. Nothing may
follow `Cancelled`.

## Data lifecycle

Each transferred stream repeats its exact session, interface, and generation in
`Hello`/`HelloAck`. The client may then keep one synchronous read or write
request outstanding. The owner may publish contiguous nonzero readiness epochs
before or during a request, but the state retains at most one unconsumed edge.

Every result or failure echoes the exact request sequence. Returned packets
must fit the realized per-packet and per-batch limits; write completion is an
ordered prefix, not permission to retry or reorder the suffix. Operation
failure is terminal. `Stop` disables readiness and packet work,
although one readiness record already published before Stop may arrive before
`Stopped(Complete)`. That acknowledgement permits only `Shutdown`, and uncertain
cleanup poisons the stream.

## Descriptor and failure ownership

Transport receive reads the 80-byte header first under one absolute deadline,
adopts every delivered right immediately, validates the bounded body length,
then reads the exact body under the same deadline with rights forbidden. The
single legal `Started` right must arrive within the header byte range and must
be a connected `AF_UNIX` stream with `FD_CLOEXEC`, `SOCK_STREAM`, and zero
`SO_ERROR`.

The transport returns one owned envelope containing the decoded frame and its
optional stream. State consumes the whole envelope; callers cannot detach a
right before its transition validates. Malformed ancillary data, wrong or
excess rights, wrong placement or type, timeout, clean disconnect, partial EOF,
half-close, and local I/O failure close pending rights, shut down the transport,
and poison it. Public errors and `Debug` output expose only fixed categories and
counts, never descriptor numbers, peer identities, packet bytes, or session
values.

## Verification and current status

The portable tests cover every message kind and split point, golden framing,
reserved/limit corruption, four-interface ownership, generation reuse,
cancellation races, readiness and request correlation, partial write
completion, cross-scope poisoning, real descriptor transfer, misplaced/wrong/
excess rights, timeout and EOF classes, and a combined control-to-data
lifecycle. Run them without elevation:

```sh
cargo test -p bangbang-unix-stream --all-features --locked
cargo test -p bangbang-session --all-features --locked
cargo test -p bangbang-vhost-user --all-features --locked
cargo test -p bangbang-vmnet-provider --all-features --locked
```

Provider tests additionally cover canonical bootstrap/supervision records,
slot resolution, four-owner capacity, monotonic generation reuse, independent
failure, data-first retention, ordered cancellation, credential/service order,
cleanup uncertainty, redaction, exact image mismatch, suspended pre-resume
cleanup, and the static root linkage surface. The prepared exact-host workflow
adds real provider-v1 data lifecycle, control cancellation, clean repeat, and
empty-residue cases before the existing dropped-owner and repeated guest gates.
Its fixed successful status explicitly records `apple-vmnet=absent`.

This implementation deliberately changes no capability disposition. The checked
inventory remains `383 implemented / 0 audit-required / 2
missing-platform-feasible / 33 proven-platform-impossible`; the two retained
rows are `corpus:network-setup` and
`semantic.network:virtio-net-vmnet-policy-and-connectivity`. Production
launcher bootstrap, sandbox-worker consumption, a guest through this provider,
and final lifecycle certification remain successor work.
