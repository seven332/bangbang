# Private vmnet Provider Protocol

The `bangbang-session` crate defines a closed `provider-v1` control and packet
protocol for a future split vmnet topology. It is portable protocol machinery,
not a production network path: this revision starts no broker or owner process,
calls no vmnet API, grants no worker resource, changes no bundle, and requires
neither root nor Apple authorization.

The intended later topology keeps framework authority outside the sandboxed
worker. A bootstrap-owned broker starts a narrowly scoped interface owner; the
worker receives only a session-bound packet stream. Later adapters must still
prove the privilege drop, sandbox grant, crash reclamation, process assembly,
and real guest connectivity before this becomes supported production behavior.

## Components

- `bangbang-unix-stream` owns deadline-bounded exact byte transfer and
  `SCM_RIGHTS` adoption for already-connected Unix streams. It owns no listener,
  path, process, peer authorization, or application framing.
- `bangbang_session::vmnet_provider` owns identities, policy slots, frame
  encoding, role-specific state, packet bounds, and descriptor correlation.
- `VmnetProviderTransport` composes both layers. A transport failure shuts down
  the stream and poisons that transport permanently.

The same shared Unix-stream primitive backs the existing portable vhost-user
frontend without changing its public protocol.

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
`Stopped(Complete)` permits only `Shutdown`, and uncertain cleanup poisons the
stream.

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
```

This protocol deliberately changes no capability disposition. The checked
inventory remains `383 implemented / 0 audit-required / 2
missing-platform-feasible / 33 proven-platform-impossible`; the two retained
rows are `corpus:network-setup` and
`semantic.network:virtio-net-vmnet-policy-and-connectivity`.
