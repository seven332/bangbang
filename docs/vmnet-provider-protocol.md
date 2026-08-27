# Private vmnet Provider Protocol

The `bangbang-session` crate defines the closed `provider-v1` control and packet
protocol for a split vmnet topology. The `bangbang-vmnet-provider` package now
implements its minimal one-shot root broker and one privilege-dropped process
per interface. Exact-host evidence calls the real vmnet API under explicit root
authority and requires no Apple-approved vmnet entitlement, provisioning
profile, or signing identity.

The networkless contained worker accepts one authenticated
`vmnet-provider-stream` startup grant and adapts it to the existing process
network registry. The production bundle now includes the separately signed,
entitlement-free provider and an explicitly elevated one-shot bootstrap. The
complete launcher/provider/worker/owner topology is assembled and supervised
without an Apple developer identity or vmnet provisioning profile. Full real
guest-through-provider lifecycle and concurrency certification remains a
separate slice, so neither retained network capability is promoted here.

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
- `bangbang-launcher` packages the provider at one fixed sibling path, adopts
  its inherited connected stream as the unique provider grant, binds it to the
  lifecycle and launch policy, and supervises the sandbox worker.
- `bangbang` owns the contained client adapter. A session-scoped control pump
  and one data pump per interface are the only readers and writers of their
  Provider-v1 streams; the existing process network registry continues to own
  MMDS, queues, limiters, metrics, hotplug, snapshot reconstruction, and
  cleanup.
- The `bangbang` library exposes only its host-network backend view to this
  package. The privileged entry point does not link the VMM, HTTP API, guest,
  launcher, bundle, grant, or listener modules.

The same shared Unix-stream primitive backs the existing portable vhost-user
frontend without changing its public protocol.

## Packaged elevated topology

`Bangbang.app/Contents/Helpers/bangbang-vmnet-provider` is a separately signed
code object with fixed identifier `dev.bangbang.vmnet-provider`, Hardened
Runtime, and no App Sandbox, Hypervisor, vmnet, application, or team
entitlement. Its public `--bootstrap-v1` mode accepts only a numeric nonroot
uid/gid, an optional daemon bit, one delimiter, and opaque launcher arguments.
It accepts no executable, bundle, socket, config, account, signing, or helper
path and never handles sudo credentials. The caller invokes this exact helper
through its chosen elevation mechanism; repository code does not invoke
`sudo`.

Root executes only the minimal provider image. The bootstrap pins its own image
and the fixed sibling outer and worker layout, then suspended-spawns the same
provider image in a descriptor-gated private transition mode. That child clears
supplementary groups, changes gid before uid, attests the irreversible target
identity, revalidates the pinned outer, fixes standard input to `/dev/null`, and
only then execs it. Consequently no sudo input reaches the outer or worker, and
the outer loader, runtime, argument and manifest parsing, API/HVF work, and
worker supervision all begin as the ordinary target user. The root parent validates
the post-exec outer image, PID, credential, and correlation before allowing it
to proceed.

The root broker and ordinary launcher exchange only canonical descriptor-free
192-byte topology frames. The protocol binds the target credential, lifecycle
session, exact host/shared/bridge-slot/count authority, process roles, launch
mode, readiness, cancellation, and terminal acknowledgement. The launcher
adopts the fixed topology and provider descriptors once, validates the root
peer independently, and converts the provider endpoint into the unique
`vmnet-provider-stream` grant using the verified provider executable as its
source identity. It never publishes or reconnects through a filesystem socket,
and pager/provider roles cannot substitute for each other.

Foreground mode retains the caller-attached root broker. Daemon mode uses a
bounded same-image root handoff and the same drop-before-outer-exec sequence;
the detached broker owns the ordinary launcher, which owns the sandbox worker,
while the broker also owns every dropped interface owner. Broker, launcher,
worker, or owner failure, signal, EOF, protocol error, or timeout cancels the
whole topology, and success requires correlated terminal acknowledgement,
owner retirement, process reap, and empty topology residue. Root never parses
VM resources or handles guest packets; only the per-interface process starts
vmnet while root and performs sustained packet work after its irreversible
drop.

