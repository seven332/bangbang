# Firecracker Validation Matrix

This matrix summarizes bangbang's current Firecracker-facing compatibility
coverage. Detailed endpoint behavior, field policy, platform limits, and
compatibility rationale remain in
[Firecracker Compatibility Scope](firecracker-compatibility.md).

## Status Vocabulary

- `implemented`: the public behavior exists for the documented subset.
- `partial`: an initial subset works, but important Firecracker behavior is
  still tracked by the related issue.
- `recognized unsupported`: the API shape is parsed or recognized before
  returning a Firecracker-shaped fault.
- `deferred`: the behavior needs a larger capability or backend design.
- `platform-limited`: the Firecracker feature depends on Linux-specific
  mechanisms or a host facility that does not map directly to macOS/HVF.

These descriptive matrix values do not satisfy #1348's terminal inventory
rule. The checked
[v1.16.0 capability inventory](../compat/firecracker/v1.16.0/README.md) uses
`audit-required`, `missing-platform-feasible`,
`implemented-and-verified`, and `proven-platform-impossible`; only the latter
two may remain at final certification, with their required evidence.

## Validation Layers

- `unit`: crate-local Rust tests for parsers, state, error formatting, and
  backend-neutral helpers.
- `api socket`: in-process API server tests over a real Unix socket.
- `process e2e`: unsigned executable tests in `crates/bangbang/tests/`.
- `signed process`: `scripts/run-signed-process-tests.sh`.
- `signed HVF`: `scripts/run-integration-tests.sh` targets that create HVF
  resources or boot guests.
- `signed production bundle`: the same wrapper's `production_bundle` target,
  which builds and inspects the fixed launcher/nested-worker topology before
  exercising it on supported Apple Silicon.
- `docs`: compatibility, security, testing, or review documentation.

## Matrix

Network lifecycle certification #1501 binds the authenticated lifecycle-v5
session identity and vmnet authority as one live-only owner across startup,
restore, runtime MMDS-only/vmnet selection, provider entries and readiness, and
capture traversal. Same-policy cross-session use is rejected before backend or
callback work; the identity is redacted and absent from detached state. Signed
networkless coverage rejects positive host, shared, and bridged policies before
session creation. Positive external vmnet start/connectivity remains #1378.

