# Snapshot Feasibility

This document records the implemented boundary and remaining roadmap for
Firecracker-shaped snapshot APIs on macOS with Hypervisor.framework. bangbang
supports a public bangbang-native Full/File lifecycle with a rooted or
rootless ordered regular-file block-and-pmem profile-3 graph. The current
writer is exact native-v2 2.7: it adds required complete serial state and makes
that storage graph optional, so serial-only and serial-plus-storage products
share one Full/File lifecycle. Exact storage-only native-v2 2.6, block-only
native-v2 2.5, single-root native-v2 2.4, device-free native-v2 2.3, and frozen
native-v1 File/Uffd remain compatibility readers. Broader Firecracker
snapshot-file and migration compatibility remains out of scope.

The immutable native-v2 `2.0.0` profile contains no semantic
component, `2.1.x` adds one state-bound demand-paged File/COW memory image, and
`2.2.x` adds a permanent typed multi-vCPU HVF platform graph. Legacy `2.3.0`
appends portable PL031, PVTime, VMGenID, and VMClock state and clone policies.
Exact `2.4.0` adds mandatory device-graph kind 7 with profile 1's one
read-only File/Sync root. Exact `2.5.0` retains kind 7 and activates profile 2
with 1–64 ordered regular-file block records. Exact `2.6.0` activates profile
3 with 1–64 ordered regular-file storage records spanning block and pmem over
MMIO or the product PCI endpoint budget. Current `2.7.0` adds mandatory serial
component kind 8 and makes kind 7 optional without changing profile 3. The
storage graph may be rootless or select the first cross-storage record as root.
Block records retain read-only/read-write, Sync/Async, Unsafe/Writeback,
partuuid, and limiter state; pmem records retain read-only/root, exact file and
mapped length, limiter/retry, queue, interrupt, mapping, and transport state.
Serial retains endpoint intent, limiter configuration, complete UART
registers, bounded RX bytes, status, and pending input/interrupt work. The
public process reconstructs the complete multi-vCPU platform, time/identity
ownership, fresh serial endpoint, and any storage owners through one newly
authorized complete-set transaction. Optional devices other than pmem and
serial remain excluded.
`LazyGuestMemory` remains the backend-neutral
private-anonymous
coordinator for the external-paging roadmap; it is not the v2 File/COW loader
and backs the frozen native-v1 `Uffd` compatibility path on macOS Apple
Silicon. `bangbang-hvf` binds that coordinator to task-local public-Mach host
faults, HVF stage-two guest read/write/execute faults, and one
`bangbang-pager-v1` external content owner. Native-v2 rejects `Uffd`; neither
path adds Linux UFFD wire compatibility.

## Current Status

bangbang implements two classified native state families, exact-version
inspection, bound guest-memory image I/O, and one macOS no-clobber two-file
publisher/loader. The current public writer captures an accepted paused
native-v2 2.7 source into a state/memory pair. A fresh process consumes that
pair through retained read-only private File/COW mappings and one exact
storage-plus-serial authority transaction, commits the complete destination
initially paused, and optionally resumes through the ordinary lifecycle path.
The public loader also recognizes storage-only native-v2 2.6, block-only
native-v2 2.5, singleton-root native-v2 2.4, device-free native-v2 2.3, and
frozen native-v1 state, routing the latter to the unchanged eager File or macOS
pager compatibility path.

The generalized destination foundation prepares one complete typed storage
resource request through a coherent direct or contained-session transaction.
The normal production bundle certifies exact block/pmem authority,
prepublication abort and reuse, deterministic cancellation, one-time typed
take/adopt/commit, launcher-opened identity after pathname replacement,
prepared and active cleanup, both independent process-death orders,
cross-class alias rejection, concurrent same-ID isolation, and redaction
without a new entitlement, ambient path, protocol, or steady helper. Exact 2.7
extends the same boundary to a configured `SerialSink` grant and fresh default
stdio; other optional-device overrides, clones, and broad portability remain
unchanged.

- `PUT /snapshot/create` and `PUT /snapshot/load` parse and normalize complete
  request bodies into debug-redacted API and runtime values before reaching VMM
action policy. Paths and override contents are never logged or echoed.
- Create is paused-state-only and supports only `Full` for a 1–32-vCPU source
  with a configured boot source, complete live serial state, and zero or 1–64
  ordered regular-file block/pmem devices. No optional device other than pmem
  and serial, MMDS, boot timer, or vhost-user is admitted. The storage graph
  may be rootless or have one first cross-storage root. Block records may mix
  access, engine, cache, partuuid, and limiter state; pmem records retain
  access, limiter, exact file/mapped geometry, and mapping state. Every storage
  record uses the process-selected MMIO or PCI transport; PCI remains bounded
  by the 31-endpoint product budget. Unsupported modes fail before storage or
  serial resource adoption.
  Unsupported broad storage profiles run the live non-persisting preflight
  described below, then fail before contained grant claims, artifact staging,
  or native state capture.
- The profile-3 device graph persists each ordered block or pmem
  configuration, runtime, limiter/retry state, queue continuation, common
  virtio state, and transport state. MMIO carries each fixed placement,
  register, queue, and interrupt state; PCI carries each endpoint identity,
  BAR and capability state, queue selection, MSI-X table/PBA/vector state, and
  shared registry placement needed to recreate the complete owner vector. Pmem
  additionally binds exact file length, aligned mapped length, and guest
  range/configuration-space state. This activation does not generalize PCI
  persistence to balloon, network, vsock, entropy, virtio-mem, vhost-user, or
  host-adapter state.
- Before applying those native-v2 profile exclusions, paused create asks
  the boot owner for one complete capture-ready storage traversal. It reconciles
  every configured startup or runtime block/pmem device with its authoritative
  live MMIO or PCI owner, rejects any live vhost-user block backend first, and
  returns a redacted in-memory state aggregate. Async generations are stopped
  together, drained together, and have every completion plus its MMIO SPI or PCI
  MSI-X interrupt published before state capture and same-generation reopen.
  An admitted regular-file vector feeds the profile-3 graph shared by exact
  2.6 and current 2.7.
  Vhost-user and any broader optional-device inventory remain followed by profile
  rejection, so they publish no artifact bytes and create no load contract.
- An admitted create holds one scoped supervisor transaction from FIFO
  admission through publication. It failure-atomically quiesces block, PMEM,
  network, and entropy retry schedulers, preflights both final namespaces,
  captures aggregate state, streams complete memory, verifies and synchronizes
  the artifacts, commits memory first and state last, and invokes the explicit
  successful-publication hook before releasing auxiliary ownership and command
  admission. Success returns `204 No Content` and leaves the source paused and
  usable.
- Load is pre-boot-only and requires a pristine process except logger/metrics.
  Successful-action history catches explicit-default/no-op configuration, and
  a live view catches residual state such as MMDS left by a failed patch.
  State is opened and decoded once before family routing. Native-v2 accepts
  only `File`, retains the read-only memory descriptor, and maps guest memory
  privately so destination writes cannot modify the pair. Frozen native-v1
  keeps its eager `File`, deprecated sole `mem_file_path`, and macOS
  Apple-Silicon `Uffd` behavior; v1 `Uffd` requires dirty tracking disabled and
  selects a `bangbang-pager-v1` Unix peer rather than a memory file or Linux
  UFFD transport.
- A valid current native-v2 load performs full bounded state, memory, FDT,
  UART, HVF, topology, time, identity, optional device-graph, transport, and
  complete storage-plus-serial resource validation before constructing a fresh
  VM. Every selector stays inert until exact set, order, identity, role,
  access, regular-file kind, size, and geometry are validated together.
  Frozen native-v1 retains its exact bundle/root/device validation. Both
  families always commit a real process session as `Paused` first.
  `resume_vm: true` reuses ordinary resume and returns only after `Running`;
  false leaves the destination paused.
- Retryable pre-construction failures keep the fresh process eligible for a
  corrected request. Failures after an uncertain construction/cleanup boundary
  latch the process terminal. Create/load execution faults are typed and
  snapshot-specific while diagnostics remain path- and value-redacted.
- `Diff`, native-v2 `Uffd`, dirty-tracked or bypassing-consumer native-v1
  `Uffd` profiles, realtime
  adjustment, overrides, unsupported device profiles, and incompatible
  artifacts retain snapshot-specific rejection boundaries. The checked
  [snapshot paging contract](../compat/firecracker/v1.16.0/snapshot-paging-contract.md)
  records the public macOS equivalent, its direct and contained restore
  assembly, and #1555's completed signed demand/removal/entitlement
  certification.
  Full/File load can enable a clean destination dirty epoch, independently of
  the source, and a tracked source resets only after visible Full publication.
  Parser and invalid-lifecycle failures still do not record snapshot latency;
  admitted success, capability rejection, and execution failure do.
- The checked
  [vsock closure ledger](../compat/firecracker/v1.16.0/vsock-contract.md)
  certifies all eight API/live records and records the complete source-side
  reset/capture plus direct/contained destination-resource producer. Exactly six
  rows remain with #1490 for optional-device encoding/placement, public load
  invocation, restored acknowledgement/reconnect/override proof,
  clone/versioning, and portability. The signed source resume is deliberately
  not described as artifact restore.
- `--snapshot-version` prints the current writer version `v2.7.0`.
  `--describe-snapshot <PATH>` opens a bounded regular file with the same
  nonblocking, path-redacted startup-file policy, classifies and fully
  validates either native-v1 or native-v2, and prints its exact embedded
  version. Pinned Firecracker prefixes and unknown bytes remain explicitly
  incompatible. In contained mode an exact
  `SnapshotDescribeInput`/`ReadOnly` grant supplies that already-opened file;
  direct mode keeps pathname opening. Both commands exit before fd-table
  setup, API socket publication, signal setup, or HVF startup.
- Contained create accepts
  `bangbang-grant:<GrantId>/<SnapshotOutputChild>` for either output. The child
  is one 1–255 byte UTF-8 component with no NUL or `/` and is not `.` or `..`.
  A repeatable `SnapshotOutputDirectory` grant may be shared by distinct
  children, paired with a second grant, mixed with one ordinary destination,
  and retained for later create attempts. Staging and final publication remain
  relative to the granted anchors and never reopen their resolved paths.
- Contained load accepts exact `bangbang-grant:<GrantId>` state and memory
  selectors. It duplicates state for one bounded family decode without
  consuming the registry, then atomically takes the tagged
  `SnapshotStateInput` and `SnapshotMemoryInput` files. Current native-v2
  additionally derives any ordered block/pmem requests plus a configured
  serial-sink request and resolves the entire keyed vector through one exact
  destination authority transaction. Default serial stdio is resource-free
  and is created only from destination descriptors.
  Missing, extra, reordered, aliased, wrong-access, wrong-role/kind,
  wrong-size, changed-geometry, and consumed resources reject before
  construction. The complete batch remains provisional until
  session/controller publication. Exact 2.4 uses the same boundary for its
  singleton root, legacy device-free native-v2 needs no root, and native-v1
  additionally discovers and adopts any persisted grant-tagged read-only root
  backing.
  Direct and mixed ordinary members keep pathname adapters; no reserved
  reference falls back to ambient opening.
- The runtime can encode a bounded state-embeddable GPA manifest, stream a full
  memory image from exact `GuestMemory` regions, and load a validated image into
  a selected newly allocated anonymous or descriptor-backed shared profile
  through already-open seekable handles. Native-v1 File restore retains the
  anonymous eager profile; native-v2 retains private File/COW mappings. A
  separate path layer can publish that image with either validated commit kind
  and load the committed pair. The public process transaction supplies the
  publisher-owned staging writer to complete capture and requires a composite
  commit; the public load transaction consumes only that committed kind-2 pair.
- Signed Apple Silicon executable coverage retains the exact 2.5 block and
  exact 2.6 storage regressions, adds rooted pmem-only/rootless mixed ×
  MMIO/PCI profile-3 certification, and certifies exact 2.7 serial-only plus
  storage-bearing UART continuation. A deterministic initrd validates seeded block/pmem content,
  forces limiter retry, persists writable pre-capture epochs, rejects
  read-only writes, observes the restored graph Paused, recaptures, resumes,
  and advances the writable epochs. Fresh destinations load the same immutable
  state/memory pair, observe prior shared external prefixes, and verify a zero
  private pmem tail before advancing again.
- Signed production-bundle coverage repeats that four-cell storage protocol
  and the serial-only/configured-output exact 2.7 protocol with exact
  external kernel/initrd/metrics/block/pmem/output grants, pathname
  replacement, granted early description, explicit and automatic resume,
  retained descriptor identity, collision preservation, and rootless-MMIO
  worker-first and launcher-first cleanup after Paused publication.
  State/memory inputs and their File/COW guest mappings remain
  immutable/private. Writable external block and pmem prefixes deliberately
  are shared rather than COW-isolated and require operator serialization; pmem
  private tails are volatile. A separate signed frozen native-v1 File fixture
  reaches the same public family dispatcher.

## Native V1 State Envelope

The implemented outer envelope is bangbang-owned and deliberately does not
claim Firecracker bitcode or on-disk compatibility. All numeric fields are
little-endian. The fixed header is 32 bytes, followed by one opaque payload and
an 8-byte integrity trailer:

| Offset | Width | Field | Native-v1 rule |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | ASCII `BANGSNAP` |
| 8 | 2 | version major | `1` |
| 10 | 2 | version minor | `0` |
| 12 | 2 | version patch | `0` |
| 14 | 2 | architecture | `1` means arm64 |
| 16 | 4 | guest page size | `4096` bytes |
| 20 | 4 | reserved flags | must be zero |
| 24 | 8 | payload length | exact opaque byte count |
| 32 | variable | payload | at most 16 MiB |
| final 8 | 8 | CRC64 | CRC-64/Jones over header and payload |

The current decoder accepts only exact version `1.0.0`, arm64, a 4096-byte
guest-memory granule, zero reserved flags, and an exact total file length. It
checks conversion and length arithmetic, the 16 MiB payload policy, truncation
or trailing bytes, and CRC before publishing metadata or a borrowed payload.
Unknown versions and incompatible architecture/page-size values fail through
distinct typed errors. Diagnostics expose only stable metadata and byte counts;
payload bytes and host paths remain redacted.

CRC-64/Jones detects accidental corruption. It does not authenticate a
snapshot: an actor able to rewrite the state file can also recompute the CRC,
so every future payload decoder must remain safe for attacker-controlled input.
The inspection CLI still treats the payload as opaque. The runtime additionally
recognizes both commit kinds below, while the HVF crate alone validates the
backend-specific composite payload.

## Native V2 Structural State Foundation

Native v2 is a bangbang-owned, arm64-specific state container designed for
typed component codecs without changing any native-v1 bytes or production
path. All numeric fields are little-endian. The fixed header is 64 bytes:

| Offset | Width | Field | Native-v2 rule |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | bytes `BANGV2A\0` |
| 8 | 2 | version major | `2` |
| 10 | 2 | version minor | compatibility-bearing minor |
| 12 | 2 | version patch | nonsemantic patch; canonical writer emits `0` |
| 14 | 2 | header bytes | exact `64` |
| 16 | 4 | flags | must be zero |
| 20 | 4 | required-feature count | at most `256` |
| 24 | 4 | component count | at most `4096` |
| 28 | 4 | reserved | must be zero |
| 32 | 8 | total length | exact complete state-file length, including CRC |
| 40 | 8 | required-feature offset | exact `64` |
| 48 | 8 | component-directory offset | exact end of the feature table |
| 56 | 8 | component-payload offset | exact end of the directory |

The header is followed by zero or more sorted, unique, nonzero `u32`
required-feature identifiers. Each component-directory entry is exactly 32
bytes:

| Entry offset | Width | Field | Native-v2 rule |
| ---: | ---: | --- | --- |
| 0 | 4 | kind | nonzero component kind identifier |
| 4 | 4 | instance | instance identifier within the kind |
| 8 | 4 | flags | `0` semantic; exact bit 0 means nonsemantic |
| 12 | 4 | reserved | must be zero |
| 16 | 8 | payload offset | exact next packed payload offset |
| 24 | 8 | payload length | nonzero |

Component keys `(kind, instance)` are strictly increasing. Payloads follow in
directory order and are contiguous: gaps, overlap, padding, wraparound, and
trailing bytes are invalid. The final eight bytes are CRC-64/Jones over every
preceding state-container byte. The current complete-file policy is 16 MiB;
raising that policy requires an explicit compatibility review and minor-version
decision. The CRC detects accidental corruption but authenticates neither this
state nor a separately stored guest-memory image.

Compatibility is fail-closed. A reader requires major `2` and rejects a newer
minor. For an older or equal minor it requires every mandatory feature and
semantic component kind to exist in the reader catalog at that minor.
Explicitly nonsemantic components may remain unknown after their complete
directory and payload ranges validate. Patch changes do not alter semantics.
The immutable production `2.0.0` catalogs are empty: its canonical fixture is
the 64-byte header plus its eight-byte CRC, nonsemantic extensions can be
structurally represented, and every required feature or semantic component
rejects. Minor 1 adds semantic memory kind `1`, whose typed profile requires
its sole instance to be `0`. Minor 2 adds semantic machine kind `2`, global
kind `3`, topology kind `4`, and per-vCPU kind `5`. The current writer emits
`2.5.0`; minor 3 adds singleton time/clone-identity kind `6`, minor 4 adds
mandatory singleton device-graph kind `7` with profile 1, and minor 5 retains
kind 7 while activating profile 2's bounded ordered block vector. The
required-feature catalog remains empty because those semantic component kinds
are the mandatory compatibility identities. Decoded `2.1.x`, `2.2.x`,
`2.3.x`, and exact `2.4.0` memory bindings retain their exact admitted version
so their unchanged paired image headers still validate; newly written bindings
use `2.5.0`. No other identifier or future minor is reserved.

Decoding first checks the fixed header, version, count caps, checked length and
offset arithmetic, exact length, whole-state CRC, complete feature inventory,
and complete component directory. This pass borrows the bounded input and
performs no count-proportional vector, string, map, or payload allocation.
Only after success do bounded borrowed iterators and lookups expose validated
components to trusted future typed codecs. Diagnostics can report stable
version, architecture, count, and limit information, but redact feature and
component identifiers and payload bytes.

The production encoder enforces the current catalogs, computes every size with
checked arithmetic, performs one fallible exact output reservation, and emits
only canonical bytes. Its generic catalog-aware encoder remains private and is
used only for grammar tests. The library family dispatcher delegates exact
`BANGSNAP` input to the unchanged v1 decoder and `BANGV2A\0` input to this v2
decoder. It recognizes only the arm64/x86_64 bitcode family prefixes derived
from Firecracker v1.16.0 at pinned commit
`d83d72b710361a10294480131377b1b00b163af8` to return a named incompatible-format
result; this is not bitcode decoding, validity proof, translation, or
Firecracker artifact compatibility.

### Native V2 Lazy File Memory Profile

The `(kind 1, instance 0)` semantic component payload is a fixed 64-byte
binding header followed by one 24-byte entry per ordered guest extent:

| Offset | Width | Field | Native-v2 memory rule |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | bytes `BANGM2A\0` |
| 8..14 | 6 | semantic version | admitted `2.1.x`, `2.2.x`, `2.3.x`, exact `2.4.0`, exact `2.5.0`, exact `2.6.0`, or exact `2.7.0`; current writer emits `2.7.0` |
| 14 | 2 | header bytes | exact `64` |
| 16 | 4 | flags | must be zero |
| 20 | 4 | guest granule | exact `4096` bytes |
| 24 | 4 | file alignment | exact `65536` bytes |
| 28 | 4 | extent count | `1..=4096` |
| 32 | 16 | image ID | opaque OS-random state/image pair identity |
| 48 | 8 | metadata CRC64 | CRC-64/Jones with this field zeroed over the header and complete extent table |
| 56 | 8 | file length | exact final extent end; no trailer |

Each extent contains little-endian `u64` GPA start, byte length, and absolute
file offset. GPA values and lengths are nonzero and 4-KiB aligned, ranges are
strictly increasing and nonoverlapping, total guest data is bounded by the
existing 1,022-GiB arm64 policy, and every arithmetic operation is checked.
The first extent starts at file offset 64 KiB. Every later extent starts at the
64-KiB alignment of the preceding extent end, and the file ends exactly at the
last extent end. The complete binding, including the topology and pair ID, is
nested in the state container and protected by both metadata and outer-state
CRCs.

The memory image repeats the exact 64-byte binding header, requires zero bytes
through offset 64 KiB, and stores guest bytes only in the bound extents.
Inter-extent alignment gaps are sparse canonical zeroes outside guest memory.
The bounded writer accepts only an empty, position-zero `Write + Seek` target,
writes the header/padding, streams each guest extent in at most 1-MiB chunks,
and verifies the exact final position and length. A cancellation-aware sibling
checks the fixed preflight, header, padding, every extent/chunk, and final-length
stages and never returns a binding after cancellation.

