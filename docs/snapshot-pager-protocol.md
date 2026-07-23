# `bangbang-pager-v1` Protocol

This document is the normative wire, lifecycle, and failure contract shared by
the bangbang VMM snapshot coordinator and its future external page-content
peer. The wire implementation is the standalone `bangbang-pager` crate; the
backend-neutral anonymous-memory coordinator is
`bangbang_runtime::lazy_memory`.

The crate adopts an already connected Unix stream. It does not select or open a
path, launch a process, transfer a descriptor, grant source authority, map
guest memory, own a Mach/HVF object, or make native-v1 `Uffd` succeed. Those
integration steps remain under delivery parent
[#1527](https://github.com/seven332/bangbang/issues/1527).

## Compatibility boundary

`bangbang-pager-v1` preserves the observable external-content ownership needed
by the pinned Firecracker page-fault corpus.
It is not Linux UFFD descriptor or wire compatibility.
Firecracker transfers a Linux
`userfaultfd` plus host virtual addresses and then receives kernel events.
Bangbang keeps task/thread ports and host addresses inside the VMM and sends
explicit bounded requests containing only opaque identifiers and offsets.

An unmodified Firecracker JSON/SCM_RIGHTS handshake, a UFFD descriptor, or UFFD
event bytes do not start this protocol and fail the fixed header check.

## Encoding and global bounds

All integers are unsigned and big-endian. Every reserved byte and flag must be
zero. Receivers reject unknown magic, versions, kinds, operation bits, flags,
reserved values, body sizes, identifiers, or trailing bytes.

The fixed 24-byte header is:

| Offset | Size | Field | Required value |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | `BBPAGER\0` |
| 8 | 2 | version | `1` |
| 10 | 2 | kind | one closed kind in the message table |
| 12 | 4 | body length | exact bytes after this header |
| 16 | 4 | flags | zero |
| 20 | 4 | reserved | zero |

Every body starts with a random, nonzero 32-byte session identity. The maximum
encoded frame is 2,097,248 bytes: the header, 72 bytes of page metadata
including the session, and one 2 MiB page. A receiver validates the advertised
body length against that maximum before allocating its body buffer.

Negotiated limits obey all of these rules:

- page size is a power of two from 4 KiB through 2 MiB;
- region count is 1 through 128;
- the combined outstanding page/removal count is 1 through 256;
- maximum frame size is no larger than 2,097,248 and is at least
  `24 + 72 + page_size`; and
- the v1 operation mask is exactly `0x0000001f`: page data, page zero, removal,
  cancellation, and shutdown.

The peer may reduce only the offered in-flight and maximum-frame limits. It
must select the offered page size, region count, and complete operation mask.

## Message bodies

Sizes include the 32-byte session but exclude the 24-byte header.

| Kind | Code | Direction | Body after session | Total body |
| --- | ---: | --- | --- | ---: |
| `Hello` | 1 | VMM → peer | limits | 56 |
| `HelloAck` | 2 | peer → VMM | selected limits | 56 |
| `Region` | 3 | VMM → peer | region ID, zero, source offset, length | 56 |
| `Start` | 4 | VMM → peer | none | 32 |
| `Ready` | 5 | peer → VMM | none | 32 |
| `PageRequest` | 6 | VMM → peer | page metadata | 72 |
| `PageData` | 7 | peer → VMM | echoed page metadata, exact bytes | 72 + page size |
| `PageZero` | 8 | peer → VMM | echoed page metadata | 72 |
| `Remove` | 9 | VMM → peer | removal metadata | 72 |
| `Removed` | 10 | peer → VMM | echoed removal metadata | 72 |
| `Cancel` | 11 | VMM → peer | reason byte, seven zero bytes | 40 |
| `Cancelled` | 12 | peer → VMM | none | 32 |
| `Terminal` | 13 | either | category, six zero bytes | 40 |
| `Shutdown` | 14 | VMM → peer | none | 32 |
| `ShutdownAck` | 15 | peer → VMM | none | 32 |

Limits are `page_size:u32`, `region_count:u16`,
`max_in_flight:u16`, `max_frame_bytes:u32`, `operations:u32`, and eight
reserved zero bytes.

A Region is `region_id:u32`, four reserved zero bytes,
`source_offset:u64`, and `length:u64`. Region IDs are nonzero and unique.
Source offsets and lengths are page-aligned, lengths are nonzero, ranges do not
overflow, and source ranges do not overlap. Regions carry no host or guest
virtual address.

Page metadata is `request_id:u64`, `region_id:u32`, `access:u32`,
`generation:u64`, `offset:u64`, `length:u32`, and four reserved zero bytes.
Access is 1 for read or 2 for write. All identities are nonzero. Offset is
region-relative, aligned, and in bounds; length is exactly the selected page
size. `PageData` contains exactly `length` bytes, while `PageZero` contains no
page bytes.

Removal metadata is `request_id:u64`, `region_id:u32`, four reserved zero
bytes, `generation:u64`, `offset:u64`, and `length:u64`. Its region-relative
range is nonempty, page-aligned, nonoverflowing, and in bounds.

Cancellation reason 1 is an explicit local request and reason 2 is a local
source/coordinator failure. Terminal categories are invalid frame (1), invalid
peer state (2), limit exceeded (3), and internal failure (4). V1 carries no
peer-authored string, UTF-8 field, path, host address, diagnostic, or retry
hint. Consequently malformed UTF-8 and peer diagnostic leakage are impossible
by construction.

## Handshake and region configuration

The VMM chooses a fresh session and sends exactly one `Hello`. The peer returns
one valid `HelloAck` selection. The VMM then sends exactly the negotiated
number of unique Regions, followed by `Start`. The peer accepts `Start` only
after that complete, valid region set and enters Active only when it emits
`Ready`.

Every later frame repeats the exact session identity. Cross-session frames,
duplicate Regions, overlapping sources, early or repeated handshake frames,
and wrong-role message kinds terminate state.

## Requests, generations, and ordering

The VMM assigns one sequence of nonzero, strictly increasing request IDs across
both page and removal work. IDs never wrap or reset within a session. It may
have no more than the selected combined number outstanding.

Responses may complete out of request order. Each response must repeat the
complete stored tuple: request ID, region ID, generation, offset, length, and
page access where applicable. A duplicate, replayed, unknown, or partially
mismatched response terminates state. The implementation removes a request
from its outstanding set only after the full tuple and data length validate.

Generation is an opaque nonzero coordinator value. The protocol compares it
exactly and does not infer ordering between generations.

There is no automatic request retry or replay. A later integration may begin a
new restore only with a new random session and freshly assigned request IDs.

## Runtime anonymous-memory coordinator

`crates/runtime/src/lazy_memory.rs` implements the in-process ownership half of
the contract without opening a transport. `LazyGuestMemory` is deliberately
distinct from ordinary initialized `GuestMemory`: it transactionally allocates
validated private-anonymous regions, begins every selected page logically
absent, and exposes no ordinary safe read, write, atomic, discard, shared
export, or snapshot-image API.

Construction validates the exact negotiated region count, page size, combined
in-flight limit, unique region IDs, ordered nonoverlapping guest ranges,
aligned nonoverlapping source ranges, checked page counts, a caller-selected
total-page bound, and a separate local waiter bound before publishing the
owner. One byte-sized tag per selected page records `Absent`, `Loading`,
`Publishing`, `Present`, or `Removing`; the owner-wide terminal phase overlays
every page. Active protocol operations and duplicate-fault completion records
use pre-reserved vectors bounded by negotiated and local limits. There is no
per-page mutex, generation object, channel, or waiter allocation.

The first absent fault returns one non-cloneable population ticket containing
the immutable region, generation, access, source offset, guest range, and
length tuple. Duplicate read or write faults join that generation and wait on
one condition variable. This coalesces page contents only: later Mach/HVF
bridges must re-evaluate each fault's permissions after wakeup. A response may
enter a scoped publication guard only for the exact current ticket. Its target
accepts exactly one full data page or zero page, and commit is the only
`Publishing` to `Present` transition.

Every issued population or removal occupies one negotiated in-flight slot.
When removal supersedes a loading page, the old population becomes a counted
retired operation until its exact stale response is consumed, its ticket is
dropped, or terminal teardown abandons the session. Removal reserves a
distinct slot before changing any page. Its scoped guard zeroes the complete
range and leaves it `Removing`; only explicit validated `Removed`
acknowledgement commit makes the range `Absent` and admits a newer refault
generation.

Requested cancellation, peer failure, abandoned current work, generation
exhaustion, synchronization poison, and teardown all close admission and wake
waiters with a stable value-redacted result. Explicit termination waits for
publication/removal actions that already crossed their linearization point;
destructors close admission without blocking and guards retain mapping
lifetime until their own cleanup. Those actions are non-reentrant: a thread
must not request overlapping removal or synchronized termination while it
retains the guard that such an operation must drain.

This coordinator installs no Mach exception port, changes no HVF mapping or
permission, reads no snapshot source, opens no peer, and changes no API
behavior. Its logical absence is not a delivered fault path until the later
host/HVF, peer, consumer, and native-v1 restore slices bind those integration
points.

## Cancellation, terminal failure, and shutdown

Cancellation is session-wide and terminal. `Cancel` abandons every outstanding
request; the peer abandons the corresponding work and emits `Cancelled`.
Ordinary responses racing after cancellation are invalid. V1 intentionally has
no per-request cancellation because response/cancel crossing would require a
second completion history and is not needed by the restore lifecycle.

Either role may send `Terminal` from any established live phase. It immediately
abandons work and ends the protocol. The category is stable and string-free;
it is not permission to log peer bytes or local values.

Orderly shutdown is drain-only. The VMM may send `Shutdown` only in Active with
no outstanding work, and the peer may acknowledge it only under the same
condition. `ShutdownAck` closes both sides.

## Stream, deadlines, EOF, and logging

`PagerTransport` sets the adopted stream nonblocking, suppresses `SIGPIPE`, and
sends or receives one whole frame under one absolute operation deadline.
Interrupts and partial transfers do not restart that deadline. Receive is
header-first, so an oversized body is rejected before allocation. Ancillary
data, including SCM_RIGHTS descriptors attached to otherwise valid bytes, is
accepted only into a bounded rejection buffer; every received descriptor is
closed immediately, no descriptor authority enters the protocol, and the
transport becomes terminal.

A timeout, malformed frame, local I/O failure, broken connection, clean EOF
between frames, or truncated EOF within a frame poisons that transport. No
later frame operation is allowed and no request is replayed. The containing
coordinator must take its one bounded restore-failure and cleanup path.

Error Display text is fixed. Error Debug hides local I/O kinds; session,
region, request, generation, offset, source, stream, and page contents use
value-redacted Debug output. Consumers must not add raw frame, payload, path,
address, or peer-provided value logging around this boundary.

## Implemented and deferred scope

The crate currently implements and tests:

- exact canonical encode/decode and bounded incremental decoding;
- VMM and peer role-specific state machines;
- request/generation/range validation and out-of-order exact matching;
- terminal cancellation and drained shutdown;
- already-connected absolute-deadline Unix transport; and
- real child-process exchanges over an inherited connected stream.

The runtime additionally implements and deterministically tests bounded
private-anonymous region ownership, duplicate-fault coalescing, exact
publication, retired-operation accounting, acknowledged removal, terminal
wakeup, poison recovery, resource limits, and repeated cleanup.

Still deferred are transport/coordinator wiring, socket brokerage, source
grants, host Mach exception mediation, HVF guest-fault mediation, peer-driven
removal/failure integration, consumer gating, native-v1 restore activation,
and signed end-to-end certification. Until those #1527 slices complete,
native-v1 `Uffd` remains rejected before resource access and the checked
capability remains `missing-platform-feasible`.