| Area | Current status | Primary validation | Related issue | Notes |
| --- | --- | --- | --- | --- |
| Native-v2 state, File/COW memory, artifact transaction, public process lifecycle, and focused reconstruction | current 2.10 complete-serial multi-vCPU Full/File lifecycle with independently optional profile-3 storage, entropy, balloon, and virtio-mem implemented and verified; exact 2.3–2.9 and frozen native-v1 remain readable | unit, fixed binary fixtures, hostile component/registry/device/FDT/balloon/virtio-mem mutations, artifact fault/authority checks, API/process routing, all-sixteen-product restore-stage injection, signed HVF, signed direct MMIO/PCI executable, signed App Sandbox and production bundle, docs | #1490, #1525, #1526, #1528, #1529, #1566, #1567, #1568, #1569, #1575, #1576, #1577, #1578, #1583, #1584, #1585, #1586, #1587, #1588, #1589, #1616, #1617, #1634, #1651, #1652, #1665, #1666, #1680, #1681, #1697, #1698 | The current writer emits `2.10.0` with required serial kind 8 and independently optional profile-3 storage kind 7, entropy kind 9, balloon kind 10, and virtio-mem kind 11 over coherent MMIO or PCI. Kind 11 is capped at 128 KiB inside the 16-MiB state cap and canonically binds configuration, features, config space, queue, transport, and plugged bitmap to unchanged kind-1 extents. Restore validates the pair before publication, maps base RAM privately, creates one fresh unlinked shared aperture with block-granular plugged views, establishes a clean dirty epoch, and constructs fresh device/metric/interrupt/dispatcher/route/cleanup owners. Signed direct and normal-production/App-Sandbox MMIO/PCI destinations verify nonzero plugged-memory bytes before mutation, then continue partial UNPLUG, driver-reprobe UNPLUG_ALL/replug, PLUG, and final UNPLUG through explicit-Paused/recapture and automatic clones. Same-process peers, immutable inputs, malformed state/memory, cancellation/death cleanup, containment, exact metrics, and all prior serial/entropy/balloon/storage continuation remain covered. Diff, native-v2 Uffd, network/vsock/MMDS restore, Firecracker bytes, source owner/dirty identity, synchronous RSS, and broad portability remain explicit non-claims. |
| Direct pmem mapping, root boot, and native-v2 restore | implemented and verified for startup MMIO/PCI, non-root runtime PCI, complete capture-ready live state, exact native-v2 2.6 through current 2.10 profile-3 serialization/restore | unit, process e2e, signed HVF capture equality, focused profile-3 fault/authority tests, signed executable storage matrix, signed normal-production bundle storage matrix, docs, pinned-source compare | #1439, #1444, #1448, #1471, #1634, #1651, #1652, #1665, #1666, #1680, #1681, #1697, #1698 | One reference-counted file/private-tail mapping is registered directly with HVF. Profile 3 binds exact file/mapped geometry and restores direct paths or exact `PmemBacking` grants with the complete block/pmem batch; current 2.10 composes that unchanged storage graph with required serial and optional entropy/balloon/virtio-mem. Signed rooted pmem-only and rootless mixed MMIO/PCI cells prove shared writable prefix epochs, unchanged read-only peers, a zero DAX private tail on every fresh mapping, limiter/retry and interrupt continuation, recapture, immutable state/memory, pathname-replacement resistance, and cleanup. Pmem remains outside ordinary RAM dirty epochs, and both checked pmem composite records are terminal. |
| Guest-memory backing profiles | anonymous default, descriptor-backed boot RAM, and exact-2.10 fresh virtio-mem mixed-memory restore implemented; direct and contained vhost-user block support dynamic memory and aggregate storage certification | unit, real Unix stream/SCM_RIGHTS/pipe/kqueue, fixed/hostile memory-image tests, signed HVF, signed executable, signed production bundle, docs | #1439, #1441, #1443, #1444, #1445, #1449, #1462, #1471, #1634, #1697, #1698 | Ordinary-only VMs retain anonymous or private File/COW RAM. Virtio-mem startup and exact-2.10 restore reserve one unlinked sparse shared aperture while only plugged block-granular views enter CPU/HVF mappings, accounting, access, and dirty metadata; every restored destination gets a new reservation and copies only committed plugged bytes. Vhost backends receive the bounded guest-ordered table but no unrelated mapping. Exact offset discard follows committed unplug, and shutdown drops the reservation after active views. Signed direct and contained MMIO/PCI guests prove byte-first restored continuation, partial shrink, reprobe UNPLUG_ALL/replug, later growth/removal, immutable independent clones, stable geometry, unchanged entitlements, no helper, and exact cleanup. Vhost snapshot state remains rejected. |
| Capability inventory enforcement | structural inventory implemented; capability audit in progress | unit, workspace CI, docs, pinned-source compare | #1348, #1349, #1527, #1546, #1547, #1548, #1549, #1550, #1551, #1552, #1553, #1554, #1555, #1578, #1589, #1616, #1617, #1634, #1651, #1652, #1665, #1666, #1680, #1681, #1697, #1698 | The machine-owned v1.16.0 source manifest and human overlay keep every disposition mechanically checked. #1697 activates current 2.10 with optional virtio-mem kind 11 across all sixteen products; after focused no-skip evidence, #1698 promotes exactly `corpus:memory-hotplug` and `semantic.memory-device:virtio-mem-lifecycle-accounting-and-state`. Counts are 242/156/3/17. Other snapshot corpora and API/schema aggregates remain under audit for Diff, network/vsock/MMDS restore, tools, Firecracker interoperability, and portability. |
| UFFD-equivalent snapshot paging | frozen native-v1 direct/contained macOS compatibility path implemented and verified; current native-v2 rejects Uffd | checked upstream/platform/consumer ledger, pager unit/process/reference-peer tests, public family-routing tests, lazy-memory concurrency tests, signed host/guest/removal/App Sandbox component tests, signed frozen-v1 File dispatch, exact entitlement dictionaries | #1527, #1546, #1547, #1548, #1549, #1550, #1551, #1552, #1553, #1554, #1555, #1578 | `bangbang-pager-v1`, `LazyGuestMemory`, and the task-local Mach/HVF fault bridges retain the bounded offset-only native-v1 demand-paging contract. V1 Uffd requires macOS Apple Silicon, the fixed-memory profile, and dirty tracking disabled; platform/profile/consumer/grant preflight precedes resource access, and the state-bound session/layout plus pager negotiation validate before transactional publication. Direct mode connects with a deadline; contained mode claims only the launcher-connected stream without a worker memory-file grant. Signed component and production pager cases cover execute/read/write demand, removal generations, coalescing, peer failure, death orders, repeat, cleanup, and exact entitlements. The current public native-v2 family rejects Uffd before memory/pager/HVF adoption; no lazy failure falls back to File/COW, and this is not Linux UFFD wire compatibility. |
| Offline seccompiler tool | implemented and verified for the complete pinned tool corpus, operation, and five arguments | unit, process e2e, independent cBPF interpreter, pinned Linux oracle, docs | #1382, #1383 | `seccompiler-bin` accepts the v1.16 target/input/output/basic/split interface, compiles exact `vmm`/`api`/`vcpu` policy semantics for x86_64 and aarch64, writes bitcode 0.6.9 combined output or exact raw split names, and applies Firecracker's 100,000-byte consumer cap. Bounded redacted no-follow input and descriptor-anchored owner-only transactional output reject special targets and preserve replacements. Fault tests cover each split publication boundary plus rollback, durability, and cleanup uncertainty. A pinned aarch64 Linux run compared 433,440 semantic cases with Firecracker v1.16. The tool does not install/enforce seccomp; VMM filter loading and process flags remain #1384. |
| Process CLI and API socket | 25 of 29 inventory records implemented and verified; two proven platform impossible; only two broad source corpora remain under audit | unit, API socket, process e2e, signed process, signed App Sandbox process, signed production bundle, pinned-source compare | #536, #545, #1008, #1010, #1048, #1058, #1060, #1070, #1092, #1260, #1302, #1352, #1365, #1368, #1384, #1419, #1578, #1589, #1616, #1617, #1634, #1651, #1652, #1665, #1666, #1680, #1681, #1697, #1698 | The checked 23-argument contract has 21 implemented leaves and two terminal seccomp platform exclusions. It covers argument precedence, Unicode identity, limits, readiness, fd-table behavior, signals including nonfatal SIGPIPE, socket ownership/cleanup, configuration startup, PCI, and current native snapshot output. `--snapshot-version` prints `v2.10.0`; bounded direct or exact-granted description reports the actual validated native-v1 or native-v2 version and explicitly rejects Firecracker bytes. The complete instance/version-output semantic and aggregate run operation are terminal. Only broad `corpus:design` and `corpus:getting-started` remain under audit for unrelated repository-wide claims. |
| Instance/version/config reads | implemented | unit, api socket, process e2e | #536 | `GET /`, `GET /version`, `GET /vm/config`, and `GET /machine-config` expose accumulated supported state for the current subset. Unsupported config sections are omitted until modeled. |
| Machine and boot configuration | Wave 2 foundations complete; later-wave snapshot/tool/dynamic-topology work remains partial; sizing/SMT/cache FDT, finite reviewed arm64 CPU-template policy, and dirty epochs complete; exact 2M/KVM/static-template execution platform-excluded | unit, api socket, process e2e, signed HVF, signed executable | #538, #1284, #1285, #1293, #1298, #1391, #1392, #1393, #1395, #1396, #1402, #1403, #1408 | Pre-boot machine config now has Firecracker-shaped defaults/replacement/partial-update/clear and empty-PATCH behavior, runtime-owned value-redacted semantic faults, deliberate aarch64 SMT-vCPU-memory/page precedence, exact `1..=32` vCPU and `1..=1,046,528` MiB configured-equals-realized bounds, transactional balloon compatibility, and defensive startup validation. Unlike Firecracker's accept/echo/later-truncate quirk above 1022 GiB, Bangbang rejects before storage. Exact Linux hugetlbfs `2M` is certified unavailable through public arm64 XNU/HVF; odd memory gets page compatibility first, while an otherwise valid request gets a stable pre-allocation platform fault. Alignment and 16-KiB IPA granules are not substitutes. Host-free-memory preflight is not promised. `track_dirty_pages` now enables one shared boot/VMM/device/guest-CPU page epoch before normal population, with protected dynamic mappings and failure-atomic Full-publication reset. Bounded/lossless custom CPU input, stronger duplicate/index checks, exact masks for eleven reviewed U64 arm64 identification registers plus ACTLR.EnTSO, U64 X/core, U128 Q, and U32 FP state, explicit little-endian Q transport, fail-closed U32 scalar transport, boot-reserved/banked-state policy, transactional static/custom/empty/`None`/omitted replacement, pending GET-visible `V1N1`, a pre-backend V1-source gate, mixed-width all-vCPU read-before-write, immediate readback, boot override precedence, whole-unpublished-VM cleanup, frozen native-v1 custom exclusion and native-v2 applied-template evidence, and strict KVM capability/vCPU-feature platform faults are covered. A public pre-VM macOS 15.2 gate protects ZFR0/SMFR0, ACTLR filters are confined to EnTSO bit 1, and every other KVM/public-HVF register family has an exhaustive stable value-free classification. Boot source, kernel/initrd loading, FDT generation including arm64 `/chosen/linux,pci-probe-only` and 64-byte `/chosen/rng-seed`, strict pre-VM cache identity/host-fact reconciliation, split or unified L1 plus shared L2/L3 FDT nodes, direct-rootfs boot paths, ordered owner-thread vCPU topology, owning concurrent boot-session coordination with active-only batch cancellation, all-MPIDR FDT input, indexed PSCI CPU_ON/CPU_OFF/CPU_SUSPEND/level-0 affinity transitions, per-vCPU timer PPIs, internal signed Linux CPU1 execution, and public host-limited SMP startup are covered. Public `InstanceStart` admits `1..=min(32, host_max)` and keeps capacity/construction failures before session retention or `Running`. Signed executable proof configures two vCPUs through the API, observes independent pinned CPU0/CPU1 progress, offlines CPU1 through guest sysfs, brings the same owner back online, and observes resumed CPU1 progress without fixed sleeps. Signed HVF bare-guest proof retains CPU1 across two virtual-timer suspend cycles while CPU0 observes ON affinity. Signed HVF CPU-template proof captures a disposable in-memory baseline, applies all seven new IDs plus ACTLR.EnTSO within the mixed ID/X/core/Q/FP profile to two fresh owners, requires exact readback, captures primary boot precedence plus retained targets, and shuts both sessions down without raw output. Signed Linux proofs compare exact cache sysfs geometry with the retained model and compare baseline/custom CPU-template ID views per CPU without serializing raw values. Static named-template execution, public cpu-template-helper operations, multi-vCPU native-v1 snapshots, FDT idle-state discovery, non-timer suspend wake, dynamic CPU topology, and cross-host portability remain deferred or platform-excluded as recorded. |
| Product PCI and modern virtio-pci | all-virtio startup, aggregate runtime hotplug/live storage certification, and current native-v2 2.10 block/pmem/serial plus optional entropy, balloon, and virtio-mem persistence; vsock remains capture-ready only | unit, process e2e, signed HVF capture/equality/reconstruction, signed executable aggregate plus storage/serial/entropy/balloon/virtio-mem restore, signed production bundle equivalent, docs, pinned-source compare | #1416, #1417, #1418, #1419, #1420, #1421, #1422, #1423, #1444, #1448, #1471, #1475, #1516, #1583, #1584, #1585, #1586, #1587, #1588, #1589, #1616, #1617, #1634, #1651, #1652, #1665, #1666, #1680, #1681, #1697, #1698 | One immutable all-virtio transport is selected after target/GIC-MSI and exact endpoint/vector preflight. Exact 2.6 persists profile-3 block/pmem, 2.7 adds required platform-MMIO serial, 2.8 may add entropy, 2.9 may add balloon, and current 2.10 may add virtio-mem in canonical PCI slot/route order. Kind 11 retains common virtio/BAR/MSI-X/interrupt/registry state and reconstructs fresh shared-memory, notifier, interrupt, dispatcher, metrics, endpoint, route, and cleanup owners. Signed direct and contained matrices prove restored byte/topology continuation, recapture, immutable clone isolation, and cleanup. Automatic guest hotplug notification, network/vsock persistence, external vmnet certification, and KVM ITS identity remain deferred. |
| Drives and virtio-block | MMIO/PCI Sync and portable Async file/block-special lifecycle, vhost-user lifecycle, GET_ID, live PATCH, runtime PUT/DELETE, capture-ready handoff, aggregate live certification, and profile-3 coexistence implemented | unit, real regular/block file/pipe/kqueue and Unix stream/SCM_RIGHTS/shared mapping, fixed grant/control codecs, api socket, process e2e, signed HVF capture equality, signed executable aggregate, signed production bundle aggregate | #539, #916, #962, #992, #994, #996, #998, #1020, #1068, #1268, #1304, #1362, #1418, #1419, #1420, #1443, #1445, #1446, #1447, #1448, #1449, #1460, #1461, #1464, #1465, #1466, #1471, #1634 | File-backed drives accept an existing regular file or exact macOS block-special descriptor with default Sync or explicit portable Async over MMIO/PCI and direct/contained ownership. Vhost-user remains a bounded operator-trusted frontend and rejects snapshot create before artifacts. Focused and signed gates cover cache, flush, limiting, replacement, hotplug, rollback, capture, teardown, exact reuse, and aggregate coexistence. Current profile 3 serializes supported regular-file block records alongside pmem and restores the complete vector atomically; native-v1 stays regular-file/Sync-only and exact 2.5 retains the block-only compatibility graph. |
| Network and MMDS | MMIO-default and all-PCI startup implemented; PCI-only Running/Paused PUT/DELETE implemented; portable packet semantics, direct-vmnet batching, bounded MMDS TCP sessions, and capture-ready state implemented | unit, api socket, process e2e, signed HVF, signed executable, signed production bundle, docs | #540, #962, #982, #1066, #1090, #1146, #1148, #1150, #1154, #1306, #1307, #1308, #1309, #1310, #1311, #1312, #1313, #1377, #1418, #1419, #1422, #1495, #1497, #1498, #1499, #1502, #1503, #1496 | Initial network config, guest-advertised MTU, independent RX/TX limiters, transactional runtime limiter updates, backend-neutral retry timing, per-session scheduling, process-local MMDS, instance-bound stateless AES-256-GCM v2 tokens, aggregate/per-interface metrics, multi-interface and multi-process MMDS isolation, and modern PCI startup are implemented and signed. Public PCI sessions now accept a new validated network ID/MAC in Running or Paused, prepare one independent MMDS-only or vmnet packet-I/O owner, publish generation-safe metrics plus an endpoint through the owner-thread transaction, and commit live config last. Bodyless DELETE coordinates reversible PCI teardown with exact packet-I/O take/stop/restore, then releases queues, callbacks/events, limiter deadline, metrics generation, MMDS detour or vmnet handle, slot/BAR/MSI-X/dispatcher resources, and live config. Default MMIO, duplicate ID/MAC, invalid/missing/capacity, contained authority, snapshot/shutdown admission, and injected failures are nonmutating or terminal when restoration is uncertain. Signed direct and normal networkless-production guests perform two rounds of PCI rescan, real MMDS exchange, sysfs removal, DELETE, Paused reuse of the same ID/MAC/BDF, and clean shutdown without vmnet entitlement; the production case also rejects a non-MMDS bridged insertion without mutation. Existing entries retain their startup resource class and state; contained vmnet accounting uses actual live vmnet entries, while MMDS-only entries require no authority. Production packaging still claims no repository credential or positive external vmnet start/connectivity result. Typed vmnet start-result reconciliation, global realized-MAC reservation, finite lifecycle deadlines, and terminal cleanup uncertainty are implemented under #1495 without positive external evidence. #1502 adds generation-scoped packet callbacks, a capacity-one owner wake bridge, exact-interface MMIO/PCI readiness before vCPU entry, one bounded RX batch per pass, publication-safe staged TX, explicit partial counts, preserved MMDS/limiter/result order, and disable/drain/stop retirement. #1503 adds the exact checksum/TSO/UFO feature matrix, bounded software normalization, transactional merged RX, raw/direct backend envelopes, exact partial-batch and spoof-observation semantics, and detailed limiter/backend latency metrics. Signed MMIO and PCI MMDS-only guests acknowledge every published feature, turn one bounded TCP request into multiple host packets, receive a validated 49152-byte merged response, and progress after an RX limiter retry. #1498 matches the pinned 36-byte AES-256-GCM token envelope, standard Base64, TTL and current-key rotation semantics, binds immutable instance ID as AAD, removes the active-token table, keeps failed first-use and rotation nonmutating, redacts key/AAD/token state, and adds signed peer-token `401` rejection while preserving own-token validity across two processes. #1499 replaces the manual detour with one bounded interface-local MMDS stack: exact speculative target ownership, ARP-first output, 30 connections, 100 resets, 2,500-byte request buffers, one response, MSS/window flow control, ordered streams, segmentation, ACK/FIN/RST progress, eviction, 1.2-second retransmission, and fifteenth-timeout reset. One output frame is retained until guest RX commit, and future protocol deadlines merge with limiter deadlines in the generation-safe MMIO/PCI scheduler. Signed guests renew v2 tokens, receive segmented 49,152-byte responses, deliberately lose an ACK, and observe retransmission. #1497 adds deterministic startup/runtime MMIO/PCI owner traversal; detached queue, feature, limiter, generation, metrics, backend, and MMDS identity; one exact TX retry; generation-aware callback quiescence; and explicit normalization of cached RX, peer packets, active TCP/ARP/output/timers, callbacks, handles, tokens, borrows, and absolute clocks for a fresh lossy destination. Signed MMDS v1/v2 process and HVF lifecycle coverage proves pause/capture/resume, equality, limiter state, runtime generation reuse, rollback, and teardown. #1496 certifies the exact 35-record aggregate ledger: 31 outcomes are terminal and four retain explicit downstream ownership. Positive external vmnet connectivity and host firewall policy remain #1378; network/MMDS encoding, restore, and clone freshness remain #1490; performance reconciliation remains #1491. |
| Virtio-vsock | implemented-and-verified API/live MMIO-or-PCI contract plus checked capture/reconstruction, quiesced source integration, and destination resource handoff; aggregate artifact restore remains #1490 | unit, api socket, process e2e, signed HVF, signed executable, signed production bundle, docs | #541, #984, #1322, #1323, #1324, #1365, #1419, #1513, #1514, #1515, #1516, #1517, #1518 | Repeatable pre-boot `PUT /vsock` and stable post-start rejection, guest-visible MMIO/FDT attachment, process-local Unix-listener ownership/inode-safe cleanup, one 1023-connection active budget shared across both initiation directions, a separate 256-entry incomplete-host-handshake bound, round-robin host-local-port allocation, bounded RW queues, dynamic 64-KiB credit windows with wrapping counters, partial/full shutdown, delivery-based two-second request/shutdown cleanup, reset/error handling, distinct read/write wakeup interest, `EVENT_IDX`, validated source-side `TRANSPORT_RESET` publication, restored-origin queue signaling with runtime-only RX acknowledgement gating, and Firecracker-shaped aggregate metrics for the implemented queue/packet/byte/cleanup/failure surface are covered. Direct signed executable cases verify ≥1 MiB in each direction for guest- and host-initiated streams, both peers' write-half-close/EOF sequence, terminal cleanup, path/payload-redacted diagnostics, and independent two-stream exchanges. Contained mode atomically claims one exact vsock directory plus safe child, publishes and supplies the main listener, routes guest initiation through a fixed session-bound launcher port facet returning connected fds without guest payloads, and preserves the same guest routing/credit/shutdown model. Signed normal-production cases prove a real guest initiates two granted-port streams, a real host completes deterministic 1-MiB bidirectional and half-close/EOF traffic through the granted main listener, exact entitlements remain unchanged, and no helper survives startup. Indirect descriptors are a supported bangbang extension. PATCH, DELETE, runtime hotplug, broader CID routing, general performance/artifact parity, runtime PCI hotplug/vhost/KVM, broader muxer metrics, and broader event types remain outside the live subset. Internal immutable redacted MMIO/PCI capture now preserves CID, features, activation, all three queue cursors, EVENT_IDX, the validated host-local cursor, and a logical selector while returning reset/source-normalization evidence separately. Shared validation rejects malformed identity, features, queue geometry/indexes, mapped-range overlap, reset origin, and selector/resource mismatch; supplied-listener/connector reconstruction creates empty live work and rearms the active snapshot-origin gate, while PCI returns components without placement. The production paused transaction now reconciles the exact config/runtime/HVF MMIO-or-PCI owner and metrics/memory authority, publishes reset, captures, and detaches source-only accepts, connections, packets, and their wakeups/deadlines under one lease while retaining listener/connector authority for fresh traffic. Current public native-v2 create invokes it before the unchanged optional-device rejection, with typed redacted process policy and no artifacts. Internal destination handoff resolves captured/override selectors before resource access, prepares owner-only stale-safe direct or exact transactional contained resources without ambient fallback, and transfers cleanup through single-use runtime adoption. The checked 14-row ledger terminally certifies eight API/live records. Public native-v2 artifacts still omit vsock encoding/placement and load rejects overrides; six aggregate invocation, restored acknowledgement/reconnect/override, clone/version, and portability outcomes remain #1490 work, so the producer is not classified as publicly snapshot-compatible. |
| Observability: logger, metrics, serial | implemented supported process-local stdio subset; production telemetry and global durability profile-limited | unit, api socket, process e2e, signed process, signed HVF, signed executable, signed production bundle, docs | #542, #918, #982, #984, #986, #988, #990, #992, #1008, #1010, #1024, #1056, #1074, #1088, #1090, #1276, #1340, #1341, #1342, #1343, #1476, #1479 | Logger configuration/filtering, unrestricted request/action records without bodies, the bounded ten-per-five-second boot-timer callsite with recovery warning, best-effort delivery, and missed/rate-limited counters are covered. Metrics use successful-write interval deltas for every implemented API/logger/signal/UART/device count, byte, failure, error, limiter field, and block `sum_us`; startup timing, boot status, latest action latency, and block min/max/sample count are stores. Lower/new generations, keyed disappearance/reappearance, sparse absent families, ambiguous at-least-once replay, and bangbang's `metrics_flush_count: 1` extension are tested. Configuration is silent; one retained-session initial attempt, 60-second Running/Paused attempts, fallible explicit action, and one best-effort normal-terminal attempt have focused/process proof, while existing API/config-file signed scenarios now observe the additional post-exit line. Nullable nonblocking configured serial files/FIFOs, contained output grants, default nonblocking stdout plus terminal/FIFO stdin, token-bucket drops, and a portable 64-byte RX FIFO with DR/OE/RDA/FCR behavior are covered. The owner run loop performs capacity-bounded reads, full-FIFO disarm and guest-drain rearm, EOF/error detach, retryable GIC delivery, Running-only consumption, paused capture exclusion, final-owner terminal/flag restoration, complete redacted capture-ready state, shared UART deltas, cleanup, signed launcher/App Sandbox flow, and multi-process isolation. There is no public streaming API, fake zero-filled absent-device schema, process-global panic/fatal writer, or rotation/syslog/journald/tracing/remote telemetry. Bangbang-native v1 keeps its six-register serial encoding, rejects nonrepresentable live RX/status/intent state, restores a fresh output pipeline, and excludes host endpoints, public path, TX/RX bytes, limiter state, and counters; generalized encoding and endpoint reconstruction remain with Wave 6. |
| VM lifecycle and run-loop control | Wave 2 lifecycle foundation complete; generalized snapshot/device and dynamic-topology work remains partial | unit, api socket, process e2e, signed HVF, signed executable, docs | #537, #1293, #1298, #1284, #1158, #1160, #1162, #1164, #1166, #1168, #1170, #1172, #1174, #1176, #1178, #1180, #1182, #1184, #1186, #1188, #1190, #1192, #1194, #1196, #1198, #1200, #1202, #1204, #1206, #1208, #1210, #1212, #1214, #1216, #1218, #1220, #1222, #1224, #1226, #1228, #1230, #1232, #1234, #1236, #1238, #1240, #1242, #1244, #1246, #1248, #1250, #1252, #1255, #1258, #1261, #1276, #1389, #1390, #1408 | Host-limited public multi-vCPU `InstanceStart`, Running transition, retained boot worker status, runtime `PATCH /vm` pause/resume for the current process-owned boot worker, family-selected native-v1/native-v2 load commit as `Paused` followed by optional ordinary resume, guest PSCI `SYSTEM_OFF`/`SYSTEM_RESET` process exits, PSCI `CPU_OFF` with same-owner `CPU_ON` re-entry, and non-success terminal process failures are covered. #1293 adds exact non-returning CPU_OFF token consumption, last-online denial, scheduler-before-power commit ordering, narrow `SCTLR_EL1` warm-entry reset, and signed Linux sysfs CPU1 offline/online proof through both internal and public startup paths. #1298 adds exact retained CPU_SUSPEND transactions, timer-PPI-before-success ordering, online affinity preservation, lifecycle cancellation/rearm, and signed two-cycle CPU1 context-retention proof. #1389 makes pause acknowledgement a topology-wide active-run barrier across every online vCPU; signed dual-process coverage proves independent CPU0/CPU1 progress stops and resumes while an isolated peer continues. Ordinary paused commands and auxiliary work remain mutable outside a snapshot transaction. #1160 adds a scoped supervisor admission barrier: earlier FIFO commands finish, later ordinary commands and resume reject during its scope, and shutdown invalidates it out of band. #1162 introduced acknowledged block/entropy retry quiescence inside that scope. #1390 failure-atomically includes PMEM and network, drains tokens only after all four schedulers acknowledge, preserves in-flight/deferred/deadline work, and holds the same worker transaction through artifact verification, synchronization, exclusive memory-first/state-last commit, and a post-publication hook. Pre-seal signal cancellation cleans owned staging; post-seal shutdown preserves the publisher's exact typed visibility result. Synchronous API/MMDS/controller and periodic work cannot interleave. #1164 adds an internal runner command that captures immutable X0-X30, PC, and CPSR values on the owning thread with explicit conflict admission. #1170 adds a separate raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 command and shares one failure-atomic core-register admission domain with general-register capture. #1182 adds raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 capture in the same domain. #1184 adds raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 capture in that domain. #1186 adds raw ACTLR_EL1 and CPACR_EL1 capture there, with a macOS 15 ACTLR boundary. #1172 adds baseline Q0-Q31, FPCR, and FPSR capture through the same admission, preserves every 128-bit Q value, and proves boundary values in signed HVF. #1174 adds CPU-level IRQ/FIQ get/set and failure-atomic capture under generalized interrupt-operation admission, distinct from GIC state. #1176 adds raw TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture as a fourth command in the shared core-register admission domain. #1178 adds stopped-runner capture of Hypervisor.framework's stable, versioned opaque GIC device blob except CPU system registers, sharing generalized interrupt admission. #1180 adds a separate failure-atomic owner-thread command for all ten EL1 ICC CPU-interface registers exposed by the current SDK in that same interrupt domain. #1166 adds a separate owner-thread command for an immutable raw HVF virtual-timer mask/offset pair and serializes it with individual timer operations; #1168 extends the same value, capture order, and admission domain with raw control/CVAL access. #1188 adds raw CNTKCTL_EL1, CNTP_CTL_EL0, and CNTP_CVAL_EL0 capture under generalized timer admission, with macOS 15 and GIC-before-vCPU prerequisites. #1212 extends that capture with raw CNTP_TVAL_EL0 without treating the signed relative view as stable or simultaneous with CVAL. #1190 adds redacted five-key APIA/APIB/APDA/APDB/APGA capture from all ten SDK halves in the shared core-register domain. #1192 adds guest-visible MIDR/MPIDR and baseline PFR/DFR/ISAR/MMFR compatibility metadata in the same domain. #1194 adds observation-only raw MDCCINT_EL1/MDSCR_EL1 debug-control capture in the same domain without changing debug or trap behavior. #1196 adds observation-only raw CSSELR_EL1 cache-selection capture there without changing or interpreting cache state. #1198 adds DFR0-counted observation-only capture of every implemented raw DBGBVR/DBGBCR hardware-breakpoint pair in the same core-register domain without writes, enablement, trap changes, or guest execution. #1200 adds the corresponding DFR0-counted raw DBGWVR/DBGWCR hardware-watchpoint capture under the same admission and observation-only constraints. #1202 adds observation-only capture of Hypervisor.framework's debug-exception and debug-register-access trap-policy booleans in that domain without changing host policy or conflating it with guest EL1 debug state. #1204 adds a separate macOS 15.2+ ZFR0/SMFR0 SVE/SME identification-metadata capture there without changing the baseline identification command or enabling SVE/SME. #1206 adds a runtime-resolved macOS 15.2+ getter-only capture of mutable `PSTATE.SM`/`PSTATE.ZA` in the same domain without calling the setter or reading SME data. #1208 adds redacted getter-only capture of raw macOS 15.2+ SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 in that shared domain without writes or SME data reads. #1210 adds redacted getter-only capture of raw macOS 15.2+ SCXTNUM_EL0 and SCXTNUM_EL1 in the same domain without writes or guest execution. #1214 adds a runtime-resolved, configuration-wide maximum guest-usable SME SVL query before VM creation, outside VM/vCPU ownership and runner admission. #1216 adds a retained default-vCPU configuration query for raw CTR_EL0/CLIDR_EL1/DCZID_EL0 metadata under the same no-handle boundary. #1218 adds an independent retained default-vCPU query for the complete eight-entry data/unified and instruction CCSIDR arrays. #1220 adds a conditional macOS 15.2+ getter-only Z0-Z31 capture that preflights `PSTATE.SM`, uses maximum SVL only as an allocation width, and redacts all bytes. #1222 adds a separate conditional getter-only P0-P15 capture that derives each predicate width as maximum SVL divided by eight and redacts all bytes. #1224 adds a conditional getter-only ZA capture that requires `PSTATE.ZA` but not `PSTATE.SM`, checked-squares maximum SVL, and redacts bytes and dimensions. #1226 adds a separate conditional fixed 64-byte SME2 ZT0 capture under the same ZA-only preflight, without querying maximum SVL. #1228 adds ordered nontransactional restore of the complete typed general-register capture, with exact partial-write failure context. #1230 adds the paired restore for the complete typed SP_EL0/SP_EL1/ELR_EL1/SPSR_EL1 capture, with exact partial-write failure context. #1232 adds the paired restore for the complete typed AFSR0_EL1/AFSR1_EL1/ESR_EL1/FAR_EL1/PAR_EL1/VBAR_EL1 capture. #1234 adds the paired restore for the complete typed ACTLR_EL1/CPACR_EL1 capture. #1236 adds the paired restore for the complete typed TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture. #1238 adds the paired restore for the complete typed SCTLR_EL1/TTBR0_EL1/TTBR1_EL1/TCR_EL1/MAIR_EL1/AMAIR_EL1/CONTEXTIDR_EL1 capture. #1240 adds the paired restore for the complete typed Q0-Q31/FPCR/FPSR capture. #1242 adds the paired restore for the complete redacted APIA/APIB/APDA/APDB/APGA key state and forms a thirty-operation shared core-register admission domain. #1244 adds the paired restore for the complete redacted SCXTNUM_EL0/SCXTNUM_EL1 value and forms a thirty-one-operation shared core-register admission domain. #1246 adds the paired one-write restore for the complete CSSELR_EL1 selector and forms a thirty-two-operation shared core-register admission domain. #1248 adds paired IRQ-then-FIQ restore under generalized interrupt-operation admission without changing that core-register count. #1250 adds paired debug-exception-then-debug-register-access trap-policy restore and forms a thirty-three-operation shared core-register admission domain. #1252 adds paired MDCCINT-then-MDSCR debug-control restore and forms a thirty-four-operation shared core-register admission domain. #1255 adds independently loaded pre-first-run restore of the complete opaque GIC device blob under generalized interrupt admission. #1258 adds pre-first-run restore of nine mutable EL1 ICC registers plus derived-RPR validation in the same interrupt domain. Current native-v2 create/load and frozen native-v1 load use their production aggregate capture/restore commands; ordinary pause/resume and standalone lease diagnostics do not invoke the individual low-level operations. FDT idle-state discovery, non-timer suspend wake, dynamic CPU topology, generic optional-device snapshot-ready ownership, complete HVF state capture/restore, and fine-grained guest error exit-code parity remain deferred; peer-owned vmnet/vsock host/kernel buffers are explicitly outside snapshot state, and `SYSTEM_RESET` remains a terminal process outcome. |
| Snapshots and restore | current public native-v2 2.10 complete-serial Full/File multi-vCPU lifecycle with independently optional profile-3 storage, entropy, balloon, and virtio-mem implemented and verified; exact 2.3–2.9 and frozen native-v1 File/Uffd readers retained | unit, API socket, process e2e, fixed/hostile exact fixtures, signed HVF capture/reconstruction, all-sixteen-product resource/failure injection, signed direct and normal-production MMIO/PCI matrices, signed App Sandbox restore, docs | #543, #1263, #1264, #1390, #1395, #1396, #1441, #1525, #1526, #1528, #1529, #1554, #1575, #1576, #1577, #1578, #1583, #1584, #1585, #1586, #1587, #1588, #1589, #1609, #1610, #1611, #1612, #1613, #1614, #1615, #1616, #1617, #1634, #1651, #1652, #1665, #1666, #1680, #1681, #1697, #1698 | Public create admits Paused Full sources with 1–32 vCPUs, required serial, optional matching-transport profile-3 storage, entropy, balloon, and virtio-mem, and no network/vsock/MMDS/boot timer; it publishes current `2.10.0` memory first and state last. Kind 11 is a bounded 128-KiB component with geometry-derived canonical plugged bits closed against unchanged kind-1 extents. Load authorizes exact resources, maps base File/COW RAM privately, creates a fresh shared aperture and block-granular plugged views, and constructs fresh serial/entropy/balloon/virtio-mem owners before Paused publication. Signed direct and normal-production/App-Sandbox MMIO/PCI evidence proves byte-first virtio-mem continuation through partial UNPLUG, reprobe UNPLUG_ALL/replug, PLUG/final UNPLUG, normalized recapture, immutable same-process and fresh-process clones, exact metrics, malformed/cancellation/death cleanup, and containment alongside prior storage/serial/entropy/balloon continuation. The format remains bangbang-native and unauthenticated. Diff, native-v2 Uffd, network/vsock/MMDS restore, Firecracker artifacts, source owner/dirty identity, synchronous RSS, and broad portability remain unsupported or under audit. |
| Memory hotplug | live MMIO/PCI lifecycle plus exact native-v2 2.10 serialization, fresh mixed-memory restore, and signed continuation implemented and verified | unit, fixed/hostile codec/bitmap/extent/materialization tests, API/process, all-product controller/owner rollback, signed HVF, signed executable, signed production bundle, docs | #544, #942, #952, #1022, #1026, #1028, #1030, #1032, #1034, #1040, #1042, #1044, #1046, #1050, #1333, #1334, #1419, #1462, #1474, #1538, #1697, #1698 | Strict configuration/status and live STATE/PLUG/UNPLUG/UNPLUG_ALL operate in configured blocks with failure-atomic guest/HVF/dirty/accounting mutation and Firecracker-shaped metrics. Exact 2.10 kind 11 persists bounded configuration, features, config space, queue, transport, and canonical plugged topology, exactly binds kind-1 memory extents, and restores private base RAM plus one fresh shared aperture per destination. Focused coverage includes hostile geometry/bitmap/extents, all sixteen products, block-granular materialization, same-process peers, controller commit/cancellation, owner rollback, and recapture. Signed direct and contained MMIO/PCI sources and explicit/automatic destinations verify nonzero restored bytes first, then partial shrink, Linux reprobe UNPLUG_ALL/replug, growth, final removal, exact success/failure metrics, immutable inputs, pathname replacement, malformed state/memory, all death orders, and cleanup. No DELETE route or synchronous RSS claim is exposed; Diff/Uffd/network/vsock/MMDS restore and broad portability remain separate work. |
| RTC | implemented Firecracker aarch64 no-interrupt subset | unit, signed HVF, signed executable, docs | #544, #944, #1052, #1074 | A PL031 RTC is registered as MMIO during HVF startup and emitted with Firecracker's `arm,pl031` / `arm,primecell` FDT shape and no interrupt property. The backend-neutral handler implements the current-time, load, match, control, mask, no-interrupt status/clear, and PrimeCell identity register surface with fixed-width validation and Firecracker-shaped error metrics. Signed executable direct-rootfs coverage proves `/dev/rtc0` and PL031 discovery. Alarm interrupts are an explicit boundary of the same upstream no-interrupt aarch64 subset, not a missing parity item. |
| Time and identity devices | PL031, VMGenID, VMClock, public ARM PVTime accounting, and public native-v2 clone restore implemented | unit, signed HVF, signed executable, aggregate signed executable, signed production bundle, docs | #543, #544, #946, #1076, #1078, #1080, #1082, #1084, #1261, #1272, #1276, #1477, #1478, #1480, #1481, #1529, #1578 | Startup emits Firecracker-shaped PL031/VMGenID/VMClock/PVTime state. Native-v2 kind 6 carries destination-SystemTime PL031 reset, cumulative PVTime excluding snapshot downtime, fresh notified VMGenID, and saved-counter notified VMClock. Restore validates every guest destination before HVF publication, commits identity/time in order, and becomes terminal after the first committed write. Signed three-vCPU lower-layer and public repeated two-vCPU process evidence prove distinct clone identity, notification/time semantics, Paused-before-progress, continuation, and cleanup. Frozen native-v1 retains its exact VMGenID/VMClock compatibility behavior. Broad cross-host portability and optional-device composition remain Wave 6 work; aarch64 `clock_realtime` rejects like pinned Firecracker. |
| Remaining Firecracker devices | implemented supported subsets; transport and profile limits explicit | unit, api socket, process e2e, signed HVF, signed executable, signed production bundle, docs | #544, #797, #800, #802, #804, #806, #808, #810, #812, #814, #815, #818, #869, #873, #875, #877, #888, #890, #892, #894, #896, #898, #900, #902, #904, #905, #908, #910, #912, #914, #920, #922, #926, #928, #930, #932, #934, #936, #938, #940, #960, #962, #964, #968, #970, #972, #988, #990, #1000, #1002, #1016, #1018, #1024, #1329, #1328, #1330, #1331, #1335, #1336, #1337, #1338, #1362, #1418, #1419, #1420, #1421, #1422, #1444, #1473, #1474, #1475, #1477, #1478, #1479, #1480, #1529, #1634, #1651, #1652, #1665, #1666, #1680, #1681, #1697, #1698 | Serial implements exact native-v2 2.7 restore, pmem profile 3 from 2.6 through current 2.10, entropy optional 2.8, balloon optional 2.9 kind 10, and virtio-mem optional 2.10 kind 11 over MMIO/PCI. Virtio-mem restoration retains exact configuration/queue/bitmap/transport state, creates fresh mixed-memory and runtime owners, and has signed direct/contained byte-first topology continuation, clone, malformed, death, and cleanup evidence. Network/vsock/MMDS serialization, automatic guest hotplug notification, and externally certified vmnet connectivity remain explicit limits. |
| macOS isolation and platform limits | production App Sandbox worker, lifecycle v5 credential/resource-limit/vmnet policy, fixed-code/current-user jailer outcomes, exact rlimits, signed daemon ownership, typed startup grants, adopted file/socket/snapshot consumers, and separate fixed vsock plus contained vhost-user connection facets implemented; exact Linux seccomp/cgroup/network/PID mechanisms certified impossible; general brokerage incomplete | unit, docs, process e2e, signed App Sandbox HVF and process, signed production bundle | #545, #924, #1102, #1302, #1351, #1354, #1356, #1358, #1360, #1362, #1364, #1365, #1368, #1370, #1376, #1377, #1384, #1420, #1421, #1449 | The ordinary CLI remains uncontained. Production has a fixed unsandboxed launcher without App Sandbox/HVF authority and one separately signed nested worker whose default networkless profile has exactly App Sandbox plus Hypervisor; an explicit vmnet profile has exactly those claims plus documented vmnet and profile-derived application/team identifiers. Both use Hardened Runtime. Assembly remains private, inspected, no-clobber, exclusive, and explicitly excludes the integration-only grant probe. Suspended default-close spawn constructs a marker-only environment and retains standard streams plus fixed lifecycle-stream, grant-datagram, dormant vsock-broker, and dedicated vhost-user-broker endpoints. Static/live code validation, real/effective credentials/direct-parent PID and session identity, random SessionId/BatchId values, exact sequences, closed states, authenticated `Start(WorkerPolicy)`, exact soft/hard `RLIMIT_FSIZE`/`RLIMIT_NOFILE`, descriptor-entered private cwd, mandatory empty or populated atomic grant acknowledgment, and an independently validated empty namespace gate public work. Lifecycle v5 adds a canonical immutable host/shared/exact-bridge allowlist and separate 1-through-4 active vmnet maximum. Contained final InstanceStart enforces the complete non-MMDS-only set before resources/backend construction; direct mode is unchanged and all-MMDS needs no authority. Static/live validation accepts only the exact profile-absent networkless shape or exact five-key/profile-present vmnet shape, and binds positive authority only to vmnet. Vmnet publication requires bounded open-once profile capture, exact relationship and signing-leaf checks, and a disposable same-authorization current-host launch; this does not claim contained connectivity. The outer `--bangbang-jailer-v1` envelope binds the exact executable/current credentials, validates and injects ID/timing, applies last-value limits with default no-file 2048, and nests the unchanged grant envelope. Its pre-delimiter parser returns a closed fixed-name error for all exact/attached cgroup, network-namespace, and PID-namespace inputs before grants, profile/staging, sessions, spawn, publication, or worker execution; signed tests prove no value, output, socket, or session mutation. Same-code default-close `SETSID` re-exec with `/dev/null` and a closed Ready/PID/ack handoff provides daemon caller detach while retaining one supervisor; parent loss before ack cancels the unpublished session. Strict bounded manifests prepare no-follow existing resources before spawn; regular files use SCM_RIGHTS with exact access/type/device/inode checks, while four mutable-directory roles use fragmented one-session implicit bookmarks plus exact anchors and balanced scope; API/vsock/snapshot directories carry create-children access and vhost-user directories carry connect-only access. The worker exposes only a bounded redacted one-time typed registry after exact Commit; sender close is cleanup, not revocation. Contained config, metadata, kernel, initrd, and snapshot describe/state/memory consumers claim exact read-only roles. Block/pmem consumers bind repeatable exact IDs/access, retain opened backings through deferred startup, support failure-atomic same-ID replacement, and never reopen tags; live block or pmem insertion may consume only unused startup authority, while limiter-only updates retain ownership. Logger/metrics/serial consumers claim singleton exact-ID `WriteOnly` files after validation, preserve write-only access while normalizing append/nonblocking status, retain logger/metrics sinks, and move serial output once into startup. Snapshot load preinspects state without consuming it, classifies the family once, atomically adopts native-v2 state/memory, and for frozen native-v1 additionally adopts any persisted root. Snapshot create retains repeatable output anchors with bounded UTF-8 children, publishes no-clobber relative to those anchors, and records exact staging identities so launcher recovery after worker death removes only matching current-user regular `0600` single-link files while preserving replacements. API/vsock use the distinct exact `bangbang-grant:<GrantId>/<SocketChild>` grammar with a bounded one-component ASCII child, exact singleton role/access, owner-thread scope/anchor lifetime, and no ambient fallback. A short-lived default-close signed binder creates one fixed private staging listener and is reaped before exposure; the worker requires matching filesystems, publishes exclusively between exact anchors, supplies the listener to the API/runtime, records only strict role/child/socket identity, and removes only the matching vnode. Contained vsock host initiation uses the supplied main listener; guest initiation activates the dormant launcher facet once, then exchanges only monotonic ports and connected stream fds under exact peer/session/anchor/child/target checks, with no guest payloads or `network.client` entitlement. Contained vhost-user block retains an exact directory anchor by GrantId, leases exact children per drive, and uses a separate fixed 256-byte session/sequence/grant/child-bound launcher facet for bounded anchored connects; retryable failure restores a fresh claim, PATCH reuses the stream, and DELETE releases only the child lease. Signed Apple Silicon proof covers policy grammar/redaction, networkless vmnet rejection, environment and unexpected-fd closure, exact limits plus `EMFILE`/`SIGXFSZ`, private cwd, daemon readiness/concurrency/pre-ack parent loss/post-ack signal cleanup, malformed bootstrap, outside-path denial, typed grant rollback/cancellation/deadline behavior, crash/concurrent noninterchangeability, normal-build probe absence, external no-API plus delayed API startup, pathname replacement identity, authorized config reads, redacted failures, read-only block denial, writable block I/O, pmem read/flush, logger records, initial/terminal metrics, real guest serial bytes, concurrent output isolation, live block swap, outside-container API connectivity, both real granted-vsock initiation directions, two real contained vhost-user children sharing one connect-only grant with active PATCH and no steady-state helper, granted native-v2 create/describe/state-memory File/COW restore with retained descriptor identity, snapshot staging crash cleanup, unchanged entitlements, and real sandboxed HVF guests. Authentication remains asymmetric, and same-identifier workers share cooperative container cleanup authority. The snapshot staging create-before-record interval and simultaneous uncatchable process deaths can leave residue; App Sandbox may also deny descriptor-relative writes after the authorized output directory itself is moved. The exact Linux seccomp, cgroup, parent-cgroup, network-namespace, and PID-namespace identities are terminal platform exclusions rather than native aliases. Arbitrary uid/gid transition, configurable chroot ownership, general dynamic post-Ready brokerage, hard revocation, cross-filesystem socket publication, real vmnet start/connectivity/cleanup evidence and repository-owned approved credentials, launch constraints, Developer ID possession/notarization, and automatic restart remain #1351 work. |
| Frozen native-v1 baseline device state | compatibility-reader component | unit, signed HVF ownership, signed frozen-v1 File public dispatch, docs | #543, #1268, #1276, #1368, #1477, #1578 | Exact bounded `BANGDEV\0` state persists the one-root baseline. Public family dispatch retains direct/contained File and macOS Uffd preparation, exact root identity, time/identity restore, Paused publication, and optional resume. No public v1 writer selector remains. |
| Frozen native-v1 composite capture | compatibility/fixture component; public writer superseded | unit, supervisor ownership, signed HVF, docs | #543, #1270, #1276, #1390, #1396, #1578 | Kind-2 `BANGCMT\0` and five-component `BANGHVF\0` bytes remain exact for fixtures and lower-layer compatibility tests. Public Full create now emits native-v2; no profile-dependent v1 fallback or user-visible format selector is exposed. |
| Frozen native-v1 paused restore | public compatibility reader | unit, process lifecycle, signed HVF, signed frozen-v1 File public dispatch, pager component/containment evidence, docs | #543, #1272, #1276, #1368, #1396, #1477, #1554, #1578 | The one-open public family dispatcher transfers validated v1 state to the unchanged File/Uffd loader, preserves exact root/grant/deprecation/tracking/error semantics, commits Paused, and optionally resumes. File has signed real-HVF dispatch evidence; Uffd retains focused unit plus signed fault/pager/containment evidence. |
| Frozen native-v1 composite publication | internal compatibility/fixture component; public writer superseded | unit, process lifecycle, signed HVF, docs | #543, #1274, #1276, #1368, #1578 | The pathless staging writer and exact v1 transaction remain for immutable fixtures and regression coverage. Public native-v2 reuses the same no-clobber memory-first/state-last authority, while final v1 bytes remain unchanged. |
| Public native-v2 Full/File endpoint activation | implemented current 2.10 complete-serial subset with independently optional regular-file block/pmem profile-3 storage, entropy, balloon, and virtio-mem, plus exact native-v2 2.3–2.9 and frozen native-v1 readers | runtime, API socket, process lifecycle, signed direct executable and normal-production/App-Sandbox matrices, docs | #543, #1276, #1396, #1575, #1576, #1577, #1578, #1589, #1616, #1617, #1634, #1651, #1652, #1665, #1666, #1680, #1681, #1697, #1698 | Public Full create emits exact native-v2 2.10 with required serial and all sixteen optional storage/entropy/balloon/virtio-mem products on one transport. Load classifies one opened state, selects frozen native-v1 or exact 2.3–2.10 File/COW, performs exact backing authority adoption, materializes private base plus fresh shared virtio-mem views, and constructs fresh optional-device owners before Paused publication. There is no ambient fallback or override. Same-process peers and fresh-process destinations preserve immutable state/memory; signed direct and contained MMIO/PCI matrices prove exact grants, pathname replacement, normalized recapture, explicit/automatic resume, byte-first restored virtio-mem continuation, metrics, clone isolation, malformed/cancellation/death containment, and cleanup. |
| Complete shared dirty epochs | implemented and publicly activatable; Diff artifacts remain deferred | unit, API socket, process e2e, signed HVF, signed executable, docs | #1395, #1396, #1698 | One backend-neutral bitmap covers every current boot, VMM, device, discard, dynamic-memory, and exact owned guest-CPU writer. HVF accepts only lower-EL write faults with WnR set, S1PTW clear, and signed-observed translation DFSC `0x05`, `0x06`, or `0x07`, or level-three permission DFSC `0x0f`; CM may be clear for ordinary stores or set for observed Linux cache-maintenance writes. IPA ownership remains mandatory, and retry does not advance PC or dispatch MMIO. Host-dirty pages remain protected until the first guest write. New RAM is protected and wholly dirty; exact removal drops its metadata. Normal boot starts before population, tracked load starts after image population, and a visible Full commit performs failure-atomic coalesced re-protection before epoch clear/increment. Complete rollback retains the old conservative epoch; incomplete rollback poisons the paused VM and prevents resume. Signed evidence covers normal boot, VMGenID device writes, two vCPUs, two exact epochs, destination load override, restored block-granular virtio-mem, cancellation, and teardown. |
| Validation matrix maintenance | implemented | docs | #546 | Future capability PRs should update this matrix when support status or validation layers change. Full upstream Firecracker test-suite mapping remains deferred. |