The loader extracts and validates the typed state component before path or
descriptor access. Its direct adapter opens the final component once with
read-only, close-on-exec, nonblocking, and no-follow flags; its contained
adapter adopts one already-opened `File`. Both require a read-only close-on-exec
regular descriptor, exact length, stable device/inode/mode/timestamps/flags,
an exact state-bound header, and zero fixed padding. Only the 64-KiB metadata
area is read before mapping. Every GPA, length, file offset, host-page
alignment, integer conversion, and file bound is preflighted before the first
`mmap`, then each extent becomes `PROT_READ|PROT_WRITE`, `MAP_PRIVATE`,
no-reserve guest memory retaining the same descriptor owner. The descriptor is
rechecked after metadata validation and after mapping; any change drops all
partial mappings. A successful load never consults the path again.

Private-file regions participate in ordinary byte access, HVF mapping, and the
existing dirty epoch. Initial demand faults are clean; explicit VMM/device or
guest writes become dirty COW pages and never write the source. Discard replaces
an exact aligned private subrange with anonymous zero pages instead of punching
or mutating the file. Anonymous dynamic regions and explicit shared reservations
can coexist, but shared export exposes only actual shared mappings; vhost-user's
whole-memory Shared requirement is unchanged. Every region retains and releases
its own owner, including partial construction, dynamic removal, ordinary drop,
and post-VM-destroy cleanup.

This format deliberately has no guest-byte CRC, authenticated digest, trailer,
or encryption. The metadata CRCs detect accidental binding corruption only.
A deployment must authenticate and, when confidentiality is required, encrypt
the complete state and memory artifacts outside this codec. After validation,
the retained inode must remain immutable for the mapping lifetime; macOS offers
no seal for an arbitrary external regular file, so concurrent external mutation
or truncation violates the loader contract. The VM-free artifact layer now
publishes current native-v2 state/memory through the same anchored, no-clobber
memory-first/state-last transaction as native-v1 and prepares or loads
compatible native-v1/v2 pairs from direct paths or already-opened descriptors.
The v2 commitment derives its binding only from its retained state bytes;
publication verifies a transaction-owned read-write staging inode without
mapping it, while final loading independently requires read-only, close-on-exec
File/COW ownership. The public process composition described below constructs,
publishes, and restores current v2 through admitted minimal production
sessions. Additional device state, Diff/merge, native-v2 Uffd, editing tools, and
broader cross-host portability policy remain follow-on work.
The focused HVF reconstruction boundary below consumes an
already-authorized `GuestMemory`; it does not open this memory artifact itself.
Its implemented
clone policy covers fresh identity and portable time semantics on the validated
destination profile, not public artifact publication or arbitrary-host
migration. The public feasibility decision is recorded separately in the checked
[snapshot paging contract](../compat/firecracker/v1.16.0/snapshot-paging-contract.md);
File/COW remains a distinct backend.

### Paused Native V2 Process Publication

#1576 added the typed `ProcessVmm`/starter/session command for
publishing one then-current native-v2 2.3 pair. Admission requires a `Paused` `Full`
source with retained boot metadata, represented machine/CPU facts, and no
drive, pmem, network, vsock, balloon, memory-hotplug, entropy, MMDS, PCI, or
boot-timer state. Serial configuration must be default, and the live complete
UART capture must equal a fresh validated `SerialMmioDevice::discarding()`
capture. That comparison includes guest registers, status, FIFO contents,
interrupt intent, and input-ready intent and happens under the same auxiliary
guard before artifact staging.

One supervisor command then completes the topology pause, streams memory into
the pathless native-family staging writer with bounded cancellation, encodes
the exact captured platform graph, closes the state-derived v2 commitment, and
recovers the inner vCPU coordinator while the outer process remains paused.
Only a recovered source may seal the commit. Pre-seal cancellation or capture
failure cleans staging and leaves the source reusable when recovery succeeds;
an ambiguous topology pause, recovery failure, or caught capture panic is
terminal. A sealed pair retains the artifact authority's exact durability even
if the dirty-epoch post-publication hook terminates the worker. Direct and
already-opened contained outputs continue to use #1575's identity, tracker,
no-clobber, cleanup, and memory-first/state-last transaction.

Boot kernel/initrd paths and arguments are copied into bounded inert metadata
from the admitted controller only; capture does not reopen them. #1578 routes
public `VmmAction::CreateSnapshot` and HTTP `PUT /snapshot/create` to this
producer for admitted Full requests. Native-v1 bytes remain unchanged, but
there is no public v1 writer selector or profile fallback.

### Paused Native V2 Process Restore

#1577 added the typed `ProcessVmm`/starter/session path that
restores a prepared native-v2 2.3 pair into the normal process lifecycle.
Admission is state-first and accepts only a pristine destination, File/COW
memory, exact direct or already-opened contained descriptors, and the minimal
profile published by #1576. Uffd memory, overrides, drives and other optional
devices, PCI, and boot-timer state fail before process construction. Recorded
kernel and initrd paths remain inert metadata and are never reopened; logger
and metrics ownership comes from the destination process.

Before HVF construction, the process adapter validates the exact default arm64
FDT shell: adjusted RAM ranges, CPUs, GIC, timer, fixed clock, PSCI, PL031 RTC,
one UART, VMGenID, and VMClock with the captured interrupt identities. Optional,
missing, duplicate, or drifted nodes fail closed. It then installs a fresh UART
with either a new destination buffer or output-only stdout; restored processes
never inherit source serial bytes or open stdin. The focused platform retains
only RTC/time/identity and that fresh UART rather than reconstructing a general
device aggregate.

A closed supervisor-session variant owns the boot or native-v2 platform
without exposing raw run control. The imported coordinator is run-ready before
publication, but the outer worker and controller are committed `Paused`.
Session assignment precedes the infallible controller commit; a requested
resume is then performed through the ordinary process action gate. The
restored controller retains the decoded machine facts, inert boot metadata,
empty drives, default serial configuration, and destination-owned observability
state.

Failures before resource adoption and unpublished cleanup remain retryable.
Failures after an HVF owner is adopted, controller/session commit begins, or
cleanup becomes ambiguous terminate the worker. The same terminal latch is
shared by ordinary boot, native-v1 restore, and native-v2 restore, so no later
construction path can replace a possibly live VM. A signed two-vCPU proof
publishes and recaptures one source, destroys it, restores the same immutable
pair directly and through retained contained descriptors after path
replacement into two fresh processes, checks the initial paused boundary,
exercises explicit and requested resume through ordinary lifecycle actions,
and shuts both destinations down cleanly.

#1578 exposes this restorer behind one public prepared-family dispatcher. It
opens and decodes state once, routes native-v1 to the frozen File/Uffd loader,
routes native-v2 to this File/COW path, and rejects pinned Firecracker or
unknown input without fallback. Public description accepts either native
family; at that slice the version command named `v2.3.0`. General
optional-device state, serial input and endpoint restoration, native-v2 Uffd,
Diff, editing, and broader portability remain later Wave 6 work.

### Native V2 2.4 Root Device Activation

#1589 advanced the then-current writer to exact `2.4.0` and made semantic component
kind `7` mandatory. Public create now replaces the device-free producer profile
with exactly one root block device: a read-only regular File backing, Sync I/O
engine, no optional devices or MMDS, canonical default serial, and no boot
timer. The root uses the process-selected MMIO or PCI transport. Capture
cross-validates the configured root against its live owner and closes the
existing memory-first/state-last artifact transaction only from an exact
graph-bearing candidate.
The ordinary process-standard-stream binding is a host-local endpoint, not
serialized device state: it is admitted with that canonical serial/UART
profile and rebound independently by each destination.

The device graph contains one block record and four closed sections:
configuration/backing selector, block runtime, common virtio state, and exact
MMIO or modern PCI transport state. The selector is inert artifact data.
Destination preparation first validates the complete 2.4 state/memory/graph
cross-binding. It also verifies the retained live-FDT address, length, and CRC,
but does not parse those guest-owned post-boot bytes: Linux may already have
consumed or reclaimed them. Exact 2.4 instead requires a versioned source
product-profile word and derives the canonical root shell from typed machine,
transport, interrupt, and device facts. Preparation then resolves the selector
through direct pathname policy or an exact destination `DriveBacking` grant.
The backing lease stays provisional
while the root and platform owners are constructed and the supervisor becomes
run-ready; it commits only after the session and controller are published
`Paused`. Any failure beforehand drops the unpublished owner and lease
together. This preserves one public artifact transaction and one family
dispatcher rather than introducing a second snapshot pipeline.

Exact `2.3.0` remains the legacy device-free platform identity and is never
reinterpreted as a 2.4 device profile. Structural and memory compatibility for
`2.0.x`–`2.2.x`, legacy 2.3 loading, frozen native-v1 loading, File/COW
semantics, optional ordinary resume, collision/cancellation behavior, and
Firecracker/unknown rejection remain unchanged. Exact 2.4 profile 1 continues
to reject extra drives, writable or non-Sync roots, and every broader device
shape; it is not reinterpreted as profile 2 or current profile 3.

### Native V2 2.5 Multi-Block Compatibility

#1616 advanced the then-current writer to exact `2.5.0`; #1617 certifies its
public direct and production boundaries. Kind `7` remains mandatory, but its
payload selects profile 2 and carries an ordered directory of 1–64 regular-file
block records. Each record owns four closed sections: stable public
configuration plus inert selector, exact block runtime and limiter/retry state,
common virtio features/queue/interrupt continuation, and one exact MMIO or
modern PCI transport. The graph has no root or selects only its first record as
root. Record identity and order are part of the compatibility contract.

Create reconciles every configured record with its live owner after all
Async generations are stopped and drained together. Completed writes and
flushes, used-ring cursors, interrupt intent, relative limiter state, and
transport ownership enter one candidate; vhost-user rejects before staging
because neither bangbang nor pinned Firecracker snapshots that external
backend. MMIO admits up to the 64-record format bound. PCI additionally applies
the existing 31-endpoint product budget and exact slot/BAR/MSI-X capacity.

Load derives the complete ordered request from decoded state. Direct mode
opens each ordinary selector under no-follow regular-file policy. Contained
mode atomically prepares the exact keyed vector; missing, extra, reordered,
aliased, wrong-access, wrong-role/kind, wrong-size, changed-geometry, consumed,
or canceled authority rejects before VM construction. There is no ambient
pathname fallback and no load-time per-drive selector, configuration, or
transport override. Every prepared lease and fresh destination Async
generation remains provisional through platform, device, scheduler,
controller, and Paused session construction and commits as one batch.

State and memory inputs remain immutable. Each fresh destination maps the
memory image File/COW, so guest-memory writes are private. Drive bytes remain
in externally managed regular files exactly as in Firecracker: writable
destinations deliberately share those files, are not COW-isolated, and require
operator serialization. The signed rooted/rootless × MMIO/PCI direct and
normal-bundle matrices exercise all three mixed records, pre-capture
persistence, post-load read/write/flush, limiter progress, queue interrupts,
Paused observation, recapture, explicit/automatic resume, fresh VM/metrics/
Async ownership, retained descriptor identity, and worker/launcher death
cleanup. Pmem support is added only by current profile 3; exact 2.5 continues
to reject pmem, network, balloon, virtio-mem, entropy, vsock, MMDS,
noncanonical serial, native-v2 Uffd, Diff, overrides/editing, Firecracker
bytes, and broad migration portability.

### Native V2 2.6 Block-and-Pmem Activation

#1634 certified exact `2.6.0` profile 3, now retained as a compatibility
profile. Kind `7` carries an
ordered directory of 1–64 regular-file block and pmem records, with at least
one record and at most one first cross-storage root. Block sections retain the
profile-2 configuration/runtime/common-virtio/transport model. Pmem sections
bind exact ID, access/root/limiter configuration; file and aligned mapped
length; queue, feature, interrupt, limiter/retry, and flush continuation; and
one exact MMIO mapping or modern PCI endpoint.

Load derives one complete typed block/pmem request. Direct mode opens exact
ordinary files; contained mode atomically resolves exact `DriveBacking` and
`PmemBacking` grants. Missing, extra, swapped, cross-class aliased,
wrong-role/kind/access/length, changed geometry, consumed authority, and
cancellation reject before construction or roll back the complete batch.
Construction, controller, completion, worker-death, and launcher-death faults
leave no partial published destination or leaked authority.

The signed direct and normal-production matrices run rooted pmem-only and
rootless mixed block/pmem cells over MMIO and PCI. Writable pmem file prefixes
are deliberately shared and advance across fresh destinations; read-only peers
remain unchanged. Each backing length is 2 MiB plus one 16-KiB Apple-Silicon
host page, while the 2-MiB-aligned mapping tail remains private and reads zero
through guest DAX on every fresh mapping. State and memory artifacts remain
immutable, destination File/COW RAM stays private, recapture is graph-stable,
and limiter/queue/interrupt ownership continues after explicit or automatic
resume. This is a bangbang-native same-backing contract, not Firecracker
artifact interoperability or broad migration portability.

### Native V2 2.7 Serial Activation

#1651 activated the exact `2.7.0` writer/reader and #1652 certifies its public
continuation and containment boundary. Kind `8` is mandatory and contains one
bounded `BANGSR2\0` profile-1 value. Kind `7` is optional: absent means
serial-only, while present retains the exact profile-3 block/pmem graph from
2.6 over MMIO or PCI.

The serial value contains default-process-stdio or configured-output endpoint
intent, optional rate-limiter configuration, divisor latches, interrupt
enable/identification, line control/status, modem control/status, scratch,
0–64 ordered receive bytes, receive-interrupt intent, and input-ready intent.
Impossible FIFO/status/interrupt combinations, invalid or oversized selectors,
noncanonical flags, incompatible versions, trailing bytes, and over-limit
allocations fail before construction.

Default restore creates fresh destination stdout and attaches only supported
destination terminal/FIFO/pipe stdin. Configured restore opens a fresh direct
file/FIFO or consumes one exact contained `SerialSink`/`WriteOnly` grant,
output-only. Any storage and configured serial resources form one
failure-atomic complete-set transaction. No source descriptor, terminal
attributes, host pipe/FIFO bytes, TX bytes, metrics, limiter clock/budget,
wakeup handle, or absolute deadline is serialized.

The restored UART is built before first execution and retains buffered RX,
register/status state, input-ready rearm, and retryable interrupt work. Paused
load can be recaptured before resume. The signed bare-arm64 protocol programs a
nondefault valid UART, retains a full 64-byte source prefix, leaves a distinct
40-byte suffix in the terminated source pipe, and accepts only a fresh
destination-provided suffix after restore. Direct serial-only/MMIO/PCI cases,
configured regular-file/FIFO replacement, and normal production/App Sandbox
default/configured grant cases prove explicit/automatic resume, TX, EOF,
destination-only metrics, immutable pair reuse, redaction, source-endpoint
exclusion, and teardown.

### Native V2 HVF Platform State Profile

Minor 2 defines the base editor-friendly platform graph, minor 3 appends its
portable time/clone-identity component, minor 4 appends device-graph profile 1,
minor 5 retains the same component key with profile 2, and minor 6 selects
profile 3. Minor 7 adds mandatory serial kind 8 and makes kind 7 optional.
Every directory entry is semantic, singleton kinds appear exactly once, and
per-vCPU instances are contiguous:

| Key | Payload | Cardinality |
| --- | --- | --- |
| `(1, 0)` | version-retaining memory binding above | exactly one |
| `(2, 0)` | `BANGMC2\0` machine, inert boot/FDT, and CPU-application evidence | exactly one |
| `(3, 0)` | `BANGGL2\0` common compatibility and one opaque VM-global GIC value | exactly one |
| `(4, 0)` | `BANGTP2\0` stable topology and PSCI lifecycle state | exactly one |
| `(5, i)` | `BANGVC2\0` complete state for vCPU index/MPIDR `i` | `i = 0..vcpu_count-1` |
| `(6, 0)` | `BANGTM2\0` portable PL031/PVTime/VMGenID/VMClock state and policies | exactly one, after all vCPUs |
| `(7, 0)` | `BANGD2A\0` exact profile-1 singleton root, profile-2 ordered block graph, or profile-3 ordered block-and-pmem graph | exactly one in 2.4/2.5/2.6; optional profile 3 in 2.7, after time state |
| `(8, 0)` | `BANGSR2\0` exact endpoint intent, limiter configuration, complete UART state, RX FIFO, and pending work | exactly one in 2.7, after optional storage |

The legacy platform scan requires 6–37 entries in that exact key order; the
exact 2.4–2.6 graph-bearing profiles require 7–38, and exact 2.7 requires
7–39 depending on optional storage. All validate before payload-dependent
allocation. Kinds 2–6 require profile 1; kind 7 requires profile 1 at exact
2.4, profile 2 at exact 2.5, or profile 3 at exact 2.6/2.7; kind 8 requires
profile 1 at exact 2.7. Every payload requires its exact fixed header size,
zero flags/reserved fields, exact checked lengths, and complete consumption.

Kind 2 stores the checked machine configuration, bounded native kernel/initrd
path bytes, optional UTF-8 boot arguments, deterministic live-FDT
placement/size, a redacted checksum identity, and one version-defined FDT
profile word. Exact 2.3 requires the legacy zero value; exact 2.4 through 2.7
require the product value proving that VMM-owned source admission used the
versioned process shell. Paths are inert metadata: construction and decode
neither resolve nor open them. A custom CPU-template receipt contains at most 256
strictly tag-ordered entries. Each entry records a closed register tag, exact
U32/U64/U128 width, logical filter/value, topology-wide common baseline, and
the effective value already verified on every vCPU. The decoder rechecks
width, canonical masked value, and
`effective = (baseline & !filter) | logical_value`.

Kind 3 directly stores common MIDR/MPIDR and reviewed identification registers,
optional ZFR0/SMFR0 evidence, the cache manifest, primary MPIDR, GIC
distributor/redistributor/SPI/timer/MSI metadata, RTC layout, and one nonempty
opaque GIC byte string capped at 12 MiB. It deliberately does not reuse the
native-v1 compatibility bytes: the native-v1 inactive-optional and fresh-RTC
policy markers would contradict minor-2 active optional state.

Kind 4 stores `1..=32` canonical members and preserves offline, runnable, or
deferred PSCI `CPU_SUSPEND32/64` state. Kind 5 associates the same canonical
index/MPIDR with the explicit policy-free native-v1 mandatory register field
group, normalized timer state, pending IRQ/FIQ, vCPU-affine GIC ICC registers,
and a reviewed optional registry. Reusing that mandatory field group changes no
native-v1 outer byte or policy.

Kind 6 has a fixed 240-byte `BANGTM2\0` header followed by one fixed 24-byte
record per topology-ordered vCPU. Its four closed policies require a fresh
destination-SystemTime PL031 reset, preserved cumulative PVTime excluding
snapshot downtime, regenerated-and-notified VMGenID, and
incremented-and-notified VMClock. The header retains PL031 placement,
VMGenID/VMClock guest ranges, FDT ranges and distinct SPI lines, plus the exact
112-byte VMClock ABI. Each vCPU record carries its canonical index, standard
aligned record IPA, and cumulative stolen nanoseconds. It deliberately
serializes no source VMGenID bytes, PL031 mutable register state, host pointer,
`Instant`, or absolute wall-clock anchor.

The optional registry has a 64-byte `BANGOP2\0` header followed by at most 118
records and 96 KiB. Each record has a `u16` tag, explicit-or-destination-default
disposition, zero reserved bytes, exact `u32` architectural width, and payload
only for explicit values. The closed sorted inventory is breakpoint values
`1..=16`, breakpoint controls `17..=32`, watchpoint values `33..=48`,
watchpoint controls `49..=64`, SME PSTATE `65`, SME system registers `66..=68`,
Z0–Z31 `100..=131`, P0–P15 `132..=147`, ZA `148`, and SME2 ZT0 `149`.
Implemented debug counts and active SME PSTATE determine the exact required
subset. Unknown, missing, duplicate, unsorted, wrong-width, oversized, or
feature-inapplicable records fail closed during a first allocation-free scan.
Only then are bounded SME buffers allocated; the existing reviewed constructors
recheck feature dependencies and Q/Z aliases. Maximum admitted SVL is the
architectural 256 bytes.

After local decoding, the owned graph validates deterministic machine-memory
extents and FDT placement; topology/vCPU count, order, MPIDRs, primary identity,
timer PPI, and redistributor capacity; common DFR0/SME evidence; and equality of
mandatory versus reviewed SIMD/FP state. It also checks RTC equality with the
global compatibility facts, canonical VMGenID/VMClock placement and memory
backing, distinct in-range SPI lines, exact PVTime count/layout/backing, and
VMClock ABI validity. Encoding reruns the same graph validation. Native paths
are capped at 4096 bytes, boot arguments at 2047 bytes, and the maxima for 32
vCPUs, CPU evidence, optional registries, memory binding, GIC, and time state
remain below the independently enforced 16 MiB outer cap. Debug, display, and
error values redact paths, arguments, register/CPU values, MPIDRs, checksums
used as identities, GIC bytes, and clone identity.

Decode remains data only: it opens no artifact, creates no HVF VM or vCPU,
starts no owner thread, maps no memory, and restores no device. The separate
focused consumer is reached only after public family and profile admission.
Source capture
requires a completed topology pause, revalidates an exact suspended token
without consuming it, captures the opaque GIC only on vCPU 0 and ICC/timer/
pending/mandatory/reviewed-optional state on every owner, and recaptures the
lifecycle graph around the topology-ordered PVTime publication. It validates
guest VMClock/PVTime bytes and VMGenID agreement with the retained owner,
captures only portable time/identity semantics, validates checked inert boot
metadata, and streams a fresh state-bound memory image after those facts are
accepted. Failure leaves the paused source reusable.