## Contained worker route and ownership

Lifecycle policy authenticates exactly one backend route: `Denied`,
`LocalSystem`, or `RemoteProvider`. The launcher admits only these combinations:

- networkless profile, denied vmnet policy, and no provider grant;
- networkless profile, positive canonical policy, and exactly one provider
  grant;
- optional Apple-authorized vmnet profile, positive policy, and no provider
  grant.

Every other profile/policy/grant combination fails before worker execution.
The worker checks the same matrix after committing its startup batch, includes
the route in process network ownership identity, and never changes route or
falls back after a failure. Pager and provider connected-stream roles are
noninterchangeable and one-time.

The remote source claims and handshakes the provider stream lazily on the first
actual vmnet-backed interface. An MMDS-only startup, runtime insertion, or
restore validates policy but never claims or contacts the provider. Host and
shared requests map directly to fixed slots; bridged requests map only by
equality against bootstrap-owned bridge slots zero through three. The bridge
name itself never crosses the protocol.

The remote backend builds only an in-process policy descriptor. It does not
construct an XPC dictionary, resolve local vmnet keys, or call any
Hypervisor.framework/vmnet acquisition API in the contained worker. The signed
networkless bundle test runs two complete fake-provider packet lifecycles plus
one unclaimed-grant lifecycle using ad-hoc signing, no vmnet entitlement, and no
Apple developer identity.

One bounded command queue wakes the sole control pump for Start, Stop, Cancel,
and Shutdown. Each successful Start handshakes its transferred stream before
publication and creates one sole data pump for readiness, read, write,
callback drain, Stop, and Shutdown. Requests use bounded waits; abandoned or
expired work terminalizes its pump, and a late raced `Started` stream is
retired inside the control state. Normal release drains callbacks, completes
data Stop/Shutdown, and only then retires the exact control generation. Restore
retains only the session source and always requests a fresh interface and
generation; no provider stream or generation enters a snapshot.

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
cargo test -p bangbang --all-features --locked host_network::remote_vmnet::tests::
```

Provider tests additionally cover canonical bootstrap/supervision records,
slot resolution, four-owner capacity, monotonic generation reuse, independent
failure, data-first retention, ordered cancellation, credential/service order,
cleanup uncertainty, redaction, exact image mismatch, suspended pre-resume
cleanup, and the static root linkage surface. The prepared exact-host workflow
adds real provider-v1 data lifecycle, control cancellation, clean repeat, and
empty-residue cases before the existing dropped-owner and repeated guest gates.
Its fixed successful status explicitly records `apple-vmnet=absent`.

The production topology gate builds and signs as the ordinary target user, then
uses one caller-authorized exact-root invocation. It verifies the fixed three-
code-object layout and entitlement split, real shared-provider start/read/write/
stop twice, outer and provider signal convergence, provider-owned daemon
handoff, and exact cleanup. The runner never invokes `sudo`, consumes no stdin,
and reports only fixed categories:

```sh
scripts/prepare-production-vmnet-topology.sh \
  --output /absolute/absent/Bangbang.app
sudo -- /usr/bin/python3 scripts/run-production-vmnet-topology.py \
  --prepared /absolute/absent/Bangbang.app \
  --target-uid TARGET_UID \
  --target-gid TARGET_GID
```

Its exact success line is:

```text
bangbang production vmnet topology proof: provider=passed repeat=passed outer-signal=passed provider-signal=passed daemon=passed cleanup=passed
```

This implementation deliberately changes no capability disposition. The
checked inventory remains `383 implemented / 0 audit-required / 2
missing-platform-feasible / 33 proven-platform-impossible`; the two retained
rows are `corpus:network-setup` and
`semantic.network:virtio-net-vmnet-policy-and-connectivity`. The packaged
elevated bootstrap and foreground/daemon supervision are now implemented. A
real guest through this production provider, the complete concurrent
lifecycle/death matrix, optional Apple-authorized evidence, and final capability
certification remain successor work.