#1616 and #1617 established the exact profile-2 block authority transaction.
#1634 extends the macOS containment claim without widening ambient authority:
exact native-v2 2.6 atomically adopts state/memory and resolves every inert
profile-3 block/pmem selector through one exact typed keyed transaction.
Missing, extra, reordered/swapped, cross-class aliased, wrong-access,
wrong-role/kind, wrong-size, changed-geometry, or consumed authority rejects
before construction without pathname fallback. The complete storage batch
commits only with Paused session/controller publication; signed production
coverage replaces all source pathnames after launcher adoption and proves
retained identity, active mixed I/O, shared writable external prefixes, zero
fresh private tails, redaction, death-order cleanup, and immutable
state/memory. #1651 activates exact native-v2 2.7 with one required complete
serial component and optional unchanged profile-3 storage. #1652 certifies
fresh default stdio and configured direct/contained output authority,
register/RX/pending-interrupt continuation, storage composition, repeated
clone loads, recapture, endpoint replacement resistance, redaction, and
cleanup without inheriting source descriptors or host-side buffered bytes.
#1665 activates exact native-v2 2.8 with optional entropy. #1666 certifies
exact queue, dual-limiter, pending/retry continuation through fresh MMIO/PCI
source/scheduler/notifier/route/endpoint/metrics owners at the direct and normal
production boundaries. #1680 activates exact 2.9 with optional balloon, and
#1681 certifies the direct and contained restored-guest continuation, limits,
clone isolation, failure, and cleanup contract. #1697 activates current exact
2.10 with optional virtio-mem, and #1698 certifies retained plugged bytes,
continued topology, clone isolation, failure, and cleanup through both signed
boundaries.