Destination reconstruction accepts only the owned validated graph and an
already-authorized `GuestMemory`. Exact ranges, retained live-FDT
address/length/checksum, destination cache facts, time metadata, guest ABI
bytes, PVTime records, identity destinations, and notification lines are
checked before VM creation. Legacy 2.3 additionally parses those bytes as its
exact default process shell. Exact 2.4 through current 2.7 instead require the
source-product profile word and derive the versioned product shell from the
typed machine, transport, interrupt, optional storage graph, and (for 2.7)
serial component after checking every cross-binding. The
focused guard then creates VM, memory/dirty tracking, exact GIC, the
complete never-run topology, retained CPU-template targets, common identity,
global GIC, and canonical per-vCPU state. It next creates a fresh PL031,
configures destination PVTime from the saved cumulative values, preflights all
identity runners/signallers/guest writes, replaces and signals a fresh VMGenID,
applies and signals the saved-counter VMClock transition, imports fresh
lifecycle tokens, and only then publishes the focused owner `Paused`.

No raw topology, run control, or resume capability escapes earlier. Every
partial failure attempts topology then backend cleanup in reverse ownership
order and retains the primary plus all cleanup failures. A failure that commits
the first guest-visible VMGenID write, or occurs at any later identity,
lifecycle, or publication stage, is explicitly terminal even when cleanup
succeeds. Repeat loads never mutate the decoded source graph, and a paused
destination can recapture to a fresh memory/state identity for another restore.

The focused platform deliberately reconstructs no unprofiled optional virtio
device or host endpoint. The process adapter validates the exact legacy FDT
shell or typed product profile, composes the authorized profile-1 root,
profile-2 block owners, or profile-3 block/pmem owners, and for exact 2.7
reconstructs the complete UART with fresh default or configured destination
endpoints. It commits that owner into the normal lifecycle initially `Paused`.
Public native-v2 activation uses this exact boundary; serialization/restoration
for optional devices other than pmem and serial remains in their device
slices.

### Stable Paused vCPU Topology State

#1567 adds a wire-format-neutral lifecycle graph for a completed arm64
topology-wide pause. Its checked public value contains exactly `1..=32`
canonical index/MPIDR members, one validated EL1 virtual-timer PPI, and a
closed disposition for each vCPU: offline, runnable after explicit resume, or
suspended in PSCI `CPU_SUSPEND32/64`. The suspended form preserves X1-X3 and
the post-trap PC, rejects a misaligned PC, and redacts all architectural values
from `Debug`.

Capture is available on the public boot-vCPU session only while its coordinator
has completed the Paused barrier and no per-vCPU step or terminal result is
pending. It reconciles the topology snapshot with the PSCI power coordinator,
all pending CPU_ON/OFF/SUSPEND transactions, session-owned deferred work,
runner-owned suspend identity and request, timer PPI, HVC form, and X0-X3/PC.
Offline and ordinary runnable members are also probed for absence of an
unreported deferred PSCI call. A second coordinator observation must match
before the immutable graph is published.

Import validates PPI, member count, topology order, MPIDRs, and the never-run
readiness of every destination owner before the first mutation. It constructs a
fresh PSCI power model and a coordinator born Paused, installs each suspended
continuation through its destination owner, and assigns fresh destination-only
power and runner transaction identities.
Nothing can dispatch until explicit resume, whose first run generation is 1.
If any installation or publication step fails, the transaction aborts all
installed runner calls in reverse order, clears coordinator dispatch state,
records every cleanup failure without replacing the primary error, and shuts
down the consumed unpublished topology. A successful immediate recapture is
equivalent to the source stable graph but contains none of its process-local
tokens.

Native-v1 bytes and its one-vCPU profile remain unchanged. Native-v2 `2.2.x`
introduced this value as kind 4 and #1569 composes it with the complete
reviewed multi-vCPU register aggregate for unpublished paused reconstruction;
legacy `2.3.0` retains that exact topology payload and appends the
time/clone-identity component. Exact `2.4.0` retains both and appends the
single-root device graph; exact `2.5.0` retains the same platform/time payloads
and selects kind 7's ordered regular-file block profile; exact `2.6.0` selects
the ordered block-and-pmem profile. Current `2.7.0` adds required kind 8 serial
state and makes kind 7 optional. Production-resource ownership for optional
devices other than pmem and serial remains outside it.

## Native V1 Guest-Memory Image and Binding

The internal memory image is bangbang-owned and uses a fixed 48-byte
little-endian header, exact concatenated guest-memory bytes, and an 8-byte
CRC-64/Jones trailer:

| Offset | Width | Field | Native-v1 rule |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | bytes `BANGMEM\0` |
| 8 | 2 | version major | `1` |
| 10 | 2 | version minor | `0` |
| 12 | 2 | version patch | `0` |
| 14 | 2 | architecture | `1` means arm64 |
| 16 | 4 | guest page size | `4096` bytes |
| 20 | 4 | reserved flags | must be zero |
| 24 | 16 | image ID | opaque OS-random pair identity |
| 40 | 8 | guest-data length | at most 1,097,364,144,128 bytes |
| 48 | variable | guest data | exact canonical range order |
| final 8 | 8 | CRC64 | CRC-64/Jones over header and guest data |

The state-authoritative binding begins with a 72-byte header and then one
24-byte entry per exact `GuestMemory` region:

| Offset | Width | Field | Native-v1 rule |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | ASCII `BANGMBND` |
| 8..14 | 6 | semantic version | exact `1.0.0` |
| 14 | 2 | architecture | `1` means arm64 |
| 16 | 4 | guest page size | `4096` bytes |
| 20 | 4 | reserved flags | must be zero |
| 24 | 16 | image ID | exact memory-header match |
| 40 | 8 | guest-data length | exact range-size sum |
| 48 | 8 | complete file length | header + data + trailer |
| 56 | 8 | memory CRC64 | exact image trailer value |
| 64 | 4 | range count | `1..=4096` |
| 68 | 4 | reserved | must be zero |
| 72 + 24n | 8 | GPA start | 4096-byte aligned |
| 80 + 24n | 8 | range size | nonzero and 4096-byte aligned |
| 88 + 24n | 8 | absolute file offset | exact canonical offset |

The first range begins at file offset 48 and every next range begins after the
previous range's bytes. Actual region boundaries are preserved without
coalescing, including discontiguous, adjacent, and runtime-inserted regions.
The maximum binding is 98,376 bytes, below the 16 MiB outer state-payload cap.

Writers and loaders require a zero-origin `Write + Seek` or `Read + Seek`
handle. A writer rejects a nonempty handle without truncation; a loader checks
the binding's exact observed length before allocation. Both restore offset zero
after their seek-to-end preflight before returning a length error. Copying uses
one fallibly allocated 1 MiB buffer, checked GPA/offset arithmetic, and the
existing `GuestMemory::read_slice`/`write_slice` boundary. Load returns the
requested anonymous or descriptor-backed shared profile only after the exact
trailer, state-bound CRC, and observed EOF validate; public restore requests
anonymous memory, and partial memory drops on every failure.

The binding is nested inside the integrity-protected commit payload described
below. It is not a commit marker by itself, and the memory file cannot recover
its GPA layout without it. Image IDs are persistent mismatch detectors, not
secrets or authentication. CRC protects against accidental corruption only.

## Native V1 Commit Record and Artifact Publication

The fixed 32-byte little-endian commit header is followed by the exact validated
memory binding and, for kind 2 only, one bounded non-empty backend-state value:

| Offset | Width | Field | Native-v1 rule |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | bytes `BANGCMT\0` |
| 8..14 | 6 | semantic version | exact `1.0.0` |
| 14 | 2 | record kind | `1` means memory-only; `2` means composite |
| 16 | 4 | flags | must be zero |
| 20 | 4 | binding length | exact `BANGMBND` byte count |
| 24 | 8 | state length | zero for kind 1; exact backend-state length for kind 2 |
| 32 | variable | memory binding | fully validated, with no trailing bytes |
| following binding | variable | backend state | absent for kind 1; non-empty `BANGHVF\0` for kind 2 |

Kind 1 retains its exact original bytes and 98,408-byte maximum. Kind 2 uses the
remainder of the outer 16 MiB payload budget after its exact binding. Unknown
kinds, nonzero flags, a nonzero kind-1 state length, empty or oversized kind-2
state, nested binding failures, truncation, and trailing bytes fail closed.

On macOS, the internal publisher either opens each direct destination directory
once or accepts an already-opened contained output anchor, then performs every
namespace operation relative to that retained descriptor.
It rejects exact directory/component aliases and pre-existing regular files,
directories, FIFOs, sockets, and symlinks. Each artifact is prepared under an
unreported 128-bit-random private name created with `O_EXCL`, `O_NOFOLLOW`, and
mode `0600`. Publication uses directory-relative
`renameatx_np(..., RENAME_EXCL)`; filesystems without exclusive rename or usable
directory synchronization are unsupported rather than receiving a
replace-capable fallback.

The generalized publisher creates both staging entries only after all path,
directory, alias, and final-absence preflights, then invokes one synchronous
producer with a pathless, non-cloneable memory writer. The producer returns the
exact backend-neutral commit record for those bytes. Writer drop closes its
descriptor before setting a publisher-observed close proof; a retained or
forgotten writer fails immediately before verification, sync, or rename. A
fixed-size check compares observed position/length, memory header identity and
data length, and the stored checksum trailer with the trusted codec-produced
binding. It does not recompute the full CRC or validate GPA ranges; the loader
remains authoritative for both.

The ordered boundary is:

1. create both private files, run the producer, require its writer-close proof,
   verify its memory output against the returned record, write the state record,
   and call `sync_all` on both files;
2. publish memory exclusively and synchronize its destination directory;
3. publish state exclusively as the only commit marker and synchronize its
   destination directory.

Rust's Apple `File::sync_all` uses the platform's stronger `F_FULLFSYNC`
behavior. This ordering is intentionally expensive. It does not create one
atomic transaction across arbitrary directories: before state publication, a
failure may leave a typed memory-only orphan. Published final names are never
automatic cleanup targets. After state rename, a failed final directory sync
returns a committed-but-durability-uncertain outcome, not an ordinary error;
the visible pair must not be retried under the same names.

Loading opens and validates state first, or decodes an exact duplicate of a
contained state grant. Only a valid commit record permits the regular,
nonblocking, no-follow memory open or supplied-memory load and selected memory allocation.
The exact image identity, length, GPA layout, CRC, final position, and EOF must
all match before memory is returned. No VM or HVF state is constructed or
mutated.

Destination directories are trusted authority boundaries. Random names,
`0600`, retained descriptors, and immediate inode checks limit accidental
races, while `RENAME_EXCL` authoritatively prevents bangbang from replacing an
existing target at the rename instant. Darwin has no public rename or unlink
conditional on an already-open inode, so an uncooperative writer with directory
mutation rights can still race staging checks or replace final names later.
CRC and image identity are mismatch/corruption detection, not authentication.
Case- or normalization-equivalent absent names can also escape exact alias
preflight; the exclusive state rename then fails safely and may leave a memory
orphan. For granted outputs, strict per-artifact ownership records let the
launcher use its retained exact anchor after worker reap and unlink only a
current-user regular `0600`, single-link device/inode match; missing or replaced
entries are preserved. A worker hard death before its record is durable, or
simultaneous uncatchable worker/launcher death, can still leave staging residue.
App Sandbox also ties security-scoped authorization to the granted directory's
pathname: moving that directory after scope activation can deny later
descriptor-relative writes even though the anchor remains open.

### Native-HVF Composite Payload

The kind-2 state value has a 32-byte `BANGHVF\0` header carrying exact semantic
version `1.0.0`, profile `1`, zero flags, component count `5`, total length, and
zero reserved fields. Each component has an 8-byte kind/flags/length header.
The decoder requires these five non-empty components exactly once and in this
order; it does not skip unknown future components:

| Kind | Component | Native-v1 contents |
| ---: | --- | --- |
| 1 | machine/profile | Complete accepted `MachineConfig`: one vCPU, memory size, no SMT, optional active dirty tracking, no huge pages, and no CPU template. The load request independently selects destination tracking. |
| 2 | compatibility/platform | Baseline and conditional optional CPU IDs, primary MPIDR, one atomic default-vCPU cache feature/geometry manifest, exact GIC metadata, fixed PL031 MMIO metadata, and explicit fresh-system-RTC policy. |
| 3 | mutable vCPU | General, core-system, exception, execution-control, cache-selection, debug-control/trap, system-context, translation, pointer-authentication, thread-context, and SIMD/FP state. |
| 4 | timer/interrupt/GIC | Normalized timer state, CPU IRQ/FIQ levels, bounded opaque Hypervisor.framework GIC bytes, and all ten EL1 ICC registers. |
| 5 | baseline device | The exact nested `BANGDEV\0` profile for one read-only root block device, UART, limiter/retry time, VMGenID metadata/policy, and VMClock metadata plus complete ABI state. |

New capture writes nested `BANGDEV\0` semantic version `1.1.0` with the exact
validated 112-byte VMClock ABI. Load also accepts the historical nested 1.0.0
shape and derives that typed value from the independently bound memory page;
1.1.0 requires both copies to agree. This nested evolution does not change the
outer `BANGHVF\0` 1.0.0 component contract.

The no-template machine/profile rule covers both static and custom selection.
An effective custom ID-register template may start, but native-v1 snapshot
create rejects it before capture or artifact publication and serializes no
modifier content. Pending static `V1N1` cannot become a running/paused source
because its Neoverse V1 source-model gate runs before backend construction.
Empty custom or explicit machine `None` clears the selection and leaves the
ordinary profile unchanged. Snapshot load still requires pristine default
no-template controller state. No schema/version or destination CPU policy is
added; Wave 6 owns any broader template-bearing profile.

Construction and decode cross-check the machine memory size and one canonical
DRAM range against the memory binding, the primary MPIDR against CPU identity,
optional-feature absence/inactivity, the baseline GIC topology, fixed RTC
mapping, and every nested device queue/platform range. The native-v1
compatibility gate requires MSI-free GIC metadata; an opt-in GICv2m session is
rejected before capture publication or destination construction because no
message-delivery or consuming-device state belongs to this profile. The cache
values come
from one retained default `hv_vcpu_config_t`; they describe same-environment
compatibility and are not a cross-host portability claim. The opaque GIC blob
is bounded before allocation and can still be rejected by Hypervisor.framework
after a host update. PL031 is deliberately reconstructed fresh: no mutable RTC
register or alarm continuity is encoded.

### Capture-Ready Storage Handoff (not native-v1)

The paused boot owner exposes one internal, value-redacted storage boundary for
the later Wave 6 serializer. It is intentionally not included in the five
`BANGHVF\0` components above. The handoff contains:

| State | Exact retained boundary |
| --- | --- |
| Aggregate | Config-order block and pmem values plus the shared block/pmem retry state observed at one `Instant`. |
| Regular block | Complete `DriveConfig`, startup/runtime origin, MMIO region or PCI SBDF/BAR placement, stable opened-backing identity and capacity, cache/engine state, queues, device registers, limiter, retry, notifications, activation, and pending interrupt effects. |
| Async block | The regular-block state plus the same generation token, cache policy, next operation and sequence counters, stopped admission and pressure state, and checked zero counts for owned operations, parked host completions, and compact final completions. |
| Pmem | Complete `PmemConfig`, guest range/protection, stable backing identity, an opaque clone retaining the exact authoritative direct mapping owner, queue/token/limiter/retry state, startup/runtime origin, and complete MMIO or PCI transport state. |
| PCI transport | Full type-0 configuration image, common/device registers, queue selectors and cursors, notifications, activation, pending interrupt intents, and MSI-X table, PBA, vectors, enable/mask, and transition state. |

The owner first reserves and reconciles the complete inventory and scans every
live block backend for vhost-user. Vhost returns a typed redacted unsupported
error before admission or guest-memory mutation. Otherwise it closes every
Async generation before draining any generation, routes foreign completions,
performs the cache-sensitive persistence barrier, publishes each compact
completion through its exact MMIO/PCI owner with normal metrics and interrupt
effects, captures every device, and reopens all generations in reverse order.
Cancellation and recoverable errors drain entered work and attempt the same
reopen; uncertain completion publication or failed reopen is terminal.

This boundary deliberately adds no native-v1 variant, serialized storage
aggregate, load/restore path, migration promise, PCI persistence promise, or
vhost snapshot support. Wave 6 owns those versioned decisions.

### Composite Capture Boundary

The supervisor command detaches the accepted machine/drive/serial
configuration, reserves FIFO snapshot admission on a paused worker, and
failure-atomically quiesces block, PMEM, network, and entropy retry schedulers.
Only after all four acknowledge does it drain already-published tokens into
deferred work. One aggregate runner command then atomically reserves metadata,
core, timer, and interrupt operation domains and captures its fixed state order.
The boot session reuses the atomic cache manifest retained at startup,
cross-checks its MMFR2 identity against the runner capture, captures baseline
device state, validates and encodes all non-memory state, then streams the exact
guest-memory image to a consumed controlled `Write + Seek + Send` output in 1
MiB chunks.
Only a complete binding permits final bundle construction.

Cancellation is cooperative before each fixed stage, each memory chunk, the
trailer, final-length validation, and one atomic commit seal. Cancellation wins
before the seal and returns through normal producer cleanup. Once the seal wins,
shutdown remains pending while the worker completes verification, state
encoding, file and directory synchronization, the exclusive memory-first/
state-last commit, and the successful-publication hook. This preserves the
publisher's exact durable, durability-uncertain, orphan-visible, or other typed
result instead of replacing it with a generic admission failure.

Any recoverable failure drops the consumed writer and auxiliary guards before
releasing snapshot admission and leaves the source paused for retry or resume.
An individual blocking OS write cannot be forcibly preempted, so the public API
never supplies an arbitrary writer: the publisher owns a controlled regular
staging file. The complete public publisher now runs synchronously inside the
worker command; no writer alias, retry publisher, or ordinary worker command can
outlive or interleave with the transaction.

## Current Ownership and Pause Boundary

The current single-vCPU process keeps control-plane, run-loop, and HVF
ownership on separate threads:

| Owner | Live resources and responsibilities |
| --- | --- |
| Process owner | `ProcessVmm` owns the VMM controller, startup executor, and active `BootRunLoopSupervisor` handle. It serves API requests and commits public instance-state transitions, but it does not own the live boot session after startup. |
| Boot worker | The `bangbang-hvf-boot-loop` thread owns `ProcessHvfBootSession`, including packet I/O and `OwnedHvfArm64BootSession`. The latter owns mapped guest memory, the MMIO dispatcher and device resources, GIC metadata, metrics state, entropy state, and block, PMEM, network, and entropy retry schedulers. Device-update commands and the native-family publishers execute here under snapshot admission. |
| vCPU runner | The `bangbang-hvf-vcpu` thread owns `HvfVcpuOwner`. `HvfVcpuRunner` serializes HVF operations through commands and can return immutable X0-X30, PC, and CPSR values; guest-visible MIDR, MPIDR, and baseline PFR/DFR/ISAR/MMFR compatibility metadata; optional macOS 15.2 ZFR0/SMFR0 SVE/SME compatibility metadata; mutable macOS 15.2 SME `PSTATE.SM`/`PSTATE.ZA` controls; conditional maximum-width macOS 15.2 streaming Z0-Z31 bytes, maximum-derived P0-P15 predicate bytes, a maximum-SVL-square ZA matrix, and fixed 64-byte SME2 ZT0 contents in separate debug-redacted values; raw macOS 15.2 SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 values in a debug-redacted value; raw macOS 15.2 SCXTNUM_EL0 and SCXTNUM_EL1 software context numbers in a debug-redacted value with paired ordered restore; raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values with paired ordered restore; raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 values; raw ACTLR_EL1 and CPACR_EL1 values; raw CSSELR_EL1 cache-selection state with paired ordered restore; every DFR0-reported raw DBGBVR/DBGBCR hardware-breakpoint pair; every DFR0-reported raw DBGWVR/DBGWCR hardware-watchpoint pair; raw MDCCINT_EL1 and MDSCR_EL1 debug controls with paired ordered restore; raw Hypervisor.framework debug-exception and debug-register-access trap policy with paired ordered restore; raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 values with paired ordered restore; raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values with paired ordered restore; raw baseline Q0-Q31, FPCR, and FPSR values with paired ordered restore; raw APIA, APIB, APDA, APDB, and APGA pointer-authentication keys in a debug-redacted value with paired ordered restore; raw physical/virtual timers plus a normalized freeze-downtime timer value with paired never-run restore; CPU-level IRQ/FIQ pending values with paired ordered restore; Hypervisor.framework's opaque GIC device-state bytes with paired pre-first-run apply; or raw EL1 GIC ICC CPU-interface values with paired owner-thread capture and pre-first-run restore of nine mutable registers plus derived-RPR validation. A separate one-attempt never-run aggregate validates and restores every implemented breakpoint/watchpoint plus compatible SME PSTATE/system/Z/P/ZA/ZT0 and authoritative SIMD/FP state in one owner command; any failure makes that runner permanently execution-ineligible. The public native-v2 writer and native-v1/native-v2 loaders capture or restore their admitted state through aggregate commands that hold metadata, core, timer, and interrupt admission until completion. |
| Auxiliary and host | Limiter retry threads retain deadlines and can request vCPU cancellation during ordinary running or paused operation. Native snapshot publication temporarily quiesces all four current retry schedulers through artifact commit and the post-publication hook. The synchronous process owner cannot dispatch another API/MMDS/controller mutation or periodic callback until publication returns. The vmnet interface, vsock listener, retained streams, peers, and their host/kernel buffers remain outside snapshot state; the accepted profile has no network/vsock device, and a transient vsock polling thread is joined at the end of each vCPU run step. |