#1481 adds the aggregate remaining-device matrix gate across the capability,
snapshot, product-PCI, time/identity, and remaining-device rows above. The
checked selector contains exactly 85 identities (52 balloon, 19 memory-hotplug,
seven entropy, six serial, and one time/identity): 84 are terminal and only the
time/identity aggregate remains `audit-required` under Wave 6 #1490. Focused
validation pins the
balloon -> memory-hotplug -> entropy -> serial -> time/identity
snapshot-preflight order, failure short-circuiting, unchanged state/artifacts, retry, and
MMIO/PCI owner release/reuse. Signed executable validation composes the live
device set over both transports; signed production validation isolates two
default-stdio launcher/App-Sandbox-worker sessions through pause, FIFO input,
EOF, independent termination, and cleanup. The private capture-ready values are
not optional-device encodings, restored-device evidence, or portability proof
except where exact 2.7 serial, exact 2.8 entropy, exact 2.9 balloon, and current
2.10 virtio-mem contracts now supply public encoding and restored-guest
evidence. Wave 7 #1491 owns no record in this selector, and
at the #1481 checkpoint the global inventory remained 191/207/3/17.

#1496 adds the checked aggregate network/MMDS matrix gate. Its exact 35-record
selector composes focused API/runtime behavior, signed MMIO/PCI transport,
signed process hotplug and isolation, signed capture-ready traversal, contained
production ownership, and the credential-free blocked vmnet preflight. It
promotes 29 formerly audited rows, leaves 31 selector rows terminal, and retains
four exact #1378/#1490/#1491 handoffs. #1518 then promotes eight API/live rows
from the exact 14-record vsock ledger and retains six #1490 artifact/restore/
clone handoffs. At that checkpoint the global inventory was 228/170/3/17; a
blocked credential gate is not positive vmnet connectivity. #1546 subsequently
moves one snapshot-paging corpus to feasible-but-undelivered #1527 ownership,
so that checkpoint was 228/169/4/17. #1555 subsequently promotes the exact
snapshot-paging corpus, producing 229/169/3/17 at that checkpoint. #1578
promotes four public native-v2 process/snapshot records. #1634 promotes the
final two pmem storage composites. #1652 promotes the serial snapshot semantic
after exact 2.7 activation and certification. #1665 activates exact 2.8, and
#1666 promotes the two entropy artifact/restore records after direct and
normal-production certification. #1680 activates exact 2.9, and #1681 promotes
the two balloon aggregate records after matching signed certification. #1697
activates current exact 2.10, and #1698 promotes only the memory-hotplug corpus
and virtio-mem lifecycle aggregate after matching signed certification, so the
current inventory is 242/156/3/17.

#1389 completes the observable `PATCH /vm` API leaf: valid same-state
`Paused` and `Resumed` requests return success, require a retained process
session, skip another backend command and generation, preserve state, and still
record successful API-request latency. Runtime, API-socket, process, and signed
single-/dual-process tests cover the contract. Snapshot-ready quiescence remains
part of the broader partial lifecycle row and is tracked separately.

## Historical Prerequisite Landing Notes

The chronological notes below preserve the boundary at each prerequisite's
landing; they do not describe current support status. The #1270 composite row
supersedes their older statements that cache queries were necessarily
non-atomic, captured subsets lacked schema or
orchestration, and composite capture remained deferred. #1276 supersedes every
statement below that public endpoint activation or public snapshot load
remained deferred; those phrases record each slice's landing state rather than
current behavior. The matrix above is authoritative for current support.
Technical destination and optional-state limitations apply only where its
current rows retain them.

#1206 extends the lifecycle and snapshot rows with a sixteenth shared-core
capture: one runtime-resolved macOS 15.2+ getter observes mutable `PSTATE.SM`
and `PSTATE.ZA` without calling the setter. Unit coverage validates the C ABI,
all Boolean combinations, raw error propagation, fresh retry, bidirectional
admission, and cleanup; signed HVF coverage validates same-vCPU idle observation
or the exact documented unavailable result. Snapshot create invokes, persists,
and restores none of it. Maximum SVL, Z0-Z31, P0-P15, ZA, and ZT0 are captured separately;
feature and transition validation, schema, persistence, and
restore remain deferred.