A successful public pause has a narrower boundary than a snapshot needs:

1. `ProcessVmm` validates `Running` and asks the supervisor to pause.
2. The supervisor queues a pause command, wakes the run loop, and cancels an
   active HVF run.
3. The boot worker finishes the canceled step's pending wakeup dispatch, drains
   the command, records its worker status as `Paused`, closes the pause gate,
   and acknowledges the command.
4. Only after that acknowledgement does `ProcessVmm` commit the public state to
   `Paused`.

After the acknowledgement, the worker cannot enter another guest run-loop
window until resume. The pause gate still wakes to drain commands, however, so
this is not a frozen runtime boundary. In particular:

- native-v2 snapshot commands wake the paused worker through that gate without
  issuing another vCPU wakeup; snapshot preparation may consume only an empty
  outer-wakeup barrier and must dispatch any idle cancellation debt before its
  topology-wide pause, while every other pending coordinator event fails
  closed;
- memory-hotplug updates and status queries can execute on the boot worker while
  paused, and updates can mutate mapped guest memory and device state;
- MMDS put and patch actions can mutate process-owned shared state;
- block, PMEM, network, and entropy retry schedulers retain their deadlines and
  can set wakeup tokens or attempt vCPU cancellation;
- explicit paused commands remain admissible even though periodic metrics and
  balloon-stat scheduling are suppressed; and
- vmnet packet queues and vsock connections can change in host or kernel buffers
  even when bangbang is not dispatching them to the guest.

The public pause path by itself does not capture vCPU, GIC, device, or
guest-memory state and does not transfer ownership of any live resource. The
native-v1 composite capture is a separate worker command invoked by an admitted
public create request only after that paused boundary;
it returns detached state and a binding, never live handles or mutable aliases.

The detailed inventory below records standalone primitives and their original
delivery boundaries. The aggregate native-v1 capture/restore described above
now consumes the fixed baseline subset through the public orchestrator. Older
per-slice statements that public activation was deferred describe their landing
time; the final implementation-split row supersedes them.

The HVF crate now has a narrower runner-local building block: one command reads
X0-X30, PC, and CPSR in architectural order on the owning thread and returns a
detached immutable value only after every read succeeds. A paired operation can
reapply that complete typed value on the same owner thread in X0-X30, PC, CPSR
order. Hypervisor.framework does not batch those 33 writes, so restore is
nontransactional: a typed failure identifies the failed register and number of
completed writes, after which the caller must retry the complete retained value
or discard the vCPU before execution. Generalized command-owned core-register
operation admission excludes runs, MMIO completion, boot setup, metadata,
timer, interrupt operations, cancellation, and shutdown until capture or
restore finishes, even when the caller abandons its response. Both boot-session
forms expose the operations. The public native-family orchestrators consume this
state through an aggregate command rather than these standalone operations;
the subset alone is not complete restorable vCPU state.

A second runner-local command reads raw `SP_EL0`, `SP_EL1`, `ELR_EL1`, and
`SPSR_EL1` values in that order and publishes one immutable value only after all
four reads succeed. A paired owner-thread operation writes the complete typed
value in the same order. Hypervisor.framework provides no four-write
transaction: a reusable typed system-register error identifies the failed
register and completed prefix, after which callers must retry the complete
value or discard the vCPU before execution. It shares a core-register admission
domain with the general-register commands and every capture, so no conflicting
runner operation can overlap it; command-owned admission survives response
abandonment and unwind. Borrowed and owned boot sessions delegate both
operations. The public native-family orchestrators use the aggregate command rather
than either standalone operation; this subset alone has no input validation,
persistence, wider restore ordering, or snapshot-schema meaning.

A separate core-register command reads raw `AFSR0_EL1`, `AFSR1_EL1`,
`ESR_EL1`, `FAR_EL1`, `PAR_EL1`, and `VBAR_EL1` in that order. It publishes
only after all six owner-thread reads succeed. A paired owner-thread operation
writes the complete typed value in the same order. The six SDK writes are
nontransactional and reuse the typed failed-register/completed-prefix error, so
callers must retry the complete value or discard the vCPU before execution.
Both commands share the same command-owned admission domain. Fault reports and
guest addresses are sensitive guest state; AFSR contents are implementation-
defined, and the value does not validate one coherent exception or include
vector-table memory. Both boot-session forms delegate capture and restore; the
public native-family orchestrators use the aggregate command rather than these
standalone operations. Signed coverage writes an aligned unused VBAR, restores the actual captured value
twice, and takes no later guest exception; captured AFSR readback is preserved
without assuming that either field is writable.

A separate core-register command reads raw `ACTLR_EL1` then `CPACR_EL1` and
publishes only after both owner-thread reads succeed. A paired owner-thread
operation writes the complete typed value in the same order. The two SDK writes
are nontransactional and reuse the typed failed-register/completed-prefix error,
so callers must retry the complete value or discard the vCPU before execution.
Both commands share the same command-owned admission domain. Complete capture
and restore require macOS 15 because Hypervisor.framework exposes only
`ACTLR_EL1.EnTSO` there; CPACR can contain optional FP/SIMD/SVE/SME access
controls that this raw value does not validate. Both boot-session forms delegate
capture and restore, while the supervisor lease and public snapshot paths
invoke neither. The value has no writable-bit, destination-feature, guest ISB,
wider ordering, persistence, or snapshot-schema policy. Signed coverage sets
only EnTSO and baseline FPEN, executes ISB before HVC, then restores the actual
capture twice without post-restore guest execution.

A separate core-register command reads guest-visible `MIDR_EL1`, `MPIDR_EL1`,
`ID_AA64PFR0_EL1`, `ID_AA64PFR1_EL1`, `ID_AA64DFR0_EL1`,
`ID_AA64DFR1_EL1`, `ID_AA64ISAR0_EL1`, `ID_AA64ISAR1_EL1`,
`ID_AA64MMFR0_EL1`, `ID_AA64MMFR1_EL1`, and `ID_AA64MMFR2_EL1` in that
order. It publishes only after all eleven owner-thread reads succeed and shares
the core-register admission domain, including bidirectional exclusion with the
standalone MPIDR metadata getter. These values describe the virtual CPU/HVF
feature view, not physical-host identity or mutable restore state; bangbang sets
MPIDR affinity to zero. Both boot-session forms delegate capture, but the
supervisor lease and public snapshot paths do not invoke it. Newer beta-only
IDs, broader configuration-time feature manifests, feature masks, destination
policy, persistence, and schema remain deferred. Signed coverage compares two
captures and the MPIDR getter without hard-coding one Apple CPU model or inferring
portability.

A separate macOS 11+ configuration query creates a fresh default
`hv_vcpu_config_t`, reads raw `CTR_EL0`, `CLIDR_EL1`, then `DCZID_EL0`, and
releases the retained object before returning one immutable value. It takes no
VM/vCPU handle and remains outside runner admission, boot sessions, and public
snapshot paths. Signed coverage compares two pre-VM queries with fixed messages
and no raw-value logging.

Another macOS 11+ configuration query creates a fresh default object, reads all
eight raw data or unified `CCSIDR_EL1` values followed by all eight instruction
values, and releases the retained object before returning one immutable
geometry value. It also takes no VM/vCPU handle and remains outside runner
admission, boot sessions, and public snapshot paths. The original standalone
feature and geometry queries remain independent compatibility surfaces. Their
raw arrays define no atomic manifest, implemented-level selection, field
interpretation, cross-host destination decision, selector synchronization,
cache maintenance, or restore policy. Signed coverage compares two pre-VM
queries with fixed messages and no raw-value logging.

Ordinary arm64 startup owns a separate combined source read from one retained
default configuration: MMFR2, the feature triple, and both CCSIDR arrays. #1392
interprets and independently reconciles that source with public macOS cache
facts before VM creation, then retains both the source and FDT presentation.
Native-v1 capture reuses its retained manifest after comparing MMFR2 with the
runner identification capture, so capture no longer re-queries a default
configuration and the native-v1 bytes/schema remain unchanged. Restore
reconstructs the compatibility source from the validated artifact but does not
invent an FDT presentation absent from the schema. None of these paths includes
the mutable live `CSSELR_EL1` selector or defines selector synchronization or
cache-maintenance policy.

A separate macOS 15.2+ core-register command reads guest-visible
`ID_AA64ZFR0_EL1` then `ID_AA64SMFR0_EL1` and publishes one optional SVE/SME
identification value only after both owner-thread reads succeed. It leaves the
eleven-register baseline capture unchanged, and both boot-session forms expose
it without involving the supervisor lease or public snapshot paths. These IDs
are compatibility metadata, not streaming execution state or mutable restore
state; broader configuration-time feature manifests, masks, destination policy,
persistence, and schema remain deferred. Signed coverage compares two idle-vCPU
captures without enabling SVE/SME, reading vector/predicate/matrix state,
executing the guest, hard-coding one model, or inferring portability.

A separate runtime-resolved macOS 15.2+ configuration query publishes the
maximum streaming vector length, in bytes, that guests may use. The SDK takes
no VM/vCPU handle, so the typed value is queried before VM creation and remains
outside runner admission and both boot-session forms. It is the conditional
Z-register allocation width, the basis for the conditional P-register width,
and each dimension of the conditional ZA allocation, not
the effective SVL selected through `SMCR_EL1`, feature or destination
compatibility policy, execution data, persistence, or a snapshot schema.
Missing symbols report the OS boundary and an available
symbol's exact `HV_UNSUPPORTED` result remains visible. Signed coverage compares
two successful same-host queries without logging the value, or accepts two
exact `HV_UNSUPPORTED` results.

A separate macOS 15.2+ core-state command runtime-resolves and calls
`hv_vcpu_get_sme_state` once on the owner thread, then publishes immutable
`PSTATE.SM` streaming-mode and `PSTATE.ZA` storage-enable flags only after the
call succeeds. Missing symbols return a structured older-macOS error, while an
available symbol's `HV_UNSUPPORTED` remains visible for SME-incapable hardware.
The flags are mutable execution controls, not identification metadata or the
conditionally present Z/P/ZA/ZT0 data. Both boot-session forms expose the
getter-only capture without involving the supervisor lease or public snapshot
paths. The command performs no maximum-SVL query; the separate configuration
value defines no setter, mode transition, persistence, schema, or restore
ordering. Signed coverage calls the getter twice on an idle vCPU without
assuming or logging values, changing PSTATE, reading SME data, or executing the
guest.

A nineteenth shared-core command conditionally captures streaming SVE Z0-Z31.
It first reads `PSTATE.SM` on the owner thread and returns a topical inactive
error before querying size or allocating when streaming mode is disabled. When
active, it queries the configuration-wide maximum SVL, validates and fallibly
allocates one contiguous `32 * maximum` buffer, then runtime-resolves the macOS
15.2+ `hv_vcpu_get_sme_z_reg` getter and fills exact maximum-width chunks in
architectural order. The typed value is published only after all 32 reads
succeed, exposes bounded slices, and redacts the complete buffer from `Debug`.
Both boot-session forms expose it, but the supervisor lease and public snapshot
paths do not invoke it. The maximum is an allocation width rather than effective
`SMCR_EL1.LEN`; P predicates, ZA, and ZT0 are captured separately. Setters and
transitions, layout interpretation,
feature/destination policy, protected persistence, schema, orchestration,
restore ordering, and multi-vCPU association remain deferred. Signed coverage
accepts only documented unavailability/inactivity or two complete equal idle-
vCPU captures without logging bytes or width, changing SME state, or executing
the guest.

A twentieth shared-core command conditionally captures streaming SVE P0-P15.
It first reads `PSTATE.SM` on the owner thread and returns the same topical
inactive error before querying size or allocating when streaming mode is
disabled. When active, it queries the configuration-wide maximum SVL, requires
that value to be non-zero and divisible by eight, fallibly allocates one
contiguous `16 * (maximum / 8)` buffer, then runtime-resolves the macOS 15.2+
`hv_vcpu_get_sme_p_reg` getter and fills exact predicate-width chunks in
architectural order. The typed value is published only after all 16 reads
succeed, exposes bounded slices, and redacts the complete buffer from `Debug`.
Both boot-session forms expose it, but the supervisor lease and public snapshot
paths do not invoke it. The maximum is an allocation basis rather than effective
`SMCR_EL1.LEN`; Z registers, ZA, and ZT0 are captured separately. Setters and
transitions, byte-layout and inactive-lane interpretation, feature/destination
policy, protected persistence, schema, orchestration, restore ordering, and
multi-vCPU association remain deferred. Signed coverage accepts only documented
unavailability/inactivity or two complete equal idle-vCPU captures without
logging bytes or widths, changing SME state, or executing the guest.

A twenty-first shared-core command conditionally captures the complete SME ZA
matrix. It first reads `PSTATE.ZA` on the owner thread and returns a topical
inactive error before querying size or allocating when ZA storage is disabled;
the SDK explicitly does not require `PSTATE.SM`. When active, it queries a
non-zero configuration-wide maximum SVL, checked-squares that byte count,
fallibly allocates the exact result, then runtime-resolves the macOS 15.2+
`hv_vcpu_get_sme_za_reg` getter for one complete read. The typed value is
published only on success, exposes the raw bytes without layout interpretation,
and redacts bytes and dimensions from `Debug`. Both boot-session forms expose
it, but the supervisor lease and public snapshot paths do not invoke it. The
maximum is an allocation dimension rather than effective `SMCR_EL1.LEN` or a
row/tile contract. ZT0 is captured separately. Setters and transitions, layout
interpretation, feature/destination policy, protected persistence, schema,
orchestration, restore ordering, and multi-vCPU association remain deferred.
Signed coverage
accepts only documented unavailability/inactivity or two complete equal idle-
vCPU captures without logging bytes or dimensions, changing SME state, or
executing the guest.

A twenty-second shared-core command conditionally captures the fixed 64-byte
SME2 ZT0 register. It first reads `PSTATE.ZA` on the owner thread and returns a
topical inactive error without a data read when ZA storage is disabled; the SDK
explicitly does not require `PSTATE.SM`. When active, it runtime-resolves the
macOS 15.2+ `hv_vcpu_get_sme_zt0_reg` getter and performs one read through a
private 64-byte, 16-byte-aligned SDK-compatible output value. It does not query
maximum SVL. The typed value is published only on success, exposes one fixed
array, and redacts every byte from `Debug`. Both boot-session forms expose it,
but the supervisor lease and public snapshot paths do not invoke it. Setters and
transitions, SME2 feature/destination policy, lane interpretation, protected
persistence, schema, orchestration, restore ordering, and multi-vCPU association
remain deferred. Signed coverage accepts only documented unavailability or
inactivity, or two complete equal idle-vCPU captures without
logging bytes, changing SME state, querying maximum SVL, or executing the guest.

A separate macOS 15.2+ core-register command reads raw `SMCR_EL1`,
`SMPRI_EL1`, and `TPIDR2_EL0` in that order and publishes one immutable value
only after all three owner-thread reads succeed. Because `TPIDR2_EL0` can hold
sensitive guest thread context, `Debug` redacts every register. Both boot-
session forms expose the getter-only capture without involving the supervisor
lease or public snapshot paths. It defines no writable-bit or feature
validation, maximum-SVL policy, persistence, schema, or restore ordering with
PSTATE and the conditionally present Z/P/ZA/ZT0 data. Signed coverage performs
two idle-vCPU captures without logging values, writes, data reads, or guest
execution.

A separate macOS 15.2+ core-register command reads raw `SCXTNUM_EL0` then
`SCXTNUM_EL1` and publishes one immutable value only after both owner-thread
reads succeed. These guest software context numbers can identify execution
contexts, so `Debug` redacts both values. Both boot-session forms expose the
capture and a separate owner-thread restore without involving the supervisor
lease or public snapshot paths. Restore accepts only the complete typed value,
writes EL0 then EL1, and reports the exact failed register and completed prefix
without values. The writes are nontransactional, so failure requires a complete
retry or vCPU discard before execution. It defines no interpretation, feature
or destination validation, protected persistence, rollback, schema, or wider
restore ordering with TPIDR and `CONTEXTIDR_EL1` state. Signed coverage captures
twice, then restores and recaptures the first complete idle-vCPU value twice
without logging values, guest execution, reset assumptions, or compatibility
inference.

A separate core-register command reads `ID_AA64DFR0_EL1`, derives the
architectural `BRPs + 1` implemented count, then reads each
`DBGBVR<n>_EL1` followed by `DBGBCR<n>_EL1` in ascending order. It exposes
only the implemented 1–16 prefix and publishes no state unless every read
succeeds. Breakpoint values can contain guest virtual addresses, Context IDs,
or VMIDs, and controls can describe enabled debug behavior, so the raw value is
sensitive. Both boot-session forms delegate this getter-only capture; it does
not write or enable debug state, change HVF trap policy, persist values, define
restore validation, or participate in the supervisor lease or public snapshot
paths. Signed coverage observes shape twice from an idle vCPU without guest
execution or model-specific reset assumptions.

A separate core-register command reads `ID_AA64DFR0_EL1`, derives the
architectural `WRPs + 1` implemented count, then reads each
`DBGWVR<n>_EL1` followed by `DBGWCR<n>_EL1` in ascending order. It exposes
only the implemented 1–16 prefix and publishes no state unless every read
succeeds. Watchpoint values contain guest data virtual addresses, and controls
can describe access type, byte selection, linking, and enabled debug behavior,
so the raw value is sensitive. Both boot-session forms delegate this getter-
only capture; it does not write or enable debug state, change HVF trap policy,
persist values, define restore validation, or participate in the supervisor
lease or public snapshot paths. Signed coverage observes shape twice from an
idle vCPU without guest execution or model-specific reset assumptions.

A separate core-register command reads raw `MDCCINT_EL1` followed by
`MDSCR_EL1` and publishes one immutable debug-control value only after both
owner-thread reads succeed. A paired owner-thread operation accepts only that
complete value and writes MDCCINT then MDSCR. The writes are nontransactional
and reuse the value-free failed-system-register and completed-prefix error, so
failure requires a complete retry or vCPU discard before execution. Both boot-
session forms expose capture and restore, but neither participates in the
supervisor lease or public snapshot paths. Signed coverage restores and
recaptures the original idle-vCPU pair twice without assuming or logging either
register, manufacturing a control change, altering comparators or host trap
policy, activating debug behavior, or executing the guest. Writable/status-bit
and destination validation, comparator/trap coordination, protected
persistence, rollback, schema, and wider debug restore ordering remain deferred.

A separate core-state command calls Hypervisor.framework's debug-exception trap
getter followed by its debug-register-access trap getter and publishes the two
host policy booleans only after both owner-thread calls succeed. They correspond
to `MDCR_EL2.TDE` and `MDCR_EL2.TDA`, not guest EL1 debug-register contents.
A separate owner-thread operation accepts only that complete typed value and
sets debug-exception policy followed by debug-register-access policy. The two
writes are nontransactional; a dedicated value-free error reports the failed
operation and completed prefix, so failure requires a complete retry or vCPU
discard before execution. Both boot-session forms delegate capture and restore,
but neither operation participates in the supervisor lease or public snapshot
paths. Signed coverage restores and recaptures the original idle-vCPU pair twice
without assuming or logging either Boolean, manufacturing a policy change,
altering guest debug registers, or executing the guest.

A separate core-register command reads raw `SCTLR_EL1`, `TTBR0_EL1`,
`TTBR1_EL1`, `TCR_EL1`, `MAIR_EL1`, `AMAIR_EL1`, and `CONTEXTIDR_EL1` in that
order. It publishes only after all seven owner-thread reads succeed and shares
the same command-owned admission domain. Table bases and context ids are
sensitive guest state. A separate owner-thread operation accepts only the
complete typed value and writes all seven fields in capture order. The writes
are nontransactional and reuse the exact failed-system-register and completed-
prefix error, so failure requires a complete retry or vCPU discard before
execution. Both boot-session forms delegate capture and restore; the public
native-family orchestrators use the aggregate command rather than these standalone
operations. The value does not
include table memory, feature or destination validation, barriers, TLB/cache
maintenance, or a safe MMU transition sequence. Signed coverage leaves the MMU
disabled, preserves actual implementation-defined AMAIR readback, and restores
and recaptures the same complete value twice without later guest execution.

Another core-register command reads the low and high halves of APIA, APIB,
APDA, APDB, and APGA in that order and publishes five 128-bit keys only after
all ten owner-thread reads succeed. Pointer-authentication keys are
cryptographic secrets, so the detached value redacts all key material from
`Debug`; its named accessors are intended only for trusted internal composition.
An owner-thread restore accepts only that complete typed value and writes the
same ten low/high halves in capture order. The writes are nontransactional and
reuse the value-free failed-system-register and completed-prefix error, so
failure requires a complete retry or vCPU discard before execution. Capture and
restore share the core-register admission domain, and both boot-session forms
expose them without involving the supervisor lease or public snapshot paths.
The value defines no feature/algorithm or destination validation, memory
zeroization, protected persistence, safe SCTLR enable ordering, rollback, or
schema policy. Signed coverage restores and recaptures visibly fake keys twice
without enabling or executing PAC or running the guest afterward.

Another core-register command reads all 16 bytes of Q0-Q31 in ascending order,
then raw FPCR and FPSR, and publishes one immutable baseline SIMD/FP value only
after all 34 reads succeed. A separate owner-thread operation accepts only that
complete typed value and writes Q0-Q31, FPCR, then FPSR. The writes are
nontransactional; a dedicated typed error distinguishes the SIMD/FP and scalar
register spaces and reports the completed prefix, so failure requires a
complete retry or vCPU discard before execution. The SDK's by-value vector
setter crosses one macOS arm64 C shim because stable Rust cannot declare that
SIMD FFI; Rust passes only a pointer to 16 bytes. Capture and restore share
command-owned admission with the general,
core-system, exception, execution-control, cache-selection, breakpoint,
watchpoint, debug-control, debug-trap, baseline identification, optional SVE/SME
identification, SME PSTATE, SME Z-register, SME P-register, SME ZA-register,
SME ZT0-register, translation, pointer-authentication, thread-context restore,
SME system-register, and system-context operations. Both boot-session forms
expose capture and restore without involving the supervisor lease or public
snapshot paths.
Hypervisor.framework aliases Q registers to the
low 128 bits of Z registers in streaming SVE mode; this subset therefore defines
no ordering with wider Z contents, P predicates, the ZA matrix, or ZT0. Those
values use separate conditional capture commands, and none defines a restore or
snapshot-schema contract.
Signed coverage restores and recaptures the actual complete non-streaming
guest-written baseline value twice without a later guest run or raw-value log.

Another core-register command reads raw `TPIDR_EL0`, `TPIDRRO_EL0`, and
`TPIDR_EL1` in that order and publishes one immutable value only after all three
reads succeed. These software thread-ID fields can contain guest TLS or kernel
pointers. A separate owner-thread operation accepts only that complete typed
value and writes the three fields in capture order. The writes are
nontransactional and reuse the exact failed-system-register and completed-
prefix error, so failure requires a complete retry or vCPU discard before
execution. The capture and restore share admission with the general,
stack/exception-return, exception-report, execution-control, cache-selection,
breakpoint, watchpoint, debug-control, debug-trap, identification, translation,
SVE/SME identification, SME PSTATE, SME system-register, pointer-authentication,
SME Z-register, SME P-register, SME ZA-register, system-context, and SIMD/FP
operations and are exposed through both boot-session forms. `TPIDR2_EL0` is captured separately
with SME system registers, while `SCXTNUM_EL0`/`SCXTNUM_EL1` use a separate
system-context value. Address/destination validation, wider context ordering,
persistence, and schema remain outside this value. The public native-family
orchestrators use the aggregate command rather than either standalone
operation.

A separate runner-local command captures raw `CNTKCTL_EL1`, `CNTP_CTL_EL0`,
`CNTP_CVAL_EL0`, and `CNTP_TVAL_EL0` in that order and publishes one immutable
value only after all four reads succeed. It shares generalized timer admission
with every virtual-timer getter, setter, and aggregate capture, and its command-
owned admission survives response abandonment and unwind.
Hypervisor.framework exposes the CNTP registers on macOS 15 and newer only when
the VM creates its GIC before the vCPU. The control ISTATUS bit is derived, CVAL
is an absolute comparator
against a continuing physical count, and the architecturally signed 32-bit TVAL
is a relative view returned as raw `u64`. TVAL changes while the sequential
CVAL/TVAL reads proceed, so this subset has no simultaneous-value guarantee,
portable elapsed-time adjustment, interrupt-delivery, writable-bit, or restore
policy.
Both boot-session forms delegate capture, while the supervisor lease and public
snapshot paths do not invoke it.

A separate runner-local command captures the HVF virtual-timer mask, raw offset,
raw `CNTV_CTL_EL0`, and raw `CNTV_CVAL_EL0` in that order and publishes one
immutable value only after all four reads succeed. It shares one serialized
timer admission domain with individual access to every captured field,
and its command-owned admission remains active when the caller drops its
response. The offset is the host-time-relative HVF value in
`CNTVCT_EL0 = mach_absolute_time() - offset`; the control register's ISTATUS bit
is derived and may change as virtual time advances. This narrow subset omits
GIC state and does not define a portable offset
adjustment or control-restore policy. Borrowed and owned boot sessions delegate
this capture, but the supervisor lease and public snapshot paths do not invoke
it.

#1261 adds a separate native-HVF timer policy rather than assigning restore
meaning to either raw capture. One owner-thread command reads physical state,
then virtual state, then samples `mach_absolute_time()` once. It stores the
frozen virtual count as `sample - raw_offset` and the full-width physical
comparator distance as `raw_CNTP_CVAL - sample`, using wrapping `u64`
arithmetic. Restore samples the destination counter and reconstructs
`offset = sample - virtual_count` and
`CNTP_CVAL = sample + physical_compare_delta`. Snapshot downtime therefore
does not advance either guest timer domain, while both domains resume advancing
from the destination restore instant. Raw TVAL is not a restore source;
derived ISTATUS is stripped; control bits outside ENABLE, IMASK, and captured
ISTATUS fail closed.

The never-run restore preflights every physical and virtual timer getter plus
the destination counter before its first mutation. It then masks vTimer exits,
disables both controls, writes CNTKCTL, adjusted physical CVAL, adjusted virtual
offset, and virtual CVAL, restores physical then virtual ENABLE/IMASK, and
restores the captured vTimer mask last. The ten writes are nontransactional. A
value-free error names the failed read/sample/write and completed write prefix;
a retry restarts at the mask with a fresh sample, otherwise the caller discards
the destination. Command admission prevents an overlapping runner operation,
and the sticky run flag rejects restore after even a failed run attempt, but it
does not supply a lease across other restore commands.

The same policy module classifies native-v1 optional state before future
composition. It rejects CPACR ZEN or SMEN access, active PSTATE.SM or PSTATE.ZA,
and any enabled implemented DBGBCR or DBGWCR, in that order and without values
or comparator indexes. Acceptance is only an inactive-state policy decision;
it does not make other getter-only SVE/SME/debug captures restorable.

Prepared borrowed and owned boot sessions also have a never-run VMGenID
replacement primitive. It preloads and range-checks the GIC SPI signaler,
generates a nonzero 16-byte value distinct from retained metadata, writes all
16 guest bytes, commits metadata only after that write, and finally calls
`hv_gic_set_spi(line, true)`. Apple defines each true call as an edge for an
edge-triggered SPI, so no artificial low transition is sent. A signal failure
is an explicit post-commit partial stage; retry generates another distinct
value and signals again, or the caller discards the session. Generation bytes
are redacted from device and error `Debug` output.

A separate interrupt command captures the CPU-level IRQ then FIQ pending
injection values and publishes one immutable value only after both owner-thread
reads succeed. A paired command writes the complete typed value in IRQ-then-FIQ
order, reports the exact failed type and completed prefix without values, and
requires a complete retry or vCPU discard after failure. Individual IRQ/FIQ
get/set commands and validated GIC PPI set/clear commands share one generalized
interrupt-operation admission domain with both aggregate commands, while CPU
levels and GIC state remain distinct models. HVF clears the CPU pending levels
after a vCPU run returns, so setters and aggregate restore are pre-run injection
primitives rather than durable delivery state. Both boot-session forms delegate
capture and restore. The public native-family paths use the aggregate command rather
than either standalone operation. Native-v1 capture persists the pending levels
with the separately modeled GIC device and EL1 ICC values, and load restores all three
inside one never-run aggregate command before VMGenID notification.

Another command creates Hypervisor.framework's opaque GIC state object, queries
and fallibly allocates its reported size, copies the complete serialized GIC
device state except CPU system registers, and releases the retained object on
every outcome. Apple defines the bytes as stable and versioned, but restore can
still reject them after host software changes. A separate setter-only dynamic
capability reapplies the exact complete non-empty value on the same owner loop
after the GIC and vCPU exist and before any run command has ever been enqueued.
Both commands share generalized interrupt admission with CPU pending operations
and GIC PPI mutation; a locked sticky run check makes the apply ordering atomic
against `hv_vcpu_run`. Future multi-vCPU support needs a broader stop barrier.
Both boot-session forms delegate capture and apply. Standalone apply still ends
its admission before response delivery, while the native-v1 loader uses the
aggregate command to keep the complete restore order indivisible; the public
orchestrator does not call the standalone apply. Apply clones the redacted value into command
ownership, preserves the exact HVF status, and defines no rollback or safe
same-VM retry after failure. Standalone apply neither quiesces device-side SPI
producers nor supplies a lease across ICC, timer, pending, vCPU, and device
restore. The value redacts its bytes from `Debug`; native-v1 persists it as an
opaque bounded component and treats setter rejection as destination
incompatibility, without parsing the bytes or claiming migration portability.

A companion command captures the ten EL1 ICC CPU-interface registers exposed
by Hypervisor.framework: PMR, BPR0, AP0R0, AP1R0, RPR, BPR1, CTLR, SRE,
IGRPEN0, and IGRPEN1. It reads every value on the vCPU owner thread and publishes
only after all reads succeed. A paired pre-first-run command loads independent
getter and setter capabilities before its first mutation, writes the nine
architecturally mutable registers in capture order, and reads the derived,
read-only RPR at its original position to require equality with the capture.
This split also matches signed Apple Silicon evidence: setting the original idle
RPR returned `HV_DENIED`, while omitting only that forbidden call allowed all
nine mutable writes and exact complete recapture.
The nontransactional operation reports the exact register, write or derived-
validation operation, completed-write count, and backend source without raw
values; after failure callers must retry the complete retained value or discard
the vCPU before execution. Both commands share generalized interrupt admission
with CPU pending operations, GIC PPI mutation, and the opaque device-blob
commands. The fixed value is per-vCPU and separate from the VM-scoped opaque
blob. Both boot-session forms delegate capture and restore, while public
native-v1 orchestration uses their aggregate equivalents rather than these
standalone commands. Callers must apply the compatible opaque blob first, but
the two standalone commands do not form a cross-step no-run lease.
`ICC_SRE_EL2`, ICH/ICV virtualization state, destination validation, host-update
preflight, multi-vCPU association, composite orchestration, and persistence
remain deferred.

Public paused snapshot create exercises the lease-based ownership boundary. A
separate admission cell atomically reserves snapshot preparation
and submits an exclusive FIFO command. Commands admitted earlier execute first;
later ordinary commands, device updates, memory-hotplug mutations, and resume
reject before enqueue. The boot worker revalidates `Paused`, enters the scoped
lease, and acquires acknowledged quiescence guards from the block, PMEM,
network, and entropy limiter retry schedulers in that order. Acquisition waits
for an already-started wakeup publication and vCPU-cancel attempt to finish.
Only after all four acknowledge does the worker drain pending tokens into
deferred work. Partial acquisition rolls back the earlier guards; while the
complete aggregate guard is held, no scheduler can publish another token or
cancel attempt.

Native-v1 keeps the guards and supervisor lease through non-memory state
capture, complete memory streaming, publisher verification and encoding,
durability barriers, exclusive memory-first/state-last publication, and a
successful-publication hook that is intentionally a no-op in this slice. The
guards drop before the supervisor lease reopens ordinary admission. Success
returns `204 No Content` with the controller still `Paused`. The synchronous
`&mut ProcessVmm` call also prevents API/MMDS/controller and periodic work from
interleaving; cancellation remains the sole out-of-band process mutation.

Cancellation before the commit seal cleans only owned staging and returns the
typed producer failure. A signal after the seal cannot interrupt the publisher;
the exact artifact-visibility result and hook decision are delivered before
shutdown consumes the pending wakeup. Queue/response closure, worker terminal
status, unwind, panic, and repeated release leave no active registration or
lease and restore admission only when the source remains recoverable.

Limiter deadlines remain absolute while quiesced. A due deadline or drained
token is republished asynchronously after guard release, duplicate immediate
work is coalesced, and a distinct future deadline is retained. Canceling a
scheduled retry clears both its deadline and deferred publication. Scheduler
stop is terminal and cannot be undone by a late guard drop.

This lease does not change ordinary paused behavior outside its scope. It was
sufficient alone only for the historical device-free/single-root profiles.
Current profile 2 composes it with the complete regular-file block-vector
drain, graph, resource, and destination transactions described above. It still
is not a generic snapshot-ready contract for vmnet, vsock, optional devices,
MMDS, vhost-user, or other unmodeled host resources.

## Firecracker Requirements

Firecracker snapshots are more than a control-plane endpoint. A compatible
implementation has to coordinate these pieces:

- VM lifecycle: snapshot creation requires a paused microVM; loading a snapshot
  creates a paused microVM before optional resume.
- Guest memory: create writes a separate memory file; load maps or populates
  guest memory from a memory backend.
- VM and vCPU state: the VMM serializes VM state, vCPU state, and architecture
  state needed to resume execution.
- Device state: every emulated device that can exist at snapshot time needs a
  persisted and restored model state.
- Dirty tracking: diff snapshots depend on a dirty-page mechanism or another
  explicitly documented fallback.
- Host resources: disk files, network interfaces, and vsock backends remain
  user-managed resources outside the snapshot files.
- Data format: the state file has a versioned format; API compatibility alone
  does not imply on-disk Firecracker snapshot compatibility.

## HVF Feasibility

The inspected Xcode SDK Hypervisor.framework headers expose building blocks for
some of the required state:

- `hv_vm_map`, `hv_vm_unmap`, and `hv_vm_protect` can map current-process
  memory into guest physical address space and adjust permissions.
- Apple Silicon vCPU APIs expose general register, system register, SIMD/FP,
  SME, pending-interrupt, virtual-timer mask, and virtual-timer offset get/set
  operations. On macOS 15 and newer, physical-timer CNTP system registers are
  available only when a GIC is created before the vCPU.
- vCPU lifecycle and register APIs are thread-affine. bangbang also routes
  physical- and virtual-timer and pending-interrupt access through the owning
  runner thread as its serialization policy, so a future capture bundle has one
  explicit vCPU command boundary after the VM is quiesced.
- macOS 15 GIC APIs expose GICv3 distributor, redistributor, ICC, ICH, ICV, MSI,
  and SPI state access and interrupt injection primitives. They also expose a
  retained state object whose stable, versioned opaque bytes cover the GIC
  device except separately captured CPU system registers.
  The implemented PCI MSI foundation configures HVF's GICM region as a Linux
  GICv2m frame and exposes a range-bound send capability, but it does not add
  delivery rollback or portable migration semantics. Product PCI binds its
  generic ECAM FDT host to that frame; its shared routes, leases, MSI-X tables,
  and function/device state deliberately have no native-v1 schema or restore
  path.

The inspected headers do not expose a KVM-style dirty log or dirty-page tracking
API, so Firecracker-style diff snapshot parity is not a direct HVF API mapping.
The implemented low-level guest-CPU primitive instead removes WRITE from every
mapped writable guest-RAM range with `hv_vm_protect`, records the first owned
page exit, restores that page's original permission, and leaves the same store
for the caller's next bounded run. Signed Apple Silicon evidence observes EC
`0x24`, WnR set, CM/S1PTW clear, and exact DFSC `0x07` for initial protection
or `0x0f` after a page is re-protected for the next epoch. Those values are an
empirical Hypervisor.framework contract, not encodings Apple documents; every
other value is declined and follows the existing MMIO/error path.

The primitive starts only after memory mapping and before any vCPU owner, and it
stops only after every owner has joined. Activation protects complete ranges
transactionally; an incomplete rollback, a page-unprotect failure, or an
incomplete stop blocks further execution/mapping mutation until cleanup or VM
unmap. One backend-neutral atomic bitmap is shared with `GuestMemory`: normal
boot installs it before kernel/initrd/FDT/device population, while snapshot load
installs it after baseline image population and before mapping, owners, and
VMGenID/VMClock updates. Bounded host/device writes and conservative discard
attempts mark it directly; CPU faults mark it through a separate HVF
restored-WRITE overlay. A different vCPU may consume only one stale exit already
raised for the same page. Dirty handling does not advance PC, dispatch MMIO, or
run the guest again internally.

Writable dynamic RAM is installed protected and wholly dirty; removal drops its
exact bitmap/protection metadata. After a Full pair is visibly committed inside
snapshot-ready quiescence, restored pages are re-protected before the shared
generation clears. Complete rollback retains the old epoch; incomplete rollback
blocks resume and requires teardown. Machine/load tracking flags are enabled.
`Diff` artifact serialization and merging/restore, dirty-tracked or
unsupported-profile external paging, and broader snapshot compatibility remain
outside native-v1.

### Public UFFD-equivalent feasibility

The checked
[snapshot paging contract](../compat/firecracker/v1.16.0/snapshot-paging-contract.md)
pins Firecracker v1.16.0's external anonymous-memory/UFFD descriptor contract
and the #1527 public-macOS evidence. Signed probes on arm64 macOS 26.5.2 with
the public macOS 26.5 SDK established two independent protection planes:
zero-permission HVF mappings produce exact guest IPA exits, while a task-local
server generated from public `mach_exc.defs` can resolve an owned host fault
inside the production App Sandbox entitlement floor. Host `PROT_NONE` alone
does not trap guest access.

The combined signed and production-entitlement probe delegated page contents
and removal state to a socket-connected child and passed host and guest
population, removal/refault-to-zero, peer-death detection, and cleanup.
External Mach exception handling was rejected because it exports task-wide
authority, fails unsafely on handler loss, and cannot use the tested production
App Sandbox discovery path. A public custom Mach memory-object pager was also
rejected by SDK, XNU, and runtime evidence.

This proves an observable equivalent is feasible, not that Linux UFFD
descriptor/wire traffic is accepted. The standalone
[`bangbang-pager-v1` protocol](snapshot-pager-protocol.md) now implements the
closed bounded offset-only wire, exact negotiation and request matching,
role-specific state machines, and already-connected deadline transport.

`bangbang_runtime::lazy_memory` now supplies the one backend-neutral
generation-aware coordinator shared by the later fault planes. It
transactionally owns private-anonymous mappings through a type distinct from
initialized `GuestMemory`, records one compact state byte per selected page,
and bounds outstanding protocol operations and local waiters separately. The
first fault owns one immutable population ticket; duplicates coalesce only
content and re-evaluate their later permissions. Exact scoped guards serialize
data/zero publication and removal without holding the coordinator lock during
the bounded mapping action.

A locally superseded population remains a counted retired protocol operation
until response consumption, ticket drop, or terminal teardown. Removal must
reserve its own slot before mutation and remains `Removing` after local zeroing
until explicit peer-acknowledgement commit. Requested cancellation, peer
failure, abandoned work, poison, generation exhaustion, and teardown close
admission and wake waiters; explicit termination drains already-linearized
actions while destructors stay nonblocking.

`bangbang-hvf` now supplies the host adapter for this internal ownership. It
generates public `mach_exc` stubs from the active SDK, atomically captures the
prior task bad-access configuration, retains non-copying writable aliases,
protects original mappings, and resolves exact owned read/write faults through
the same coordinator. Complete bytes become visible only after alias copy and
an ordering fence. Unowned or unsupported exceptions forward to the prior
legacy/Mach behavior, shutdown restores only while still current, and an owned
failure exits the supervised worker with fixed status 70. Task/thread ports
and host addresses remain inside the process. Signed direct and App Sandbox
tests cover reads, writes, atomics, raw pointers, forwarding, later-owner
preservation, repeated cleanup, and terminal failure.

The HVF guest adapter maps lazy regions with no initial stage-two access,
classifies owned data/instruction aborts, resolves all touched pages,
synchronizes instruction contents, publishes serialized read/write/execute
permission unions, and retries without advancing PC.
Multi-vCPU duplicates share coordinator work, while stale no-progress,
resolver, and protection failures close the path. Signed lifecycle and guest
boot cases cover execute/read/write population, failure, cancellation, and
cleanup, including one deliberately concurrent two-vCPU page request and an
unowned instruction fault that retains the existing error path.

The coordinator is now connected to the protocol through the launcher's
connected-stream grant, and ordered peer removal/failure plus the checked
consumer boundary are implemented. A one-shot protected view covers
in-process slice/atomic/raw/device/full-snapshot access under the Mach bridge;
shared/export, dirty, ordinary balloon discard, dynamic topology, and public
memory-borrow paths reject before escape.