#1208 extends the same rows with a seventeenth shared-core capture: raw macOS
15.2+ `SMCR_EL1`, `SMPRI_EL1`, and `TPIDR2_EL0` reads publish only after all
three succeed, and `Debug` redacts every value. Unit coverage validates exact
SDK ids, order, boundary values, every failure point and fresh retry,
bidirectional admission, abandonment, unwind, panic, and cleanup; signed HVF
coverage validates two idle same-vCPU captures without raw logging, writes,
maximum-SVL queries, SME data reads, or guest execution. Snapshot create
invokes, persists, and restores none of it; feature and writable-bit validation,
schema, persistence, and safe restore ordering remain deferred.

#1210 extends the same rows with an eighteenth shared-core capture: raw macOS
15.2+ `SCXTNUM_EL0` and `SCXTNUM_EL1` reads publish only after both succeed,
and `Debug` redacts both software context numbers. Unit coverage validates exact
SDK ids, EL0-then-EL1 order, boundary values, both failure points and fresh
retry, bidirectional admission, abandonment, unwind, panic, and cleanup; signed
HVF coverage validates two idle same-vCPU captures without raw logging, writes,
guest execution, reset assumptions, or compatibility inference. Snapshot create
invokes, persists, and restores none of it; interpretation, feature/destination
validation, schema, persistence, and safe restore ordering remain deferred.

#1214 extends the lifecycle and snapshot rows with a configuration-wide,
runtime-resolved macOS 15.2+ maximum guest-usable SME SVL query. The typed
`usize` value remains outside backend instance state, VM/vCPU ownership, runner
admission, boot sessions, and snapshot orchestration. Unit coverage validates
missing and present symbols, full-width `size_t` preservation, exact return and
operation behavior, the public accessor, and the non-target boundary. Signed
HVF coverage queries twice before VM creation without logging the value and
accepts only two successful equal observations or two exact `HV_UNSUPPORTED`
results. Snapshot create invokes, persists, and restores none of it; effective
SVL selection, feature/destination policy, ZT0 lane policy and ZA layout, schema, persistence,
and restore remain deferred.

#1216 extends the lifecycle and snapshot rows with raw macOS 11+
`CTR_EL0`/`CLIDR_EL1`/`DCZID_EL0` feature metadata from a fresh retained default
vCPU configuration. The query takes no backend instance, VM/vCPU handle, or
runner admission and does not change the configuration used for vCPU creation.
Unit coverage validates exact ids, arbitrary values, deterministic order, null
creation, every getter failure, operation errors, target behavior, accessors,
and success/error/unwind release. Signed HVF coverage compares two pre-VM
queries without logging raw values or performing selector, CCSIDR, maintenance,
or guest operations. This standalone query is not the distinct combined
startup source added by #1392; CCSIDR geometry is queried separately here, and
this raw diagnostic surface still defines no interpretation, destination
policy, schema, persistence, or restore behavior.

#1218 extends the same rows with two complete eight-entry raw data/unified and
instruction CCSIDR arrays from an independent fresh retained default vCPU
configuration. The query also takes no backend instance, VM/vCPU handle, or
runner admission and does not change live vCPU creation. Unit coverage
validates exact cache types, all sixteen arbitrary values, deterministic order,
null creation, both getter failures, exact operation errors, target behavior,
accessors, and success/error/unwind release. Signed HVF coverage compares two
pre-VM queries without logging raw values or performing selector, live CCSIDR,
ISB, maintenance, or guest operations. Snapshot create invokes, persists, and
restores none of it; the feature and geometry queries are not atomic, and
interpretation, masks, destination policy, schema, persistence, and restore
remain deferred.

#1220 extends the lifecycle and snapshot rows with a nineteenth shared-core
command that conditionally captures all streaming Z0-Z31 bytes on macOS 15.2+.
It preflights `PSTATE.SM`, queries maximum SVL only as the exact allocation
width, fallibly allocates one contiguous buffer, and publishes no value until
all 32 runtime-resolved getter calls succeed; `Debug` redacts every byte. Unit
coverage validates ABI and ids, inactive/size/allocation boundaries, exact
order and bytes, every getter failure and retry, bounded accessors, thirty-four-way
admission, abandonment, channel, panic, and cleanup. Signed HVF coverage accepts
only documented unavailability/inactivity or two complete idle-vCPU captures
without logging raw bytes or width, changing SME state, or executing the guest.
Both session forms expose capture, but snapshot create invokes, persists, and
restores none of it; P0-P15, ZA, and ZT0 are captured separately, while effective SVL,
feature/destination policy,
protected persistence, schema, restore ordering, orchestration, and multi-vCPU
association remain deferred.

#1222 extends the same rows with a twentieth shared-core command that
conditionally captures all streaming P0-P15 predicate bytes on macOS 15.2+.
It preflights `PSTATE.SM`, queries maximum SVL, requires a non-zero value
divisible by eight, fallibly allocates one contiguous buffer, and publishes no
value until all 16 runtime-resolved getter calls succeed; `Debug` redacts every
byte. Unit coverage validates ABI and ids, inactive/size/divisibility/allocation
boundaries, exact order and bytes, every getter failure and retry, bounded
accessors, thirty-four-way admission, abandonment, channel, panic, and cleanup.
Signed HVF coverage accepts only documented unavailability/inactivity or two
complete idle-vCPU captures without logging raw bytes or widths, changing SME
state, or executing the guest. Both session forms expose capture, but snapshot
create invokes, persists, and restores none of it; Z0-Z31 and ZA are captured
separately alongside ZT0, while effective SVL, feature/destination policy, inactive-
lane interpretation, protected persistence, schema, restore ordering,
orchestration, and multi-vCPU association remain deferred.

#1224 extends the same rows with a twenty-first shared-core command that
conditionally captures the complete SME ZA matrix on macOS 15.2+. It preflights
`PSTATE.ZA` without requiring `PSTATE.SM`, queries a non-zero maximum SVL,
checked-squares it, fallibly allocates the exact buffer, and publishes no value
until the single runtime-resolved getter succeeds; `Debug` redacts bytes and
dimensions. Unit coverage validates the exact ABI, both streaming-mode values
under active/inactive ZA, zero/overflow/allocation boundaries, exact bytes,
backend failure and retry, raw accessors, thirty-four-way admission,
abandonment, channel, panic, and cleanup. Signed HVF coverage accepts only
documented unavailability/inactivity or two complete idle-vCPU captures without
logging bytes or dimensions, changing SME state, or executing the guest. Both
session forms expose capture, but snapshot create invokes, persists, and
restores none of it; Z/P/ZT0 are captured separately, while effective SVL,
feature/destination policy, layout interpretation, protected persistence,
schema, restore ordering, orchestration, and multi-vCPU association remain
deferred.

#1226 extends the same rows with a twenty-second shared-core command that
conditionally captures the fixed 64-byte SME2 ZT0 register on macOS 15.2+. It
preflights `PSTATE.ZA` without requiring `PSTATE.SM`, performs no maximum-SVL
query, and publishes no value until one runtime-resolved getter succeeds through
a private 16-byte-aligned SDK-compatible output object; `Debug` redacts every
byte. Unit coverage validates the exact SDK ABI, 64-byte size and 16-byte
alignment, both streaming-mode values under active/inactive ZA, exact bytes,
backend failure and retry, fixed-size access, thirty-four-way admission,
abandonment, channel, panic, and cleanup. Signed HVF coverage accepts only
documented unavailability/inactivity or two complete idle-vCPU captures without
logging bytes, changing SME state, querying maximum SVL, or executing the guest.
Both session forms expose capture, but snapshot create invokes, persists, and
restores none of it; Z/P/ZA are captured separately, while setters/transitions,
SME2 feature/destination policy, lane interpretation, protected persistence,
schema, restore ordering, orchestration, and multi-vCPU association remain
deferred.

#1228 extends the same rows with the first owner-thread restore operation for a
captured architectural subset. It borrows the complete typed X0-X30/PC/CPSR
value, clones it into the runner command, and writes all 33 registers in
architectural order. The shared core-register admission domain is generalized
from capture to operation and now covers twenty-three mutually exclusive
operations. Hypervisor.framework provides no batch transaction: a typed error
reports the failed register, completed-write count, and backend source, so the
caller retains the complete value for a full retry or must discard the vCPU
before execution. Unit coverage exercises every failure position and retry,
exact ordering, owner-thread dispatch, thirty-four-way conflicts,
abandonment, channels, queued destruction, unwind, panic, shutdown, and both
boot-session forms. Signed HVF coverage restores and recaptures one complete
same-vCPU idle value twice without guest execution or raw-value logging.
Rollback, schema/deserialization, input and destination validation, wider-state
ordering, snapshot orchestration, and public snapshot load remain deferred.

#1230 extends the same rows with the paired owner-thread restore operation for
the complete typed SP_EL0/SP_EL1/ELR_EL1/SPSR_EL1 capture. It writes the four
raw values in capture order through the existing runner owner and expands the
shared core-register admission domain to twenty-four mutually exclusive
operations. Hypervisor.framework still provides no transaction: a reusable
typed system-register error reports the exact failed register, completed-write
count, and backend source, while retaining the caller's complete value for a
full retry or vCPU discard before execution. Unit coverage exercises every
failure position and full retry, exact ordering, owner-thread dispatch,
thirty-four-way conflicts, abandonment, channels, queued destruction, unwind,
panic, shutdown, and both boot-session forms. Signed HVF coverage restores and
recaptures one complete same-vCPU idle value twice without guest execution or
raw-value logging. Rollback, schema/deserialization, input and destination
validation, wider-state ordering, snapshot orchestration, and public snapshot
load remain deferred.

#1232 extends the same rows with the paired owner-thread restore operation for
the complete typed AFSR0_EL1/AFSR1_EL1/ESR_EL1/FAR_EL1/PAR_EL1/VBAR_EL1
capture. It writes the six raw values in capture order through the existing
runner owner and expands the shared core-register admission domain to
twenty-five mutually exclusive operations. The reusable typed system-register
error reports the exact failed register, completed-write count, and backend
source while retaining the caller's complete value for a full retry or vCPU
discard before execution. Unit coverage exercises every failure position and
full retry, exact ordering, owner-thread dispatch, thirty-four-way conflicts,
abandonment, channels, queued destruction, unwind, panic, shutdown, and both
boot-session forms. Signed HVF coverage restores the actual same-vCPU
guest-written capture twice, preserves implementation-defined AFSR readback,
and performs no post-restore guest execution or raw-value logging. Vector-table
memory, coherent exception validation, destination policy, rollback, schema,
wider ordering, snapshot orchestration, and public snapshot load remain
deferred.

#1234 extends the same rows with the paired owner-thread restore operation for
the complete typed ACTLR_EL1/CPACR_EL1 capture. It writes both raw values in
capture order through the existing runner owner and expands the shared core-
register admission domain to twenty-six mutually exclusive operations. The
reusable typed system-register error reports the exact failed register,
completed-write count, and backend source while retaining the complete value
for a full retry or vCPU discard before execution. Unit coverage exercises both
failure positions and full retry, exact ordering, owner-thread dispatch,
thirty-four-way conflicts, abandonment, channels, queued destruction, unwind,
panic, shutdown, and both boot-session forms. Signed HVF coverage restores the
same-vCPU guest-written EnTSO/FPEN capture twice without post-restore guest
execution or raw-value logging. The macOS 15 ACTLR boundary, optional CPACR
feature and destination validation, writable-bit policy, guest ISB transitions,
wider feature-state ordering, rollback, schema, snapshot orchestration, and
public snapshot load remain deferred.

#1236 extends the same rows with the paired owner-thread restore operation for
the complete typed TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture. It writes all three
raw values in capture order through the existing runner owner and expands the
shared core-register admission domain to twenty-seven mutually exclusive
operations. The reusable typed system-register error reports the exact failed
register, completed-write count, and backend source while retaining the
complete value for a full retry or vCPU discard before execution. Unit coverage
exercises all three failure positions and full retry, exact ordering,
owner-thread dispatch, thirty-four-way conflicts, abandonment, channels,
queued destruction, unwind, panic, shutdown, and both boot-session forms.
Signed HVF coverage restores the same-vCPU guest-written capture twice without
post-restore guest execution or raw-value logging. Pointer/address validation,
TPIDR2/SCXTNUM/CONTEXTIDR coordination, rollback, schema, wider context ordering,
snapshot orchestration, and public snapshot load remain deferred.

#1238 extends the same rows with the paired owner-thread restore operation for
the complete typed SCTLR_EL1/TTBR0_EL1/TTBR1_EL1/TCR_EL1/MAIR_EL1/AMAIR_EL1/
CONTEXTIDR_EL1 capture. It writes all seven raw values in capture order through
the existing runner owner and expands the shared core-register admission domain
to twenty-eight mutually exclusive operations. The reusable typed system-
register error reports the exact failed register, completed-write count, and
backend source while retaining the complete value for a full retry or vCPU
discard before execution. Unit coverage exercises all seven failure positions
and full retry, exact ordering, owner-thread dispatch, thirty-four-way
conflicts, abandonment, channels, queued destruction, unwind, panic, shutdown,
and both boot-session forms. Signed HVF coverage leaves the MMU disabled and
restores the actual same-vCPU guest-written capture twice, including
implementation-defined AMAIR readback, without post-restore guest execution or
raw-value logging. Translation-table memory, feature and destination validation,
barriers, TLB/cache maintenance, safe MMU transition ordering, rollback,
schema, wider state ordering, snapshot orchestration, and public snapshot load
remain deferred.

#1240 extends the same rows with the paired owner-thread restore operation for
the complete typed Q0-Q31/FPCR/FPSR capture. It adds one macOS arm64 C shim so
Clang can invoke the SDK's by-value SIMD vector setter while stable Rust passes
an ordinary 16-byte pointer, then writes all 34 fields in capture order through
the existing runner owner. The shared core-register admission domain expands to
twenty-nine mutually exclusive operations. A dedicated typed error distinguishes
the SIMD/FP and scalar register spaces and reports the completed-write prefix
and backend source while retaining the complete value for a full retry or vCPU
discard before execution. Unit coverage exercises all 34 failure positions and
full retry, exact ordering, owner-thread dispatch, thirty-four-way conflicts,
abandonment, channels, queued destruction, unwind, panic, shutdown, and both
boot-session forms. Signed HVF coverage restores the actual same-vCPU
non-streaming guest-written capture twice without post-restore guest execution
or raw-value logging. SVE/SME Q/Z alias ordering, feature and destination
validation, FPCR/FPSR writable-bit policy, protected persistence/zeroization,
rollback, schema, wider state ordering, snapshot orchestration, and public
snapshot load remain deferred.

#1242 extends the same rows with the paired owner-thread restore operation for
the complete redacted APIA/APIB/APDA/APDB/APGA pointer-authentication key
capture. It splits each `u128` into its low/high halves, writes all ten system
registers in capture order through the existing runner owner, and expands the
shared core-register admission domain to thirty mutually exclusive operations.
The reusable value-free system-register error reports the exact failed
register, completed-write count, and backend source while retaining the
caller's complete value for a full retry or vCPU discard before execution. Unit
coverage exercises all ten failure positions and full retry, exact pairing and
ordering, owner-thread dispatch, thirty-way conflicts, abandonment, channels,
queued destruction, unwind, panic, shutdown, redacted `Debug`, and both boot-
session forms. Signed HVF coverage restores and recaptures the visibly fake
same-vCPU guest-written keys twice without PAC execution, post-restore guest
execution, or raw-value logging. Feature/algorithm and destination validation,
zeroization, protected persistence, safe SCTLR enable ordering, rollback,
schema, wider state ordering, snapshot orchestration, and public snapshot load
remain deferred.

#1244 extends the same rows with the paired owner-thread restore operation for
the complete redacted SCXTNUM_EL0/SCXTNUM_EL1 system-context capture. It writes
both system registers in capture order through the existing runner owner and
expands the shared core-register admission domain to thirty-one mutually
exclusive operations. The reusable value-free system-register error reports
the exact failed register, completed-write count, and backend source while
retaining the caller's complete value for a full retry or vCPU discard before
execution. Unit coverage exercises both failure positions and full retry,
exact ordering, owner-thread dispatch, thirty-one-way conflicts, abandonment,
channels, queued destruction, unwind, panic, shutdown, redacted `Debug`, and
both boot-session forms. Signed HVF coverage restores and recaptures the first
same-vCPU idle capture twice without guest execution, reset-value assumptions,
compatibility inference, or raw-value logging. Interpretation, feature and
destination validation, protected persistence, TPIDR/CONTEXTIDR coordination,
rollback, schema, wider state ordering, snapshot orchestration, and public
snapshot load remain deferred.

#1246 extends the same rows with the paired owner-thread restore operation for
the complete typed CSSELR_EL1 cache-selection capture. It writes the selector
once through the existing runner owner and expands the shared core-register
admission domain to thirty-two mutually exclusive operations. The reusable
value-free system-register error reports the exact failed register, zero
completed writes, and backend source while retaining the caller's complete
value for a full retry or vCPU discard before execution. Unit coverage
exercises the one failure and full retry, exact owner-thread dispatch,
thirty-four-way conflicts, abandonment, channels, queued destruction, unwind,
panic, shutdown, and both boot-session forms. Signed HVF coverage restores and
recaptures the first same-vCPU idle selector twice without logging it, querying
CCSIDR, issuing ISB, performing cache maintenance, running the guest, or making
reset/topology/destination assumptions. Selector interpretation/validation, an
atomic cache feature/geometry manifest, ISB/dependent CCSIDR visibility,
maintenance, protected persistence, rollback, schema, snapshot orchestration,
and public snapshot load remain deferred.

#1248 extends the same rows with a paired owner-thread restore operation for
the complete typed CPU-level IRQ/FIQ pending capture. It writes IRQ then FIQ
under one command-owned generalized interrupt-operation admission guard without
changing the thirty-two-operation core-register count. A dedicated value-free
error reports the exact failed interrupt type, completed-write count, and
backend source while retaining the caller's complete value for a full retry or
vCPU discard before execution. Unit coverage exercises both failure positions
and full retry, exact ordering and values, every forward/reverse conflict,
abandonment, channels, queued destruction, unwind, panic, shutdown, and both
boot-session forms. Signed HVF coverage restores and recaptures a known
IRQ-only same-vCPU value twice after a FIQ-only mutation, then explicitly clears
both levels without a guest run. HVF clear-after-run behavior, GIC/device
composition, routing, delivery/EOI, automatic pre-run reassertion, persistence,
rollback, schema, multi-vCPU coordination, snapshot orchestration, and public
snapshot load remain deferred.