Native-v1 `Uffd` now accepts the same fixed-memory, one-vCPU native-v1 state
profile on macOS Apple Silicon with dirty tracking disabled. Preflight checks
the platform, machine/profile, protected consumer class, and exact contained
pager grant before opening the snapshot state or connecting a direct socket.
State validation then derives a 32-byte pager session from the 16-byte memory
image ID, CRC-64/Jones value, and data length; validates the GPA layout and
header-relative source offsets; opens or grant-adopts and validates the exact
root backing; negotiates the peer; and only then publishes
the private-anonymous memory, host/guest fault owners, HVF mappings, vCPU,
devices, and runtime. Direct mode connects with a bounded deadline. Contained
mode one-time claims the launcher-connected stream and deliberately consumes no
snapshot-memory file grant in the worker. Every preparation or construction
failure unwinds owned resources; after the one-shot peer is adopted, such a
failure cancels it and terminalizes the VMM process instead of advertising a
retry that cannot reuse the stream. No File/COW or eager fallback is permitted.
#1555's final signed certification proves paused host demand, exact
post-resume guest instruction/read/write pages, removal before/during/after
population with zero refault, multi-vCPU/failure/death/repeat cleanup, and the
exact production entitlement floor. The checked corpus is now terminal for
this narrow profile; dirty/shared/external, Diff, optional-device,
multi-vCPU-artifact, portability, and Linux UFFD wire profiles remain excluded.

### Implemented public native-v1 restore order

The public load orchestrator holds one aggregate never-run runner admission
window and uses this order only after complete compatibility and optional-state
validation:

1. construct validated guest memory and baseline devices, then create and
   validate the GIC;
2. when requested, attach a clean dirty bitmap after image population, map and
   protect guest RAM, and only then create the one vCPU owner;
3. restore baseline architectural register and data state in its documented
   dependency order, while active SVE/SME/debug optional state remains rejected;
4. apply the compatible opaque GIC device blob;
5. restore and validate the EL1 ICC CPU-interface state;
6. restore normalized physical and virtual timers, taking timer-PPI state from
   the compatible GIC image rather than replaying TVAL or ISTATUS;
7. restore CPU IRQ/FIQ pending injection last among runner-owned state;
8. preflight mapped memory and both time/identity SPI lines, replace the complete
   guest VMGenID buffer with a fresh value, and inject its SPI only after every
   GIC restore, so the notification cannot be overwritten;
9. publish the VMClock odd/release/disruption-plus-generation/release/even
   sequence and inject its SPI; and
10. commit a paused session and permit resume only after every step succeeds.

The runner-owned portion is one command rather than a transaction: an HVF write
failure may leave a prefix applied, so the destination is torn down and explicit
cleanup evidence decides whether the process may retry. VMGenID replacement,
VMClock update, session assembly, and initially paused worker handoff remain in
the same cleanup ledger until controller commit. A completely cleaned failure
before either identity mutation may retry; any VMGenID/VMClock guest-memory or
notification commit is terminal even after complete resource cleanup.
`PUT /snapshot/load` invokes this sequence only after pristine-request and
committed-pair validation, commits `Paused`, and optionally invokes ordinary
resume.

## Native-v1 Snapshot-Ready Ownership

The implemented baseline builds an internal, exclusive quiescence lease on top
of the public `Paused` state. It is complete for the admitted native-v1 profile,
but it is not a generic contract for optional resources. None of its phases is
a new Firecracker-facing instance state.

The process owner requests preparation through the supervisor but does not take
the live session from its worker. The boot worker acquires, owns, and releases
the lease because it already owns guest memory and device dispatch. The vCPU
runner retains all thread-affine HVF access. Native-v1 uses a bounded aggregate
capture command while the lease is held; command ordering alone would not
establish the lease without the process/worker admission and acknowledged
auxiliary quiescence boundaries.

### Internal lifecycle

| Internal phase | Required behavior |
| --- | --- |
| Ordinary `Paused` | Today's pause acknowledgement has completed. Paused commands and the mutations listed above can still occur. |
| Supervisor preparing | Implemented for admitted native-v1 create. Admission reservation and nonblocking FIFO submission share one lock, so earlier commands precede capture and later ordinary commands reject. The public controller remains `Paused`. |
| Supervisor leased | Implemented after worker-side pause revalidation. It closes ordinary supervisor command admission and failure-atomically acknowledges block, PMEM, network, and entropy limiter retry quiescence; tokens are drained only after all four acknowledge. |
| Snapshot-ready | Implemented for the admitted native-v1 baseline: fixed-profile validation, aggregate state capture, complete guest-memory streaming, artifact verification/synchronization, exclusive commit, and the post-publication hook occur while the lease remains held and the public controller stays `Paused`. |
| Supervisor releasing | Implemented for scoped success, operation error, response closure, unwind, and shutdown invalidation. Recoverable release restores ordinary paused admission exactly once. |

The implemented native-v1 path acknowledges the following invariants for its
fixed baseline. Any profile expansion must prove the corresponding additional
owners before admission:

- no vCPU run or MMIO completion is in flight, no new run can start, and the
  runner accepts only lease-authorized capture operations;
- no device dispatch or device-update command is active, and later mutating
  commands are rejected or deferred by an explicit admission policy;
- guest memory is stable except for access performed by the lease-owning capture
  path, including no memory-hotplug mutation;
- process-owner mutations that can affect captured state, including MMDS
  changes, are rejected or deferred; future work must classify genuinely
  read-only requests separately;
- periodic work cannot re-enter the synchronously borrowed process, and each of
  the four current retry schedulers has acknowledged quiescence, with no
  deadline thread able to publish another wakeup token;
- no VMM thread is reading or writing vmnet packets or vsock streams, and the
  transient vsock poller has joined;
- lease acquisition and capture are bounded or observe an out-of-band stop
  token, so shutdown does not depend on queueing a command behind lease-owned
  work or on the synchronous API requester making progress; and
- shutdown and terminal status are checked before readiness is returned, so a
  stale successful acknowledgement cannot outlive the session.

The vmnet and vsock invariant controls bangbang's access to external resources;
it does not freeze the host. Packets may accumulate in vmnet/kernel queues, and
peer activity may change socket buffers or connection state. Those resources
need an explicit metadata, discard, or reconnect policy during later restore
design. Live host descriptors and opaque kernel buffers are outside the guest
snapshot state unless a later design proves otherwise.

### Capture locality

| State or operation | Required owner |
| --- | --- |
| General, system, SIMD/FP, timer, pending-interrupt, and other vCPU-affine HVF state | Captured and restored by a dedicated serialized command on the vCPU runner thread. |
| HVF GIC state | Opaque device-only bytes are captured by a serialized runner command under the current single-vCPU stopped boundary. vCPU-affine CPU-interface registers remain a separate runner-owned inventory and must not be read directly by the process owner. |
| Guest memory and MMIO-device state | Inspected or copied on the boot worker while it holds the lease. |
| Limiter deadlines and other auxiliary scheduler state | Quiesced through an acknowledged handshake coordinated by the boot worker; the scheduler's own state owner supplies any captured fields. |
| API transaction and detached captured-state bundle | Coordinated by the process owner only after snapshot readiness is acknowledged. It may own an immutable captured bundle, but never the live boot session or runner-owned HVF handles. |
| vmnet, vsock, disks, and other host resources | Represented by explicit configuration or restore metadata according to later resource policy, not by serializing live host handles. |

The native-v1 baseline register inventory, GIC/device payload schemas, capture
ownership, and lease duration through synchronous memory output are now fixed
by the composite capture. #1395 and #1396 subsequently complete shared public
dirty epochs. Optional resources and optional-resource policy remain separate
design decisions.
The internal process owner now composes the independently implemented publisher
and capture through one close-proven staging writer; restore
consumes the resulting committed artifacts.

### Failure and terminal precedence

- Preparation or capture failure must cancel lease-owned work, restore every
  successfully quiesced scheduler and admission gate, and return to coherent
  ordinary `Paused` behavior before reporting a recoverable error. If rollback
  cannot establish that boundary, the worker must become terminal rather than
  claim ordinary pause or snapshot readiness.
- Resume cannot start a guest run while preparing, snapshot-ready, or releasing.
  It must first cancel or finish capture and receive the exactly-once lease
  release acknowledgement, then use the existing paused-to-running transition.
- Process shutdown takes precedence over preparation and capture. It cancels
  lease work through an out-of-band control path, rather than queueing behind
  that work or relying on a blocked API requester. It prevents a later readiness
  acknowledgement and leaves the existing session owner responsible for
  stopping schedulers, shutting down the runner, destroying the VM, and joining
  the worker exactly once.
- A guest terminal outcome or worker failure that wins the race before pause or
  readiness acknowledgement invalidates the request. The process owner must not
  commit a stale state transition, and existing terminal process behavior
  remains authoritative.

## Remaining Expansion Prerequisites

The supported baseline is complete. Broader snapshot support still requires:

- explicit external-resource and override policy for every profile beyond one
  read-only root block device and default serial, plus optional-device state;
- differential image serialization, merge, and restore policy before `Diff`
  can be admitted; and
- compatible capture/restore and signed acceptance coverage for each expanded
  profile.

The detailed list below is the pre-composite prerequisite inventory retained to
show why the baseline was chosen. Its capture/schema gaps are superseded by
#1270, and its baseline destination-validation/restore-orchestration gaps by
#1272. Optional-state expansion and external resources remain relevant; #1276
supplies the public routing and signed baseline proof, while #1395 and #1396
complete public shared dirty epochs without admitting `Diff` artifacts.

- Snapshot-ready pause ownership: extend the implemented supervisor admission
  foundation to satisfy every invariant above without racing the HVF runner,
  process-owner mutations, auxiliary wakeups, or terminal teardown.
- Captured-memory ownership: the file model and publisher can serialize an
  already-owned `GuestMemory`, but orchestration still needs an immutable
  snapshot-ready memory owner held for the complete copy boundary.
- HVF vCPU state capture: X0-X30, PC, and CPSR; raw SP_EL0, SP_EL1, ELR_EL1,
  and SPSR_EL1; raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and
  VBAR_EL1; raw ACTLR_EL1 and CPACR_EL1; raw CSSELR_EL1 cache selection; every
  DFR0-reported raw DBGBVR/DBGBCR hardware-breakpoint pair; every DFR0-reported
  raw DBGWVR/DBGWCR hardware-watchpoint pair;
  guest-visible MIDR, MPIDR, PFR0/1, DFR0/1, ISAR0/1, and MMFR0/1/2
  baseline compatibility metadata; optional macOS 15.2 ZFR0/SMFR0 SVE/SME
  compatibility metadata; mutable macOS 15.2 `PSTATE.SM`/`PSTATE.ZA` controls;
  conditional maximum-width macOS 15.2 streaming Z0-Z31 bytes;
  conditional maximum-derived macOS 15.2 streaming P0-P15 predicate bytes;
  conditional maximum-SVL-square macOS 15.2 ZA matrix bytes;
  raw macOS 15.2 `SMCR_EL1`, `SMPRI_EL1`, and `TPIDR2_EL0` state;
  raw macOS 15.2 `SCXTNUM_EL0` and `SCXTNUM_EL1` software context numbers;
  raw MDCCINT_EL1 and MDSCR_EL1 debug controls; raw
  Hypervisor.framework debug-exception and debug-register-access trap policy;
  raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1; baseline Q0-Q31, FPCR, and FPSR; raw
  SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and
  CONTEXTIDR_EL1; raw APIA, APIB, APDA, APDB, and APGA pointer-authentication
  keys; raw physical timer CNTKCTL, control, CVAL, and TVAL values; raw virtual
  timer mask, offset, control, and CVAL values; and CPU-level IRQ/FIQ pending
  values have owner-thread capture subsets.
  General, core-system, exception, execution-control, cache-selection,
  debug-control, debug-trap policy, thread-context, translation, system-context, baseline
  SIMD/FP, and pointer-authentication key values also have isolated low-level
  owner-thread restore operations. #1261 additionally supplies normalized
  physical/virtual timer capture and never-run restore with a freeze-downtime
  policy, plus a fail-closed inactive SVE/SME/debug classifier. CPU-level
  IRQ/FIQ pending values have a separate paired restore
  under generalized interrupt admission. None has snapshot validation or
  orchestration.
  Identification metadata still needs masks and destination compatibility
  policy and is not mutable state to restore.
  SME PSTATE capture still needs maximum-SVL and feature validation plus
  destructive transition ordering with Z/P/FPSR and conditional ZA/ZT0 data;
  its raw flags must not be treated as safe restore input.
  SME Z-register capture still needs effective-SVL and feature/destination
  validation, protected persistence, byte-layout and zeroization policy, and
  coordinated transition/restore ordering with P/FPSR and conditional ZA/ZT0;
  its raw bytes must not be treated as safe restore input.
  SME P-register capture still needs effective-SVL and feature/destination
  validation, protected persistence, byte-layout, inactive-lane, and zeroization
  policy, and coordinated transition/restore ordering with Z/FPSR and
  conditional ZA/ZT0; its raw bytes must not be treated as safe restore input.
  SME ZA-register capture still needs effective-SVL and feature/destination
  validation, protected persistence, byte-layout and zeroization policy, and
  coordinated transition/restore ordering with Z/P/FPSR and conditional ZT0;
  its raw bytes must not be treated as safe restore input.
  SME ZT0-register capture still needs SME2 feature/destination validation,
  protected persistence, lane and zeroization policy, and coordinated
  transition/restore ordering with Z/P/ZA/FPSR; its raw bytes must not be
  treated as safe restore input.
  SME system-register capture still needs feature and writable-bit validation,
  maximum-SVL policy, protected persistence for sensitive `TPIDR2_EL0`, and
  ordered restore with PSTATE plus conditional Z/P/ZA/ZT0 data; its raw values
  must not be treated as safe restore input.
  System-context capture-order apply still needs interpretation, feature and
  destination validation, protected persistence, rollback, and coordinated
  ordering with TPIDR and `CONTEXTIDR_EL1` state; its raw values must not be
  treated as validated snapshot restore input.
  Cache-selection capture-order apply still needs selector interpretation and
  validation, an atomic destination cache feature/geometry manifest,
  ISB/dependent CCSIDR visibility, maintenance, protected persistence,
  rollback, and schema; its raw value must not be treated as validated cache
  restore input.
  Hardware-breakpoint and hardware-watchpoint capture still need control-bit
  and destination-count validation, protected persistence, host trap
  coordination, and ordered restore. Debug-control capture/apply and host debug-trap
  capture/apply remain separate and lack joint feature/writable-bit validation,
  security/destination policy, and composite ordering; raw comparator,
  MDCCINT/MDSCR, and host trap values must not be treated as a complete safe
  debug restore input.
  The standalone default-configuration CTR_EL0/CLIDR_EL1/DCZID_EL0 metadata and
  instruction/data CCSIDR geometry remain independent queries and do not form
  one atomic manifest with the live selector. #1392's separate retained startup
  source validates guest FDT presentation, but still does not make mutable
  selector restore safe.
  Remaining system registers and other
  optional architecture state still need a full inventory. Raw timer values
  remain observation-only, while the separate normalized policy strips derived
  ISTATUS, ignores TVAL, and adjusts host-relative offset/CVAL at restore;
  timer-PPI delivery and EOI behavior remain part of GIC/run-loop composition;
  pointer-authentication key restore still needs feature validation, protected
  persistence, zeroization, and safe SCTLR enable ordering; and every remaining
  captured field still needs a restore path on the owning thread. The eight
  general-register, core-system, exception-register, execution-control,
  thread-context, translation, baseline SIMD/FP, and pointer-authentication
  primitives already supply only their isolated,
  nontransactional owner-thread write sequences; none is snapshot validation,
  wider ordering, rollback, feature/MMU/streaming transition, dependent-memory
  or maintenance coordination, or load orchestration.
- Interrupt-controller state: #1178 captures Apple's stable, versioned opaque
  GIC device blob except CPU system registers, #1255 adds its isolated
  pre-first-run owner-thread apply, #1180 captures all ten EL1 ICC registers,
  and #1258 restores the nine mutable ICC registers while validating derived
  RPR. `ICC_SRE_EL2`, ICH/ICV inventory, destination validation, compatible
  composite orchestration, host-update preflight, multi-vCPU association, a
  cross-step no-run lease, and a bangbang schema remain required before
  interrupt delivery can be considered restorable.
- Device-state persistence: every implemented device needs a stable serialized
  state model, restore validation, and rollback or terminal-failure behavior.