#1250 extends the same rows with paired owner-thread restore for the complete
typed Hypervisor.framework debug-exception/debug-register-access trap-policy
capture. It writes exception policy then register-access policy under one
command-owned core-operation guard and expands that domain to thirty-three
mutually exclusive operations. A dedicated value-free error reports the exact
failed host-policy operation, completed-write count, and backend source while
retaining the caller's complete value for full retry or vCPU discard before
execution. Unit coverage exercises both write failures and retry, exact Boolean
propagation and order, every forward/reverse conflict, abandonment, channels,
queued destruction, unwind, panic, shutdown, and both boot-session forms.
Signed HVF coverage restores and recaptures the original idle same-vCPU pair
twice without assuming or logging either Boolean, manufacturing a policy
change, altering guest debug controls/comparators, running guest instructions,
or executing the vCPU. Joint debug feature/security and destination policy,
wider guest/host debug ordering, persistence, rollback, schema, multi-vCPU
coordination, snapshot orchestration, and public snapshot load remain deferred.

#1252 extends the same rows with paired owner-thread restore for the complete
typed raw MDCCINT_EL1/MDSCR_EL1 debug-control capture. It writes MDCCINT then
MDSCR under one command-owned core-operation guard and expands that domain to
thirty-four mutually exclusive operations. The reusable value-free system-
register error reports the exact failed register, completed-write count, and
backend source while retaining the caller's complete value for full retry or
vCPU discard before execution. Unit coverage exercises both write failures and
retry, exact values and order, every forward/reverse conflict, abandonment,
channels, queued destruction, unwind, panic, shutdown, and both boot-session
forms. Signed HVF coverage restores and recaptures the original idle same-vCPU
pair twice without assuming or logging either register, manufacturing a control
change, altering comparator or host trap state, activating debug behavior, or
executing the vCPU. Feature/writable/status-bit and destination validation,
comparator and host-trap coordination, protected persistence, rollback, schema,
multi-vCPU coordination, snapshot orchestration, and public snapshot load
remain deferred.

#1255 extends the lifecycle and snapshot rows with paired pre-first-run restore
of #1178's complete opaque Hypervisor.framework GIC device blob. A setter-only
dynamic capability remains independent from capture, forwards the exact
non-empty pointer and `usize`/`size_t`, and preserves the original HVF result
without exposing bytes. The owner command clones the redacted value, shares
generalized interrupt admission, and atomically rejects any runner whose sticky
run lifetime has started. Unit coverage exercises empty/no-call, exact pointer
and size, backend provenance, completed and failed-run rejection, every
forward/reverse conflict, abandonment, channels, queued destruction, panic,
shutdown, and both boot-session forms. Signed HVF coverage captures and
reapplies the original same-VM blob before any run, then destroys the VM without
parsing, comparing, mutating, or logging bytes or executing the guest. EL1 ICC
restore remains separate, while host-update preflight, transactional recovery,
protected persistence, the cross-step no-run lease, schema, multi-vCPU
coordination, snapshot orchestration, and public snapshot load remain deferred.

#1258 extends the lifecycle and snapshot rows with paired pre-first-run restore
of #1180's complete ten-register EL1 ICC value. Independent getter and setter
capabilities load before mutation; nine architecturally mutable registers are
written in capture order, while derived read-only RPR is read and validated at
its original position. The typed value-free failure distinguishes write from
derived validation and reports the exact register, completed write prefix, and
backend source. Unit coverage exercises the ten-position sequence, every mutable
write failure, RPR read failure and mismatch, complete retry, the sticky never-
run gate, every interrupt-operation conflict, abandonment, channels, queued
destruction, unwind, panic, shutdown, and both boot-session delegates. Signed
HVF coverage applies the same-VM opaque blob first, restores the idle ICC value,
and proves two exact recaptures without guest execution or value logging.
Destination validation, host-update preflight, transactional recovery, protected
persistence, cross-step no-run leasing, composite orchestration, schema,
multi-vCPU coordination, and public snapshot load remain deferred.

#1261 extends the lifecycle, snapshot, and time/identity rows with an internal
native arm64 timer and VMGenID restore policy. One timer-domain owner command
normalizes virtual count and full-width physical comparator distance around a
single host-counter sample; a paired sticky-never-run command preflights every
destination field and the counter, strips ISTATUS, ignores TVAL, and applies ten
ordered nontransactional writes. Typed value-free errors report the failed
read/sample/write and completed write prefix; a complete retry recomputes
host-relative fields from a fresh sample. A pure native-v1 classifier rejects
CPACR-enabled SVE/SME, active PSTATE.SM/ZA, and enabled implemented breakpoint
or watchpoint controls without values.

The same slice adds backend-neutral VMGenID replacement that commits retained
metadata only after the complete distinct nonzero 16-byte guest write, plus
borrowed and owned HVF session methods that preflight runner/GIC capability and
assert the edge-rising SPI last. Signal failure is an explicit post-commit
partial stage. Unit coverage exercises wrapping arithmetic, control policy,
every preflight/write failure, fresh retry, admission/lifecycle cleanup,
optional-state precedence/redaction, random/zero/duplicate/write/signal
VMGenID stages, and exact memory/metadata ordering. Signed HVF coverage restores
timer state across destroyed source and fresh destination VMs, verifies shared
elapsed-counter invariants for disabled and armed/masked controls, and proves
both session forms update guest VMGenID bytes and metadata before successful
real SPI injection. The composite restore lease/schema, supervisor/public load
wiring, VMClock restore, guest-observed VMGenID handling, timer EOI policy,
active optional-state restore, and userspace secret rotation remain deferred.

#1296 extends the lifecycle validation foundation without changing public PSCI
support. One owner-thread retained virtual-timer wait derives an exact Mach
deadline from raw offset/control/CVAL state, rechecks an enabled guest-unmasked
timer, and sets its selected PPI before completion. Identity-bound condvar
cancellation composes with active-run batch exits: a canceled wait consumes its
own acknowledgement, while a timer-won race preserves the raw next-run exit
needed for coordinator cancellation debt. Unit coverage exercises wrapping and
timebase arithmetic, every owner/PPI failure, operation admission, mixed-batch
races, and shutdown. Signed HVF coverage proves due/future timers under both
HVF exit-mask states plus disabled/guest-IMASK cancel and shutdown without fixed
sleeps. At the #1296 boundary, PSCI `CPU_SUSPEND`, coordinator suspended
membership, SGI/SPI/direct IRQ/FIQ wake, and guest-visible discovery remained
deferred to #1295.

#1298 activates the narrow guest-facing layer above that foundation. Both
`CPU_SUSPEND` widths reserve an exact retained transaction without changing
`ON` affinity, and suspended members reuse ordinary coordinator generations
for interruptible timer waits. A due enabled, guest-unmasked virtual timer
publishes its PPI before deferred `SUCCESS`; wakeup/pause cancellation rearms
without X0 completion, while stop/shutdown/terminal drains synthesize no wake.
Unit coverage spans decoding, power conflicts, exact runner tokens, mixed/all
suspended scheduling, cancellation debt, and session teardown. The signed
two-vCPU bare guest proves CPU0 can observe CPU1 as `ON` while CPU1 makes no
post-call progress, then proves two real timer wakes preserve non-result
context and return success without fixed sleeps. FDT idle-state discovery,
SGI/SPI/direct IRQ/FIQ wake and powerdown resume remain deferred.

#1300 completes the dependency-ordered PSCI discovery layer after the power
calls are real. `PSCI_VERSION` reports 1.0, and one metadata table defines the
exact `PSCI_FEATURES` matrix plus immediate/coordinated availability; both
CPU_SUSPEND IDs return zero feature bits for original power-state format and
platform-coordinated mode. The retained
Firecracker v1.15.1 `arm,psci-0.2`/HVC FDT binding discovers that runtime
revision just as its KVM baseline does. `SMCCC_VERSION` reports 1.1 with the
mandatory minimum `SMCCC_ARCH_FEATURES` VERSION/self results; optional
architecture workarounds, SoC ID, KVM PV/vendor calls, and TRNG remain
unsupported. Unit coverage exhausts supported and excluded IDs, runner reads
and writes, unknown calls, and nonzero HVC immediates. A signed one-vCPU bare
guest stores 36 feature-query results plus both version and architecture
discovery results before terminating through SYSTEM_OFF without fixed sleeps.

#1566 adds the wire-format-neutral reviewed optional arm64 restore foundation
needed by native-v2. Five public macOS 15.2 SME setters are dynamically
resolved without raising the deployment target. One one-attempt, never-run
owner command validates exact DFR0 counts, SME version/identification/maximum
SVL, conditional inventory and widths, fresh disabled controls/PSTATE,
destination-local sparse defaults, and every Q/Z alias before mutation. It
disables breakpoint/watchpoint controls before comparator publication, applies
SME system registers before PSTATE and Z/P/ZA/ZT0, then restores authoritative
Q0-Q31, FPCR, and FPSR last. Every read/write failure reports redacted
family/stage/index/completed-write context, and any failed attempt permanently
prevents guest execution on that runner. Unit coverage exhausts static
rejections, sparse reads, operation failures, ordering, admission, channels,
panic, and execution poisoning; signed HVF coverage proves real same-owner
debug and active-SME restore/recapture on supported Apple Silicon. Native-v1
bytes and inactive-optional policy remain unchanged. The stable topology/PSCI
lifecycle follows below; native-v2 encoding, complete multi-vCPU aggregate
construction, and public snapshot reconstruction remain #1528 follow-up work.

#1567 adds a public, wire-format-neutral stable paused-topology graph and
capture/import transaction for the boot-vCPU session. The graph canonically
binds `1..=32` members to topology index and MPIDR, retains the virtual-timer
PPI, and distinguishes offline, runnable, and deferred PSCI
`CPU_SUSPEND32/64` members. Suspended members preserve the closed call
convention, X1-X3 arguments, and post-trap PC without exposing those values in
diagnostics. Capture requires a completed Paused barrier and cross-checks the
coordinator, PSCI power transactions, session records, runner-owned deferred
call, and architectural registers before publishing. Import validates all
topology and PPI facts plus the never-run readiness of every owner before
mutation, prepares fresh destination-local PSCI and runner identities, installs
suspended members in topology order, and publishes a coordinator born Paused
with no dispatch or cancellation debt.
Failures unwind installed calls in reverse order, retain every cleanup failure,
and consume the unpublished topology; explicit resume begins generation 1.
Unit coverage includes empty, maximum, oversized, malformed, offline,
runnable, both suspend conventions, token inequality, no pre-resume dispatch,
recapture equivalence, redaction, rollback, and shutdown behavior. Native-v1
and native-v2 bytes remain unchanged; native-v2 topology encoding, the complete
multi-vCPU state aggregate, and public snapshot reconstruction remain #1528
follow-up work.

## Update Rule

When a PR changes Firecracker-facing behavior, update this matrix if it changes
support status, adds or removes a validation layer, or moves work between
implemented, partial, deferred, recognized unsupported, or platform-limited
states.