- Dirty tracking decision (completed by #1395/#1396): shared HVF and userspace
  epochs support Full commit reset, while diff snapshots still need explicit
  image serialization, merge, and restore semantics.
- Data-format decision: bangbang must choose between Firecracker file-format
  compatibility, a bangbang-native format behind Firecracker-shaped APIs, or a
  documented unsupported boundary.
- Security policy: snapshot paths, memory contents, restored CPU state, and
  restored device state must be treated as untrusted input and must preserve the
  existing host-path redaction policy.

## Implementation Split

Snapshot-ready ownership should land as ordered, PR-sized slices before a
snapshot create success path. Each slice must preserve recognized unsupported
API behavior until all of its prerequisites exist. Rows describe the boundary
when each slice landed; later rows supersede earlier deferred-work clauses.

| Slice | Scope | Minimum validation |
| --- | --- | --- |
| Stable paused arm64 topology capture/import (wire-format-neutral foundation implemented) | #1567 adds a checked `1..=32` topology graph with canonical index/MPIDR identity, virtual-timer PPI, offline/runnable dispositions, and redacted CPU_SUSPEND32/64 X1-X3/post-trap-PC continuations. Capture requires a completed topology pause and cross-validates coordinator, PSCI, session, runner, HVC, and owner registers before publication. Import prevalidates before mutation, allocates fresh destination tokens, constructs the coordinator born Paused, installs suspended members in order, and dispatches nothing until explicit resume generation 1. Failure aborts installed calls in reverse order, clears dispatch state, retains cleanup evidence, and consumes the unpublished topology. Native-v1/native-v2 bytes and public snapshot reconstruction remain unchanged. | Empty/maximum/oversized/canonicality/PPI/primary-offline/PC-alignment boundaries; both suspend conventions and exact register checks; power/runner token inequality; paused admission and no pre-resume dispatch; offline/runnable/suspended import and equivalent recapture; reverse rollback, shutdown, redaction, and existing pause/resume/timer-wake lifecycle coverage. |
| Unpublished native-v2 multi-vCPU platform capture/reconstruction (implemented) | #1569 composes the `2.2.0` graph from one completed paused source, existing memory binding, inert boot metadata, retained machine/CPU facts, singular global GIC, and canonical per-vCPU owner captures. Restore preflights supplied memory/FDT/cache before HVF, creates the complete never-run destination, replays retained CPU targets, restores global then per-vCPU state, imports fresh lifecycle tokens, and publishes only one focused owner born Paused. It opens no recorded path and intentionally excludes public actions, devices, and time/identity correction. | Every restore-stage failure and reverse cleanup sequence; CPU-receipt replay/drift/read/apply failures; source reuse and redaction; strict Clippy/unit gates; signed three-vCPU runnable/suspended/offline capture, encode/decode, fresh restore, no-early-progress recapture, timer-PPI/PSCI completion, offline CPU_ON continuation, final recapture, and clean shutdown. |
| Native-v2 time and clone identity (implemented) | #1529 advances the writer to `2.3.0` and appends singleton kind 6 after every vCPU. It carries only portable PL031/PVTime/VMGenID/VMClock state and four closed policies: destination-SystemTime RTC reset, cumulative stolen time without downtime, fresh notified VMGenID, and saved-counter notified VMClock. Source capture validates guest and retained-owner agreement before creating a fresh memory binding. Restore preflights all guest destinations, installs PL031/PVTime, signals VMGenID then VMClock, imports lifecycle state, and publishes Paused; any failure after the first committed identity write is terminal. Public actions and general devices remain excluded. | Fixed schema fixtures and hostile time-policy/count/layout/ABI/cross-component mutations; source capture ordering and reusable failure; all RTC/PVTime/identity restore stages and commit boundary; exact aarch64 `clock_realtime` rejection tied to pinned Firecracker sources; signed three-vCPU repeat load, distinct clone IDs, saved-counter transitions, guest-observed notification order and time values, recapture-to-restore, no early progress, continuation, and cleanup. |
| Private paused-process native-v2 publication (implemented) | #1576 admits only a minimal `Paused` `Full` production source, proves default reset-compatible UART state from the live validated model before staging, derives inert boot metadata without path reopen, and composes topology pause, cancellable memory streaming, exact 2.3 encoding, source recovery, commit seal, and post-publication dirty-epoch handling in one supervisor command. It reuses direct/contained native-family outputs while leaving public create native-v1. | Exhaustive controller-profile rejection, real-model UART comparison, direct/output publication, collision and staging cleanup, cancellation/retry, topology/recovery/panic/post-commit terminal paths, repeat loader validation, signed three-vCPU cancellation/recovery/recapture, and a separately signed two-vCPU private process publish/resume/repause/recapture proof. |
| Private paused-process native-v2 restoration (implemented) | #1577 admits only pristine File/COW destinations, classifies direct or contained state before resource adoption, retains decoded machine facts and inert boot metadata, validates the exact default arm64 FDT/UART/RTC/time shell, and commits a closed focused supervisor plus controller into the normal process lifecycle initially Paused. Fresh buffered or stdout-only serial never inherits source bytes or opens stdin; requested resume uses the ordinary action gate. Pre-adoption failures are retryable, while owner, commit, or ambiguous-cleanup failures share the terminal construction latch with boot and native-v1 restore. Public load remains native-v1. | State-first family and descriptor/profile rejection; exact hostile FDT node/range/interrupt/identity checks; fresh serial and controller adoption; session/commit/cleanup/terminal-latch faults; repeated immutable File/COW loads; and a signed two-vCPU source recapture followed by paused and resume-requested restore into two fresh normal processes with lifecycle and clean-shutdown proof. |
| Public native-v2 Full/File activation (implemented) | #1578 routes public paused `Full` create to the then-current `2.3.0` producer and performs one-open family dispatch on load: frozen native-v1 retains File/Uffd compatibility, native-v2 retains File/COW, and pinned Firecracker or unknown bytes reject without fallback. Both families publish Paused before optional resume; v2 Uffd, Diff, custom or mutated serial, drives/devices/MMDS/PCI/boot timer, overrides, editing, and broad portability remain fail-closed. At that slice the CLI reported `v2.3.0` and described the exact validated v1/v2 version. | Runtime/VMM/API/process family, hostile-input, collision/cancellation, observability, redaction, retry/terminal, and immutable-pair tests; signed frozen-v1 File dispatch; signed public two-vCPU direct v2 Paused/explicit+automatic resume, private COW writes, repeated isolated destinations, artifact immutability, Uffd rejection, and real closed-stdout terminal cleanup; signed App Sandbox CLI plus production state/memory grant, retained-descriptor, staging-recovery, and launcher/worker lifecycle evidence. |
| Native-v2 2.4 root activation (implemented compatibility profile) | #1589 advanced only the then-current writer to `2.4.0`, preserved exact device-free 2.3 compatibility, and admitted one read-only File/Sync root over MMIO or PCI. The mandatory device graph binds configuration, block runtime, common virtio, and transport state to the same state/memory candidate. Public load validates the complete graph, authorizes the inert root selector at the destination, keeps the backing lease provisional through owner construction, and commits it only with the Paused session/controller handoff. Exact 2.4 remains readable with those unchanged limits. | Exact 2.3/2.4 version and graph boundary fixtures; hostile graph/candidate/transport/backing mutations; MMIO/PCI public create/load and controller-handoff fault injection; collision, redaction, retry, terminal, and lease cleanup tests; signed direct two-vCPU root create/restore and signed production root-grant identity across replacement, explicit/automatic resume, immutable artifacts, and cleanup. |
| Native-v2 2.5 regular-file multi-block activation and certification (implemented compatibility profile) | #1616 advanced the then-current writer to exact `2.5.0` profile 2 with rooted or rootless ordered vectors of 1–64 regular-file block devices over MMIO or the product PCI budget. Mixed RO/RW, Sync/Async, Unsafe/Writeback, partuuid, limiter/retry, queue, interrupt, and transport state bind to the same immutable state/memory pair. Load derives one exact keyed complete-set transaction with no fallback or per-drive override and commits every lease/fresh Async generation with the Paused session. File/COW memory is private; externally managed writable drive bytes are deliberately shared and require operator serialization. Vhost-user and optional devices remain excluded from exact 2.5. | #1617's finite create/load stage matrix; hostile profile/graph/state-memory/authority/geometry/transport/Async/controller/completion/cancellation/cleanup injections; exact retryable/terminal/redaction checks; signed direct and normal-production rooted/rootless × MMIO/PCI deterministic all-drive pre/post-capture persistence, limiter/interrupt progress, recapture, fresh destination ownership, explicit/automatic resume, immutable state/memory, shared writable epochs, retained grants, collision/replacement safety, and both worker/launcher death orders. |
| Native-v2 2.6 regular-file block-and-pmem activation and certification (implemented compatibility profile) | #1634 advanced the then-current writer to exact `2.6.0` profile 3 with 1–64 ordered block/pmem records, rooted or rootless across the storage classes. It persists exact configuration/runtime/limiter/queue/interrupt/mapping and MMIO/PCI transport state, then resolves one direct or contained keyed complete-set transaction without ambient fallback. State/memory and File/COW RAM remain immutable/private; writable external block and pmem prefixes deliberately share bytes, while each fresh pmem private tail starts zero. | Fixed profile-3 fixtures; hostile missing/extra/swapped/aliased/role/kind/access/length/geometry/cancellation/construction/controller/completion/cleanup tests; signed direct and normal-production rooted pmem-only/rootless mixed × MMIO/PCI pre/post-capture persistence, read-only protection, limiter/interrupt progress, recapture, explicit/automatic resume, immutable-pair reuse, shared writable epochs, fresh zero DAX tails, exact grants, replacement safety, and worker/launcher death cleanup. |
| Native-v2 2.7 serial activation and certification (implemented current profile) | #1651 added mandatory complete serial state with optional unchanged profile-3 storage; #1652 certifies fresh default/configured endpoint reconstruction, exact UART registers/RX/status/pending work, serial-only and MMIO/PCI-storage products, complete direct/contained authority, repeated immutable loads, recapture, and destination-local limiter/metrics/terminal/FIFO policy. | Exact codec/cross-graph/resource/fault/compatibility fixtures; private signed HVF reconstruction; signed direct bare-arm64 default stdio and configured regular-file/FIFO continuation; signed normal production/App Sandbox default-pipe and configured write-only-grant continuation with pathname replacement, source-only byte exclusion, redaction, and cleanup. |
| Supervisor lease and admission (foundation implemented) | #1160 adds atomic admission/FIFO ordering, worker-side pause revalidation, one scoped lease-owned operation, normal-command rejection, structured release, and out-of-band shutdown invalidation. Real capture work and admission across the remaining owners are deferred. | Supervisor and `ProcessVmm` unit tests plus API/process pause-state tests. |
| Auxiliary quiescence and complete publication transaction (implemented for native-v1 baseline) | #1162 introduced acknowledged RAII quiescence for block and entropy; #1389 added the topology-wide SMP pause barrier and PMEM guard; #1390 includes network, acquires all four failure-atomically, drains tokens only after complete acknowledgement, preserves in-flight/deferred/deadline work, and holds the worker lease through commit plus the post-publication hook. Process API/MMDS/controller and periodic work are serialized by the synchronous owner borrow. | Deterministic scheduler, supervisor, cancellation/seal, publication-visibility, process/API serialization, and fresh-retry tests plus combined signed SMP pause and one-vCPU baseline publication evidence. |
| Complete dirty epochs and public tracking (implemented) | #1395 supplies fail-closed HVF protection/fault retry. #1396 adds the shared `GuestMemory` bitmap, exact initial/reprotected DFSC `0x07`/`0x0f` ownership checks, every current bounded host/device writer, conservative discard, protected wholly-dirty dynamic RAM, destination load ordering, and post-visible-Full reset/rollback/poison semantics. Machine and load tracking flags are enabled without adding Diff artifacts. | Exact/repeated/concurrent host and CPU union, discard, dynamic mapping, load override/VMGenID, publication/cancellation/reset failures, and public transaction tests plus signed normal boot/load, two-vCPU current-device, and two-epoch exact-set evidence. |
| Runner general-register capture and restore (first bidirectional subset implemented) | #1164 adds a typed immutable X0-X30, PC, and CPSR value plus one failure-atomic owner-thread capture. #1228 adds ordered owner-thread restore of that complete typed value and generalizes the shared admission name from capture to operation. Hypervisor.framework does not make the 33 writes transactional: typed failure context identifies the failed register and completed prefix, and callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore, but the snapshot lease invokes neither. Core system, exception, execution-control, identification, translation, baseline SIMD/FP, schema, validation, rollback, wider ordering, and multi-vCPU coordination remain separate or deferred. | Exact 33-field read/write order; every read and write failure; typed partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed same-vCPU idle capture/restore/recapture without guest execution or value logging. |
| Runner core system-register capture and restore (second bidirectional subset implemented) | #1170 adds a typed immutable raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 value plus one owner-thread capture. #1230 adds ordered owner-thread restore of that complete value and a reusable typed system-register failure with the exact failed register and completed prefix. Hypervisor.framework does not make the four writes transactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Exception, execution-control, identification, translation, broader system state, validation, schema, rollback, wider ordering, orchestration, and multi-vCPU coordination remain separate or deferred. | Exact four-field read/write order; every read and write failure; typed partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed guest-written known-value capture/restore/recapture without post-restore guest execution or value logging. |
| Runner EL1 exception-register capture and restore (third bidirectional subset implemented) | #1184 adds typed immutable raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 state plus one owner-thread capture. #1232 adds ordered owner-thread restore of that complete value through the reusable typed system-register failure with the exact failed register and completed prefix. Hypervisor.framework does not make the six writes transactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Vector-table memory, coherent exception semantics, destination validation, persistence, schema, rollback, wider ordering, orchestration, and multi-vCPU coordination remain deferred. | Exact six-field read/write order; every read and write failure; typed partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed guest-written capture/restore/recapture preserving implementation-defined AFSR readback without post-restore guest execution or value logging. |
| Runner EL1 execution-control capture and restore (fourth bidirectional subset implemented) | #1186 adds typed immutable raw ACTLR_EL1 and CPACR_EL1 state plus one owner-thread capture. #1234 adds ordered owner-thread restore of that complete value through the reusable typed system-register failure with the exact failed register and completed prefix. Complete capture and restore require macOS 15 because Hypervisor.framework exposes only ACTLR_EL1.EnTSO there. The two writes are nontransactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. CPACR optional-feature and destination validation, writable-bit policy, guest ISB transitions, wider feature-state ordering, persistence, schema, rollback, orchestration, and multi-vCPU coordination remain deferred. | Exact ACTLR-then-CPACR read/write order; both read and write failures; typed partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed EnTSO/FPEN capture/restore/recapture without post-restore guest execution or value logging. |
| Arm64 guest cache FDT and retained startup identity (implemented) | #1392 reads MMFR2 plus the cache feature/geometry manifest from one default configuration, decodes active legacy/CCIDX levels, uniquely reconciles sizes and sharing with public macOS performance-level facts, and fails before VM creation on incomplete or conflicting evidence. It publishes a validated nested L1/L2/L3 FDT graph for up to 32 guest vCPUs, retains the exact source for ordinary native-v1 capture, and cross-checks runner MMFR2 without changing native-v1 bytes or schema. A restored session reconstructs compatibility source only and reports no retained FDT hierarchy. Cross-host portability, mutable selector synchronization, cache maintenance, writable CPU feature views, and cache-presentation snapshot schema remain deferred. | Legacy/CCIDX decode and malformed-field tests; injected sysctl match/missing/mismatch/ambiguity/sharing tests plus real width/absence boundary; parsed one-/six-/ten-vCPU FDT graph tests; pre-VM failure ordering and debug/error redaction; retained-manifest/MMFR2 capture tests; and signed two-vCPU Linux sysfs equality against the retained production hierarchy. |
| Default arm64 vCPU cache feature configuration (raw prerequisite implemented) | #1216 adds a typed immutable raw CTR_EL0/CLIDR_EL1/DCZID_EL0 value queried from a fresh default retained vCPU configuration before VM creation. This standalone diagnostic remains outside backend instance state, VM/vCPU ownership, runner admission, boot sessions, and snapshot orchestration. CCSIDR geometry is queried separately here; #1392 interprets a distinct combined startup source, while this surface still defines no destination policy, persistence, schema, or restore behavior. | Exact macOS 11+ object/feature APIs and ids; null creation, CTR-then-CLIDR-then-DCZID order, arbitrary values, all getter failures, success/error/unwind release, target behavior, accessors, and signed same-host pre-VM stability without raw logging or cache operations. |
| Default arm64 vCPU CCSIDR geometry (raw prerequisite implemented) | #1218 adds a separate typed immutable pair of eight-entry raw data/unified and instruction CCSIDR arrays queried from its own fresh retained default vCPU configuration before VM creation. This standalone diagnostic remains outside backend instance state, VM/vCPU ownership, runner admission, boot sessions, and snapshot orchestration, and is not atomic with #1216. #1392 interprets a distinct combined startup source; this surface still defines no implemented-level selection, destination policy, persistence, schema, or restore behavior. | Exact macOS 11+ object/CCSIDR API and cache types; null creation, data-then-instruction order, all sixteen arbitrary values, both getter failures, success/error/unwind release, target behavior, accessors, and signed same-host pre-VM stability without raw logging or live cache operations. |
| Runner EL1 cache-selection capture and restore (tenth bidirectional subset implemented) | #1196 adds typed immutable raw CSSELR_EL1 state plus one failure-atomic owner-thread capture. #1246 adds one owner-thread write of that complete value through the reusable value-free system-register failure with the exact register and zero completed writes. Callers must retry the complete value or discard the vCPU after failure. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. The standalone default-configuration feature/geometry queries remain non-atomic; #1392's separate retained startup source validates the FDT but does not define mutable selector interpretation, ISB/dependent CCSIDR visibility, cache maintenance, protected persistence, rollback, orchestration, schema, or multi-vCPU selector association. | Exact stable SDK id and one-register read/write order; read failure and fresh retry; write failure with typed value-free zero-prefix context and complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed idle same-vCPU capture/restore/recapture twice without selector logging, CCSIDR queries, ISB, maintenance, guest execution, reset assumptions, topology inference, or destination claims. |
| Runner EL1 hardware-breakpoint capture (raw subset implemented) | #1198 adds a typed immutable implemented count plus raw DBGBVR/DBGBCR prefixes, bounded indexed mappings for all sixteen SDK slots, and one getter-only, failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease or changing debug behavior. Watchpoints and host trap state are captured separately; control-bit validation, protected persistence, schema, restore, and multi-vCPU association remain deferred. | Exact indexed SDK ids; DFR0-first count policy; deterministic pair order, every failure point and fresh retry, thirty-four-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed idle-vCPU shape capture without writes, debug activation, trap changes, guest instructions, or guest execution. |
| Runner EL1 hardware-watchpoint capture (raw subset implemented) | #1200 adds a typed immutable implemented count plus raw DBGWVR/DBGWCR prefixes, bounded indexed mappings for all sixteen SDK slots, and one getter-only, failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease or changing debug behavior. Breakpoints and host trap state are captured separately; control-bit validation, protected persistence, schema, restore, and multi-vCPU association remain deferred. | Exact indexed SDK ids; DFR0-first count policy; deterministic pair order, every failure point and fresh retry, thirty-four-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed idle-vCPU shape capture without raw logging, writes, debug activation, trap changes, guest instructions, or guest execution. |
| Runner EL1 debug-control capture and restore (twelfth bidirectional core subset implemented) | #1194 adds typed immutable raw MDCCINT_EL1 and MDSCR_EL1 state plus one failure-atomic owner-thread capture. #1252 adds ordered owner-thread restore of that complete value through the reusable value-free system-register failure with the exact failed register and completed prefix. The two writes are nontransactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Breakpoint/watchpoint comparators and host trap state remain separate; feature/writable-bit and destination policy, wider ordering, persistence, rollback, orchestration, schema, and multi-vCPU association remain deferred. | Exact stable SDK ids and MDCCINT-then-MDSCR read/write order; both read and write failures; typed value-free partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed original-value restore/recapture twice without register assumptions or logging, manufactured changes, adjacent debug mutation, guest instructions, or guest execution. |
| Runner arm64 debug-trap policy capture and restore (eleventh bidirectional core subset implemented) | #1202 adds a typed immutable pair of Hypervisor.framework debug-exception and debug-register-access trap booleans plus one failure-atomic owner-thread capture. #1250 adds ordered owner-thread restore of that complete value through a dedicated value-free failure with the exact failed host-policy operation and completed prefix. The two writes are nontransactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Guest MDCCINT/MDSCR and comparator state remain separate; joint feature/security and destination policy, wider ordering, persistence, rollback, orchestration, schema, and multi-vCPU association remain deferred. | Exact macOS 11+ owner-thread getter/setter names; exception-then-register-access read/write order; all Boolean combinations; both read and write failures; typed value-free partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed original-value restore/recapture twice without Boolean assumptions or logging, guest debug mutation, guest instructions, or guest execution. |
| Runner identification-register capture (compatibility metadata implemented) | #1192 adds typed immutable guest-visible MIDR, MPIDR, PFR0/1, DFR0/1, ISAR0/1, and MMFR0/1/2 baseline metadata plus one failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease. Optional SVE/SME IDs are captured separately; beta-only newer IDs, broader configuration-time manifests, feature masks, destination policy, persistence, schema, and multi-vCPU association remain deferred. | Exact eleven stable SDK ids; deterministic order, every failure point and retry, thirty-four-way core-operation conflicts plus standalone metadata-getter exclusion, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed same-vCPU stability/MPIDR comparison without model constants. |
| Runner SVE/SME identification-register capture (optional compatibility metadata implemented) | #1204 adds a separate typed immutable raw ZFR0/SMFR0 value plus one macOS 15.2+ failure-atomic owner-thread command in the shared core-register admission domain. The baseline identification value remains unchanged, and both boot-session forms expose the optional capture without involving the snapshot lease. SME PSTATE is captured separately; broader configuration-time manifests, masks, destination policy, streaming data, persistence, schema, restore, and multi-vCPU association remain deferred. | Exact two stable SDK ids and availability; ZFR0-then-SMFR0 order, both failure points and fresh retry, thirty-four-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed same-vCPU stability without model constants, feature enablement, streaming mode, state reads, or guest execution. |
| SME maximum-SVL configuration query (buffer-sizing prerequisite implemented) | #1214 adds one runtime-resolved macOS 15.2+ no-handle query and a typed immutable maximum guest-usable SVL byte length. It remains outside backend instance state, VM/vCPU ownership, runner admission, boot sessions, and snapshot orchestration; #1220 consumes it as an exact per-Z allocation width, #1222 as the basis for each `maximum / 8` P-register width, and #1224 as both dimensions of the checked-square ZA allocation. Z/P require a live-vCPU streaming-mode preflight, whereas ZA requires its storage-enable preflight. ZT0 is independent of maximum SVL; effective SVL, feature/destination policy, persistence, schema, and restore remain deferred. | Exact C ABI and symbol/return behavior; full-width `size_t` preservation, missing-symbol and non-target boundaries, exact `HV_UNSUPPORTED`, typed value/accessor coverage, and a signed double query before VM creation without raw logging or SME state/data operations. |
| Runner SME PSTATE capture (raw subset implemented) | #1206 adds a separate typed immutable `PSTATE.SM`/`PSTATE.ZA` value plus one runtime-resolved macOS 15.2+ getter-only, failure-atomic owner-thread command in the shared core-register admission domain. Both boot-session forms expose it without involving the snapshot lease or calling the setter. Maximum SVL, Z0-Z31, P0-P15, ZA, and ZT0 are captured separately; feature validation, transition ordering, persistence, schema, restore, and multi-vCPU association remain deferred. | Exact C ABI layout and symbol/return behavior; all Boolean combinations, backend failure and fresh retry, thirty-four-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, and signed idle-vCPU observation or exact `HV_UNSUPPORTED` without logging, setters, state changes, SME data reads, guest instructions, or guest execution. |
| Runner SME Z-register capture (conditional raw subset implemented) | #1220 adds a runtime-resolved macOS 15.2+ getter-only command that preflights `PSTATE.SM`, queries maximum SVL, fallibly allocates one contiguous buffer, and publishes exact maximum-width Z0-Z31 slices only after every owner-thread read succeeds. `Debug` redacts the complete buffer, both boot-session forms expose it, and the snapshot lease does not invoke it. P0-P15, ZA, and ZT0 are captured separately; effective SVL, setters/transitions, feature/destination policy, layout conversion, protected persistence, schema, restore ordering, orchestration, and multi-vCPU association remain deferred. | Exact SDK ids/C ABI and availability; inactive/zero/overflow/allocation failures; deterministic 32-read order, every getter failure and fresh retry, bounded accessors, redaction, thirty-four-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed unavailable/inactive or two complete idle captures without raw logging, setters, state changes, guest instructions, or guest execution. |
| Runner SME P-register capture (conditional raw subset implemented) | #1222 adds a runtime-resolved macOS 15.2+ getter-only command that preflights `PSTATE.SM`, queries and validates maximum SVL, fallibly allocates one contiguous buffer, and publishes exact `maximum / 8`-byte P0-P15 slices only after every owner-thread read succeeds. `Debug` redacts the complete buffer, both boot-session forms expose it, and the snapshot lease does not invoke it. Z0-Z31, ZA, and ZT0 are captured separately; effective SVL, setters/transitions, feature/destination policy, layout and inactive-lane interpretation, protected persistence, schema, restore ordering, orchestration, and multi-vCPU association remain deferred. | Exact SDK ids/C ABI and availability; inactive/zero/divisibility/overflow/allocation failures; deterministic 16-read order, every getter failure and fresh retry, bounded accessors, redaction, thirty-four-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed unavailable/inactive or two complete idle captures without raw logging, setters, state changes, guest instructions, or guest execution. |
| Runner SME ZA-register capture (conditional raw subset implemented) | #1224 adds a runtime-resolved macOS 15.2+ getter-only command that preflights `PSTATE.ZA` without requiring `PSTATE.SM`, queries a non-zero maximum SVL, checked-squares it, fallibly allocates the exact buffer, and publishes the complete raw matrix only after the owner-thread getter succeeds. `Debug` redacts bytes and dimensions, both boot-session forms expose it, and the snapshot lease does not invoke it. Z/P/ZT0 are captured separately; effective SVL, setters/transitions, feature/destination policy, layout interpretation, protected persistence, schema, restore ordering, orchestration, and multi-vCPU association remain deferred. | Exact C ABI and availability; both streaming-mode values under active/inactive ZA; zero/overflow/allocation failures; exact bytes, backend failure and fresh retry, raw accessors, redaction, thirty-four-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed unavailable/inactive or two complete idle captures without raw logging, setters, state changes, guest instructions, or guest execution. |
| Runner SME2 ZT0-register capture (conditional raw subset implemented) | #1226 adds a runtime-resolved macOS 15.2+ getter-only command that preflights `PSTATE.ZA` without requiring `PSTATE.SM`, then performs one fixed 64-byte read through a private 16-byte-aligned SDK-compatible value without querying maximum SVL. The detached state is published only after success, redacts every byte from `Debug`, and is exposed by both boot-session forms without involving the snapshot lease. Z/P/ZA are captured separately; setters/transitions, SME2 feature/destination policy, lane interpretation, protected persistence, schema, restore ordering, orchestration, and multi-vCPU association remain deferred. | Exact SDK C ABI, 64-byte size and 16-byte alignment, missing-symbol/present-symbol behavior, both streaming-mode values under active/inactive ZA, exact bytes, backend failure and fresh retry, fixed-size accessor, redaction, thirty-four-way conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, and signed unavailable/inactive or two complete idle captures without raw logging, setters, state changes, maximum-SVL queries, guest instructions, or guest execution. |
| Runner SME system-register capture (raw subset implemented) | #1208 adds a separate typed immutable raw SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 value plus one macOS 15.2+ getter-only, failure-atomic owner-thread command in the shared core-register admission domain. `Debug` redacts every register, and both boot-session forms expose capture without involving the snapshot lease. Maximum SVL, Z0-Z31, P0-P15, ZA, and ZT0 are captured separately; feature and writable-bit validation, persistence, schema, restore ordering, and multi-vCPU association remain deferred. | Exact three stable SDK ids and availability; SMCR-then-SMPRI-then-TPIDR2 order, every failure point and fresh retry, thirty-four-way conflicts, abandonment, command/response channel closure, queued destruction, unwind, panic, shutdown, redacted `Debug`, and signed same-vCPU idle capture without raw logging, writes, maximum-SVL queries, SME data reads, guest instructions, or guest execution. |
| Reviewed optional arm64 state restore (wire-format-neutral foundation implemented) | #1566 adds dynamically resolved public-HVF setters for SME PSTATE, Z/P, ZA, and ZT0 plus one presence-aware, one-attempt, never-run owner aggregate. It validates exact DFR0 counts, SME version/identification/maximum SVL, conditional widths and dependencies, fresh disabled debug controls/PSTATE, destination-local sparse defaults, and every Q/Z low-lane alias before mutation. Breakpoint and watchpoint controls are disabled before values and final controls; SME system registers precede PSTATE and Z/P/ZA/ZT0; Q0-Q31, FPCR, and FPSR are authoritative last writes. Stable redacted errors expose only family/stage/index/completed writes, and any failure permanently prevents execution on that runner. Native-v1 bytes and inactive-optional policy remain unchanged; #1568 later assigns the closed native-v2 tags and #1569 invokes the aggregate in unpublished multi-vCPU reconstruction. | Exact SDK ABI, dynamic-symbol and non-target boundaries; every static destination rejection; sparse/default read selection; every backend read/write failure and partial prefix; one-attempt admission across conflicts, channels, panic, and already-run state; failure-poisoned execution; unsigned unit coverage; and signed same-owner debug/active-SME restore, recapture, alias, transition-default, second-attempt, and cleanup proof. |
| Runner system-context register capture and restore (ninth bidirectional subset implemented) | #1210 adds a separate redacted typed raw SCXTNUM_EL0/SCXTNUM_EL1 value plus one macOS 15.2+ failure-atomic owner-thread capture. #1244 adds ordered owner-thread restore of that complete value through the reusable value-free system-register failure with the exact failed register and completed prefix. The two writes are nontransactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Interpretation, feature/destination validation, protected persistence, wider TPIDR/CONTEXTIDR ordering, rollback, orchestration, schema, and multi-vCPU association remain deferred. | Exact two-register read/write order; every read and write failure; typed value-free partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; redacted `Debug`; and signed idle same-vCPU capture/restore/recapture twice without guest execution, reset assumptions, compatibility inference, or value logging. |
| Runner EL1 translation-register capture and restore (sixth bidirectional subset implemented) | #1182 adds typed immutable raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 state plus one owner-thread capture. #1238 adds ordered owner-thread restore of that complete value through the reusable typed system-register failure with the exact failed register and completed prefix. Hypervisor.framework does not make the seven writes transactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. System-context registers and pointer-authentication keys are captured separately; table memory, feature and destination validation, barriers, TLB/cache maintenance, safe MMU transition ordering, persistence, orchestration, schema, rollback, and multi-vCPU coordination remain deferred. | Exact seven-field read/write order; every read and write failure; typed partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed MMU-off guest-written capture/restore/recapture preserving actual implementation-defined AMAIR readback without post-restore guest execution or value logging. |
| Runner pointer-authentication key capture and restore (eighth bidirectional subset implemented) | #1190 adds a redacted typed value containing five 128-bit APIA, APIB, APDA, APDB, and APGA keys plus one failure-atomic owner-thread capture. #1242 adds ordered owner-thread restore of the complete value through the reusable value-free system-register failure with the exact failed register and completed prefix. The ten writes are nontransactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Feature/algorithm and destination validation, zeroization, protected persistence, safe SCTLR enable ordering, rollback, orchestration, schema, and multi-vCPU association remain deferred. | Exact ten-register ids, low/high pairing, and read/write order; every read and write failure; typed value-free partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; redacted debug; and signed fake-key capture/restore/recapture without PAC execution, post-restore guest execution, or value logging. |
| Runner SIMD/FP capture and restore (seventh bidirectional subset implemented) | #1172 adds typed immutable Q0-Q31, FPCR, and FPSR state plus a 16-byte-aligned getter FFI seam. #1240 adds one target-gated C shim for the SDK's by-value vector setter and ordered owner-thread restore of the complete typed value. The 34 writes are nontransactional; a dedicated typed error distinguishes SIMD/FP and scalar registers and reports the exact completed prefix, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. Maximum-width streaming Z0-Z31 and maximum-derived P0-P15 are captured separately only while `PSTATE.SM` is active; maximum-square ZA and fixed-size ZT0 are captured separately whenever `PSTATE.ZA` is active. Streaming Q/Z alias ordering, feature/destination validation, FPCR/FPSR writable-bit policy, protected persistence/zeroization, rollback, schema, orchestration, and multi-vCPU coordination remain deferred. | Exact 34-field read/write order; C/Rust pointer-to-vector ABI boundary; every read and write failure; mixed-register typed partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed non-streaming guest-written capture/restore/recapture without post-restore guest execution or value logging. |
| Runner thread-context register capture and restore (fifth bidirectional subset implemented) | #1176 adds typed immutable raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 state plus one owner-thread capture. #1236 adds ordered owner-thread restore of that complete value through the reusable typed system-register failure with the exact failed register and completed prefix. Hypervisor.framework does not make the three writes transactional, so callers must retry the complete value or discard the vCPU before execution. Both boot-session forms expose capture and restore under shared core-operation admission, but the snapshot lease invokes neither. TPIDR2 is captured separately with SME system registers, SCXTNUM_EL0/EL1 use the separate system-context value, and CONTEXTIDR_EL1 remains in translation state; address/destination validation, wider context ordering, persistence, schema, rollback, orchestration, and multi-vCPU coordination remain deferred. | Exact three-field read/write order; every read and write failure; typed partial-write context; complete retry; thirty-four-way conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed guest-written capture/restore/recapture without post-restore guest execution or value logging. |
| Runner physical-timer capture (raw subset implemented) | #1188 adds typed immutable raw CNTKCTL_EL1, CNTP_CTL_EL0, and CNTP_CVAL_EL0 state plus one failure-atomic owner-thread command; #1212 extends the same value and command with raw CNTP_TVAL_EL0. It generalizes timer admission so physical capture and every virtual-timer operation reject each other. Both boot-session forms expose capture without involving the snapshot lease. CNTP requires macOS 15 and GIC creation before the vCPU; CVAL/TVAL are separately timed absolute/relative views, and elapsed-time adjustment, writable-bit filtering, interrupt delivery, persistence, orchestration, schema, and restore remain deferred. | Exact SDK ids and availability; deterministic four-field order, every failure point and retry, bidirectional timer conflicts, abandonment, channel, queued destruction, unwind, panic, shutdown, signed disabled/masked guest-written capture, and signed idle TVAL observation without raw-value or stability assumptions. |
| Runner virtual-timer capture (raw subset implemented) | #1166 adds typed immutable mask/offset state and #1168 extends it with raw control/CVAL values. Timer-specific owner-thread get/set commands and one serialized four-field capture share generalized timer admission with physical-timer capture. Both boot-session forms expose capture, but the snapshot lease does not invoke it. CPU pending levels, the opaque GIC device blob, and EL1 ICC state are captured separately; restore-time offset/control policy, orchestration, and restore remain deferred. | Deterministic four-field order, conflict, abandon, channel, panic, and retry tests plus signed known-value capture that safely restores the original stable values and writable control bits. |
| Native arm64 timer and VMGenID restore policy (internal primitives implemented) | #1261 normalizes virtual count and physical CVAL distance around one host-counter sample, filters writable controls, strips ISTATUS, ignores TVAL, and applies a ten-write never-run restore after complete preflight. It also rejects active native-v1 SVE/SME/debug optional state and replaces the retained 16-byte VMGenID before injecting its edge-rising SPI. Both boot-session forms delegate timer and VMGenID operations; #1477 later composes VMGenID with typed VMClock restore under the public aggregate. Timer EOI policy remains deferred. | Wrapping arithmetic and control filtering; every preflight/write failure and completed prefix; fresh-sample retry; all runner conflicts/lifecycle cleanup; random/zero/duplicate/write/signal VMGenID stages and redaction; signed fresh-VM timer restore, armed/masked controls, both session delegates, guest-buffer/metadata equality, and successful SPI injection before run. |
| Runner pending-interrupt capture and restore (first bidirectional interrupt subset implemented) | #1174 adds typed IRQ/FIQ owner-thread get/set commands and one failure-atomic IRQ-then-FIQ capture. #1248 adds ordered owner-thread restore of that complete value through a dedicated value-free failure with the exact failed type and completed prefix. The two writes are nontransactional, so callers must retry the complete value or discard the vCPU before execution. CPU pending levels and validated GIC PPI mutations share generalized interrupt-operation admission but remain distinct state models. Both boot-session forms expose capture and restore, but the snapshot lease invokes neither. HVF clears both levels after a run, so automatic pre-run reassertion, the separately captured opaque GIC blob and EL1 ICC value, routing, delivery/EOI, persistence, schema, orchestration, and multi-vCPU association remain deferred. | Exact IRQ-then-FIQ read/write order; both read and write failures; typed value-free partial-write context; complete retry; bidirectional conflicts; abandonment, channels, queued destruction, unwind, panic, shutdown; and signed IRQ-only restore/recapture twice after a FIQ-only mutation, followed by explicit clear, without a guest run or GIC/delivery claims. |
| Runner opaque GIC device-state capture and restore (second bidirectional interrupt subset implemented) | #1178 adds a redacted immutable byte value and owner-loop capture for Hypervisor.framework's stable, versioned GIC device blob, with fallible allocation and retained-object cleanup. #1255 adds an independently loaded setter and command-owned pre-first-run apply of the complete value. Both operations share generalized interrupt admission; restore checks the sticky run lifetime atomically, preserves exact HVF failure provenance, and clones no bytes into diagnostics. Both boot-session forms expose capture and apply without involving the snapshot lease. EL1 ICC state is separate; parsing, persistence, compatibility preflight, cross-step lease, schema, orchestration, and multi-vCPU stopping remain deferred. | Capture create/size/data/release order and cleanup; restore exact pointer/`usize` length, empty/no-call and backend failure; sticky run gate; every forward/reverse conflict; abandonment, channels, queued destruction, unwind, panic, shutdown; redacted debug; and signed non-empty same-VM capture/reapply before run without parsing, comparison, logging, or guest execution. |
| Runner EL1 GIC ICC register capture and restore (third bidirectional interrupt subset implemented) | #1180 adds a typed immutable ten-register value and owner-thread capture for PMR, BPR0, AP0R0, AP1R0, RPR, BPR1, CTLR, SRE, IGRPEN0, and IGRPEN1. #1258 adds a pre-first-run owner command that independently preloads getter and setter capabilities, writes the nine architecturally mutable fields in capture order, and validates the derived read-only RPR at its original position. A typed value-free error distinguishes write from derived-value validation and reports the exact register and completed write prefix. The operation is nontransactional, so callers must retry the complete value or discard the vCPU before execution. It shares generalized interrupt admission and complements, but is not embedded in, the opaque GIC blob; callers apply that compatible blob first without receiving a cross-step lease. Both boot-session forms expose capture and restore without involving the snapshot lease. `ICC_SRE_EL2`, ICH/ICV, destination validation, host-update preflight, persistence, composite orchestration, and multi-vCPU association remain deferred. | Exact SDK ids and ten-position read/write-or-validate order; every capture read failure, every mutable write failure, RPR read failure and mismatch; typed value-free partial-write context; complete retry; sticky never-run gate; bidirectional conflicts, abandonment, channels, queued destruction, unwind, panic, shutdown, and both boot-session delegates; signed guest-written PMR/BPR/SRE/group-enable capture plus same-idle-vCPU opaque-blob/ICC capture, ordered restore, and two exact recaptures without guest execution or value logging. |
| Native-v1 baseline device profile (internal state and preflight implemented) | #1268 adds an exact standalone `BANGDEV\0` v1 profile capped at 16 KiB for one read-only root virtio-block device, complete healthy virtio-mmio registers, one queue and active cursors, guest-visible interrupt status, frozen limiter/retry time, UART registers with fresh-default output, and canonical VMGenID/VMClock metadata without reusable generation bytes. Capture joins process-owned drive/serial configuration with one quiesced worker observation; a supplied grant backing is identified from its live descriptor without reopening its persisted tag. Load preflight validates mapped non-overlapping rings and cursors, either reopens the direct root read-only/no-follow or adopts the contained persisted read-only `DriveBacking`, requires exact device/inode/length/mode/mtime/ctime identity, and builds drop-safe block/serial resources off-side. #1270 nests this exact value in the composite bundle, #1272 installs it without boot writes and performs post-GIC VMGenID replacement, and #1368 supplies atomic contained state/memory/root preparation. | Deterministic codec/header/EOF/bounds/redaction; transport no-partial-restore; queue mapping/cursor/retry; injected-time limiter and scheduler tests; real-file identity/no-follow and supplied-file origin; fresh-serial preflight; no-boot-write installation; runtime/HVF ownership; signed direct and contained distinct-destination continuity. |
| EL2 GIC CPU registers and remaining emulated-device state | Inventory `ICC_SRE_EL2` plus ICH/ICV ownership and add stable state models for optional MMIO devices outside the native-v1 baseline. | Per-device round-trip unit tests and signed HVF EL2 CPU-interface/device-state coverage if nested virtualization is enabled. |
| Full guest-memory image I/O (internal primitives implemented) | #1263 defines the native-v1 fixed memory header and state-authoritative GPA binding, preserves exact discontiguous/dynamic region boundaries and canonical absolute offsets, and streams full bytes through a fallible 1 MiB buffer with CRC-64/Jones. #1441 lets the internal loader select anonymous or descriptor-backed shared memory only after seek-observed length, pair identity, trailer, binding checksum, and EOF validation; native-v1 File restore remains eager anonymous while public native-v2 retains private File/COW mappings. #1270 adds cooperative stage/chunk cancellation and holds immutable capture ownership through this copy. | Golden header/binding/CRC bytes; exact maximum metadata; anonymous/shared multi-region and chunk-boundary round trips; malformed layout/length/identity/integrity; short/interrupted/failing I/O and seek races; cancellation before fixed stages and successive chunks; allocation/access failure and partial-owner drop; full process and signed capture coverage. |
| No-clobber artifact commit boundary (internal primitive implemented) | #1264 adds the fixed memory-only commit record, directory-fd-anchored macOS staging, exclusive memory-first/state-last publication with file and directory barriers, typed orphan and committed-uncertain outcomes, and the inverse state-first committed-pair loader. #1270 preserves kind 1 exactly and adds bounded kind 2 for binding plus opaque complete state. #1274 adds a generic typed producer over a pathless staging writer, enforced writer-close proof, and fixed-size record/output matching while preserving kind 1. #1575 adds a closed owned native-family commitment, derives v2 memory identity only from the exact state bytes, and routes current-v2 publication plus compatible v1/v2 state preparation and direct/contained pair loading through the same transaction. The v2 staging verifier admits only the owned read-write staging inode, while final loading retains the separate read-only/CLOEXEC private File/COW policy. Destination directories are trusted and published finals are never cleanup targets. At #1575 landing no public VMM/API path invoked v2; the later #1578 row supersedes that dispatch boundary. | Exact codec bytes and malformed inputs for both v1 kinds and native-v2; closed family/version/redaction checks; callback ordering/skip/panic/error/retention/forget and retry; v1/v2 output mismatch; same/cross-directory and anchored success; state-first direct/opened loading; File/COW isolation and source preservation; all final file types and aliases; ordered failure injection; late collisions; observed staging replacement; cleanup failure; corruption; redaction; and coordinated multiprocess contention. |
| Native-v1 composite bundle and private capture (internal implemented) | #1270 added the exact five-component `BANGHVF\0` profile, atomic default-vCPU cache manifest, bounded GIC capture, one aggregate four-domain runner command, explicit fresh-RTC policy, and a supervisor-owned capture that holds paused admission and auxiliary quiescence through encoding and cancellable memory streaming. It returns a detached kind-2 bundle, publishes no final path, and leaves recoverable source sessions paused, retryable, and resumable. At that slice's landing public activation and optional devices were deferred; #1276 later activated this baseline. | Kind-1 preservation and kind-2/component golden/malformed/cross-validation/redaction tests; exact runner capture order, conflicts, abandonment, and cleanup; supervisor order/cancellation/retry/drop tests; full memory decode plus real signed capture and retained source-owner reuse. |
| Native-v1 private load and paused restore (internal implemented) | #1272 added committed-pair load, fixed platform/cache validation, baseline installation without boot writes, fresh VM/GIC/runner construction, aggregate architecture/GIC/ICC/timer/pending restore, VMGenID replacement, initially paused worker handoff, and value-free retryable/terminal cleanup evidence. At that slice's landing public `LoadSnapshot` was deferred; #1276 later routed the same transaction and applied resume only after paused commit. | Platform/install unit tests; exact aggregate validation/restore order, source/destination optional-state rejection, sticky never-run admission, paused worker start, controller intent/terminal tests, strict redaction/lints, and a signed disk-artifact distinct-destination continuation with VMGenID replacement. |
| Native-v1 composite publication transaction (implemented) | #1274 added the process create seam and #1276 activated it. #1390 moves the entire direct or anchored publisher under the paused worker lease, seals cancellation before commit, preserves post-seal typed visibility through shutdown, and invokes the explicit no-op post-publication hook before releasing all four retry guards and admission. Capture failure/cancellation removes only private staging and leaves recoverable sources paused; worker panic remains terminal. | Runtime callback/close/output/ordering/failure tests; ProcessVmm preflight/config/no-mutation tests; supervisor collision/cancellation/seal/shutdown/retry/panic tests; synchronous API/MMDS/periodic serialization; and signed production publication followed by distinct-destination load/restore/continuation. |
| External resource policy | Define disk, vmnet, and vsock metadata, buffering boundary, disconnect/reconnect behavior, and restore overrides. | Resource-policy unit/process tests and focused signed network/vsock coverage. |
| Legacy native-v1 public endpoint activation (implemented, writer superseded) | #1276 originally routed create and load for the admitted native-v1 profile, preserved Firecracker-shaped response/latency/deprecation behavior, committed load as `Paused` before applying `resume_vm`, and exposed typed redacted execution faults. #1578 supersedes only public create with native-v2 and retains native-v1 as an exact family-selected reader. | Runtime/process/API tests, immutable native-v1 fixtures, focused Uffd compatibility coverage, and signed frozen-v1 File dispatch through the current public family router. |
| Native-v1 PL031, VMGenID, and VMClock restore (implemented) | #1477 adds the exact validated 112-byte VMClock ABI to nested `BANGDEV\0` 1.1.0 while loading legacy 1.0.0 from bound memory, reconstructs PL031 from destination wall clock with no alarm state, and performs VMGenID replacement/notification before the fenced VMClock counter update/notification after aggregate restore and before resume. Any failure after the first identity commit is terminal. | ABI/codec/memory-agreement and legacy unit tests; every VMClock write and signal disposition; aggregate ordering and cleanup terminality; PL031 destination-time/no-alarm tests; signed two-destination guest polling of both VMGenID halves, stable sequence and both VMClock counters, RTC monotonicity, and continuation. |

Shared dirty epochs are complete; Diff artifacts and optional resources remain
their own issue-sized areas. The public create and restore transactions are
deliberately limited to the native-v1 baseline.

bangbang reports unsupported only for request shapes and profiles outside that
baseline. Accepted Full create and File load requests use the production
publisher/loader; native envelope version reporting and read-only inspection
remain available independently.
