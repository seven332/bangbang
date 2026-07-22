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
| Direct pmem mapping and root boot | implemented for startup MMIO/PCI and non-root runtime PCI; complete capture-ready live state and aggregate live certification implemented; optional serialization/restore deferred | unit, process e2e, signed HVF capture equality, signed executable aggregate, signed production bundle aggregate, docs, pinned-source compare | #1439, #1444, #1448, #1471 | One reference-counted file/private-tail mapping is registered directly with HVF; there is no anonymous data copy or writeback path. Writable files use a shared host mapping. Read-only descriptors use a private write-capable host mapping required by HVF while guest permissions stay read-only, so accidental host writes are COW and signed guest writes fault without changing the backing. Flush synchronizes exactly the nonzero file prefix with `MS_SYNC`; the aligned tail is volatile. Exactly one ordinary block or pmem root is accepted before startup, pmem order selects `/dev/pmem<i>` plus `ro`/`rw`, and runtime root mutation remains rejected. Signed evidence covers pre-flush writable coherence, read-only protection, MMIO `ro` root, PCI `rw` root, exact teardown/reuse, and a normal contained root using the launcher-opened descriptor after pathname replacement with unchanged entitlements. #1471 certifies pmem together with Sync, Async, vhost-user, and virtio-mem through direct and contained product-PCI guests. Pmem remains outside ordinary RAM dirty epochs; exactly `corpus:pmem` and `semantic.storage:pmem-root-mapping-flush-and-state` stay with Wave 6 for optional-device serialization/restore. |
| Guest-memory backing profiles | anonymous default plus descriptor-backed boot RAM and stable virtio-mem aperture implemented; direct and contained vhost-user block support dynamic memory and aggregate storage certification | unit, real Unix stream/SCM_RIGHTS/pipe/kqueue, signed HVF, signed executable, signed production bundle, docs | #1439, #1441, #1443, #1444, #1445, #1449, #1462, #1471 | Ordinary-only VMs retain anonymous RAM. Vhost or virtio-mem startup selects unlinked sparse shared backing; virtio-mem reserves one complete deterministic aperture before block preparation, while only plugged views enter CPU/HVF mappings, FDT/current accounting, byte access, and dirty metadata. Every initial or runtime backend receives one immutable guest-ordered table containing boot RAM plus that aperture (at most three arm64 regions), can read/write currently unplugged bytes, and receives no unrelated mapping. This is an explicit trusted confidentiality/integrity/availability capability without Darwin memfd seals, a bundled backend, helper, or backend-policy claim. Exact offset discard follows committed unplug, backend death/DELETE drops only frontend clones, and shutdown drops the reservation after active views. Signed direct and contained MMIO/PCI guests prove both configuration orders, storage I/O across `0 -> 128 MiB -> 0`, exact stable table geometry, CONFIG refresh, Running/Paused runtime attach/delete/reuse, unchanged entitlements, no helper, and vhost snapshot rejection before artifacts. #1471 adds the single-session aggregate Sync/Async/vhost/pmem lifecycle and leaves only the two named Wave 6 pmem snapshot composites nonterminal. |
| Capability inventory enforcement | structural inventory implemented; capability audit in progress (191 implemented, 207 audit, 3 feasible-missing, 17 proven impossible) | unit, workspace CI, docs, pinned-source compare | #1348, #1349, #1420, #1421, #1422, #1423, #1444, #1445, #1446, #1447, #1448, #1449, #1461, #1462, #1471, #1473, #1474, #1475, #1479 | The machine-owned v1.16.0 source manifest records exact 26/38/44/152 Swagger identities, 23 configured executable arguments, three non-Swagger DELETE routes, public-tool operations/arguments, and an explicit source corpus. A separate human overlay owns every disposition and adds cross-leaf semantic records. Delivery validation permits honest `audit-required`/missing work; final mode rejects it. The 17 terminal exclusions cover strict runtime seccomp/jailer, exact Linux hugetlbfs `2M`, and narrow KVM/static CPU categories. #1420-#1423 promote runtime device hotplug; #1444-#1449 cover direct pmem, vhost, portable Async, and capture-ready storage; #1461/#1462 close block-special and vhost/virtio-mem delivery; #1471 promotes 28 directly owned storage records; #1473 promotes 50 directly owned balloon records; #1474 promotes 17 directly owned memory-hotplug API records; and #1475 promotes five directly owned entropy API records; #1479 promotes five directly owned serial API records. The checked storage, balloon, memory-hotplug, and entropy ledgers each leave exactly their two named Wave 6 optional-device composites nonterminal for encoding and restore; the serial ledger leaves its one semantic aggregate for encoding, endpoint reconstruction, and restore. Snapshot, vmnet, observability, tool, and final export owners otherwise remain nonterminal. |
| Offline seccompiler tool | implemented and verified for the complete pinned tool corpus, operation, and five arguments | unit, process e2e, independent cBPF interpreter, pinned Linux oracle, docs | #1382, #1383 | `seccompiler-bin` accepts the v1.16 target/input/output/basic/split interface, compiles exact `vmm`/`api`/`vcpu` policy semantics for x86_64 and aarch64, writes bitcode 0.6.9 combined output or exact raw split names, and applies Firecracker's 100,000-byte consumer cap. Bounded redacted no-follow input and descriptor-anchored owner-only transactional output reject special targets and preserve replacements. Fault tests cover each split publication boundary plus rollback, durability, and cleanup uncertainty. A pinned aarch64 Linux run compared 433,440 semantic cases with Firecracker v1.16. The tool does not install/enforce seccomp; VMM filter loading and process flags remain #1384. |
| Process CLI and API socket | 22 of 29 inventory records implemented and verified; two proven platform impossible; five cross-family/aggregate records remain under audit | unit, api socket, process e2e, signed process, signed App Sandbox process, signed production bundle, pinned-source compare | #536, #545, #1008, #1010, #1048, #1058, #1060, #1070, #1092, #1260, #1302, #1352, #1365, #1368, #1384, #1419 | The checked 23-argument contract proves 20 complete leaves plus the CLI/readiness and signal/exit/fd/cleanup semantics. It covers Unicode-alphanumeric IDs under the exact UTF-8 byte bound, non-negative HTTP/MMDS limits including zero runtime behavior, first-`--` end-of-options handling, `--boot-timer`, argument and bad-configuration exits, best-effort non-clobbering fd-table preallocation, fatal host signals and non-terminating `SIGPIPE`, API socket ownership, config-file/no-api readiness, cleanup, selected process-local MMDS/observability behavior, and bounded native `--describe-snapshot` inspection. Contained description adopts one exact read-only file grant without reopening its tag; direct inspection retains pathname behavior and Firecracker state artifacts remain explicitly incompatible. Direct API paths preserve no-clobber owner-only publication and identity cleanup; contained production recognizes only an exact granted directory plus safe child, serves a real outside-container client after anchored publication, and leaves that grant unconsumed in no-API mode. Exact `--enable-pci` now selects all-virtio PCI startup on supported macOS arm64/HVF hosts with pre-readiness platform probing and signed all-class MSI-X/I/O evidence. Both seccomp flags are terminal public-macOS exclusions with fixed pre-path/no-output/no-socket process evidence. `--snapshot-version`, the snapshot-containing identity/output semantic, aggregate run operation, and broad design/getting-started corpora remain `audit-required`. The lower-level App Sandbox test remains integration evidence rather than the production launcher containment claim. |
| Instance/version/config reads | implemented | unit, api socket, process e2e | #536 | `GET /`, `GET /version`, `GET /vm/config`, and `GET /machine-config` expose accumulated supported state for the current subset. Unsupported config sections are omitted until modeled. |
| Machine and boot configuration | Wave 2 foundations complete; later-wave snapshot/tool/dynamic-topology work remains partial; sizing/SMT/cache FDT, finite reviewed arm64 CPU-template policy, and dirty epochs complete; exact 2M/KVM/static-template execution platform-excluded | unit, api socket, process e2e, signed HVF, signed executable | #538, #1284, #1285, #1293, #1298, #1391, #1392, #1393, #1395, #1396, #1402, #1403, #1408 | Pre-boot machine config now has Firecracker-shaped defaults/replacement/partial-update/clear and empty-PATCH behavior, runtime-owned value-redacted semantic faults, deliberate aarch64 SMT-vCPU-memory/page precedence, exact `1..=32` vCPU and `1..=1,046,528` MiB configured-equals-realized bounds, transactional balloon compatibility, and defensive startup validation. Unlike Firecracker's accept/echo/later-truncate quirk above 1022 GiB, Bangbang rejects before storage. Exact Linux hugetlbfs `2M` is certified unavailable through public arm64 XNU/HVF; odd memory gets page compatibility first, while an otherwise valid request gets a stable pre-allocation platform fault. Alignment and 16-KiB IPA granules are not substitutes. Host-free-memory preflight is not promised. `track_dirty_pages` now enables one shared boot/VMM/device/guest-CPU page epoch before normal population, with protected dynamic mappings and failure-atomic Full-publication reset. Bounded/lossless custom CPU input, stronger duplicate/index checks, exact masks for eleven reviewed U64 arm64 identification registers plus ACTLR.EnTSO, U64 X/core, U128 Q, and U32 FP state, explicit little-endian Q transport, fail-closed U32 scalar transport, boot-reserved/banked-state policy, transactional static/custom/empty/`None`/omitted replacement, pending GET-visible `V1N1`, a pre-backend V1-source gate, mixed-width all-vCPU read-before-write, immediate readback, boot override precedence, whole-unpublished-VM cleanup, custom snapshot exclusion, and strict KVM capability/vCPU-feature platform faults are covered. A public pre-VM macOS 15.2 gate protects ZFR0/SMFR0, ACTLR filters are confined to EnTSO bit 1, and every other KVM/public-HVF register family has an exhaustive stable value-free classification. Boot source, kernel/initrd loading, FDT generation including arm64 `/chosen/linux,pci-probe-only` and 64-byte `/chosen/rng-seed`, strict pre-VM cache identity/host-fact reconciliation, split or unified L1 plus shared L2/L3 FDT nodes, direct-rootfs boot paths, ordered owner-thread vCPU topology, owning concurrent boot-session coordination with active-only batch cancellation, all-MPIDR FDT input, indexed PSCI CPU_ON/CPU_OFF/CPU_SUSPEND/level-0 affinity transitions, per-vCPU timer PPIs, internal signed Linux CPU1 execution, and public host-limited SMP startup are covered. Public `InstanceStart` admits `1..=min(32, host_max)` and keeps capacity/construction failures before session retention or `Running`. Signed executable proof configures two vCPUs through the API, observes independent pinned CPU0/CPU1 progress, offlines CPU1 through guest sysfs, brings the same owner back online, and observes resumed CPU1 progress without fixed sleeps. Signed HVF bare-guest proof retains CPU1 across two virtual-timer suspend cycles while CPU0 observes ON affinity. Signed HVF CPU-template proof captures a disposable in-memory baseline, applies all seven new IDs plus ACTLR.EnTSO within the mixed ID/X/core/Q/FP profile to two fresh owners, requires exact readback, captures primary boot precedence plus retained targets, and shuts both sessions down without raw output. Signed Linux proofs compare exact cache sysfs geometry with the retained model and compare baseline/custom CPU-template ID views per CPU without serializing raw values. Static named-template execution, public cpu-template-helper operations, multi-vCPU native-v1 snapshots, FDT idle-state discovery, non-timer suspend wake, dynamic CPU topology, and cross-host portability remain deferred or platform-excluded as recorded. |
| Product PCI and modern virtio-pci | all-virtio startup, aggregate runtime hotplug, aggregate live storage certification, and entropy capture-ready ownership implemented; optional-device persistence/restore deferred | unit, process e2e, signed HVF capture equality, signed executable aggregate, signed production bundle aggregate, docs, pinned-source compare | #1416, #1417, #1418, #1419, #1420, #1421, #1422, #1423, #1444, #1448, #1471, #1475 | The supported macOS arm64/HVF product path accepts exact `--enable-pci` after target/GIC-MSI preflight and selects one immutable all-virtio transport. It owns Firecracker-shaped segment-0 ECAM/BAR apertures, deterministic generation-bound slots/functions, one 512-KiB BAR per endpoint, exact fixed MSI-X demand plus worst-case runtime headroom, and independently revocable full-pool registries. Balloon, block, network, pmem, vsock, entropy, and virtio-mem publish in Firecracker order; legacy virtio MMIO nodes are suppressed while platform devices remain MMIO. Default startup remains all-virtio-MMIO with `pci=off`. The retained manager supports failure-atomic Running/Paused non-root block, pmem, and network PUT/DELETE under one 31-endpoint budget with manual guest coordination, serialized mutation, type-scoped IDs, generation-bound metrics, exact teardown, and capacity reuse. Runtime pmem preflights root/duplicate, shared endpoint, inventory, PCI function, BAR, MSI-X, dispatcher, and metrics capacity before any grant claim, direct open, or mapping. Signed aggregate direct and contained guests compose Sync, Async, vhost-user, pmem, virtio-mem, concurrent updates, serialized hotplug/reuse, exact configuration projection, and cleanup in one session. Signed entropy owner and executable tests additionally prove canonical PCI transport placement, dual-bucket throttling, quiesced capture traversal, resume, and repeated Linux reads. Native-v1 create runs complete storage and entropy preflight before rejecting PCI and before artifact mutation; load keeps its pre-mutation PCI rejection. Automatic guest notification, PCI serialization/restore, external vmnet certification, and KVM ITS identity remain deferred. |
| Drives and virtio-block | MMIO/PCI Sync and portable Async file/block-special lifecycle, vhost-user lifecycle, GET_ID, live PATCH, runtime PUT/DELETE, capture-ready handoff, and aggregate live certification implemented | unit, real regular/block file/pipe/kqueue and Unix stream/SCM_RIGHTS/shared mapping, fixed grant/control codecs, api socket, process e2e, signed HVF capture equality, signed executable aggregate, signed production bundle aggregate | #539, #916, #962, #992, #994, #996, #998, #1020, #1068, #1268, #1304, #1362, #1418, #1419, #1420, #1443, #1445, #1446, #1447, #1448, #1449, #1460, #1461, #1464, #1465, #1466, #1471 | File-backed drives accept an existing regular file or exact macOS block-special descriptor with default Sync or explicit portable Async over MMIO/PCI and direct/contained ownership. Direct block geometry and persistence use public Darwin ioctls; contained block-special control remains bound to the launcher's retained descriptor without ambient reopen. GET_ID derives the exact 20-byte Firecracker v1.16.0 decimal identity from opened metadata and updates atomically with a successful replacement. Vhost-user supplies a bounded frontend to an operator-trusted backend over an immutable boot-RAM/virtio-mem aperture and supports CONFIG refresh plus PCI insert/delete/reuse without claiming a shipped backend or backend policy. Focused and signed family gates cover cache, flush, limiting, replacement, hotplug, rollback, capture, teardown, and exact reuse. #1471 composes read-only Sync root, writable Sync, Async, vhost, pmem, and virtio-mem in direct and contained product-PCI sessions with concurrent disjoint PATCHes, serialized dynamic block/pmem reuse, persistence, final projection, backend-death or orderly cleanup, grant identity, and unchanged entitlements. Native-v1 stays regular-file/Sync-only; optional pmem serialization/restore remains with the two exact Wave 6 composites. |
| Network and MMDS | MMIO-default and all-PCI startup implemented; PCI-only Running/Paused PUT/DELETE implemented; portable packet semantics, direct-vmnet batching, bounded MMDS TCP sessions, and capture-ready state implemented | unit, api socket, process e2e, signed HVF, signed executable, signed production bundle, docs | #540, #962, #982, #1066, #1090, #1146, #1148, #1150, #1154, #1306, #1307, #1308, #1309, #1310, #1311, #1312, #1313, #1377, #1418, #1419, #1422, #1495, #1497, #1498, #1499, #1502, #1503 | Initial network config, guest-advertised MTU, independent RX/TX limiters, transactional runtime limiter updates, backend-neutral retry timing, per-session scheduling, process-local MMDS, instance-bound stateless AES-256-GCM v2 tokens, aggregate/per-interface metrics, multi-interface and multi-process MMDS isolation, and modern PCI startup are implemented and signed. Public PCI sessions now accept a new validated network ID/MAC in Running or Paused, prepare one independent MMDS-only or vmnet packet-I/O owner, publish generation-safe metrics plus an endpoint through the owner-thread transaction, and commit live config last. Bodyless DELETE coordinates reversible PCI teardown with exact packet-I/O take/stop/restore, then releases queues, callbacks/events, limiter deadline, metrics generation, MMDS detour or vmnet handle, slot/BAR/MSI-X/dispatcher resources, and live config. Default MMIO, duplicate ID/MAC, invalid/missing/capacity, contained authority, snapshot/shutdown admission, and injected failures are nonmutating or terminal when restoration is uncertain. Signed direct and normal networkless-production guests perform two rounds of PCI rescan, real MMDS exchange, sysfs removal, DELETE, Paused reuse of the same ID/MAC/BDF, and clean shutdown without vmnet entitlement; the production case also rejects a non-MMDS bridged insertion without mutation. Existing entries retain their startup resource class and state; contained vmnet accounting uses actual live vmnet entries, while MMDS-only entries require no authority. Production packaging still claims no repository credential or positive external vmnet start/connectivity result. Typed vmnet start-result reconciliation, global realized-MAC reservation, finite lifecycle deadlines, and terminal cleanup uncertainty are implemented under #1495 without positive external evidence. #1502 adds generation-scoped packet callbacks, a capacity-one owner wake bridge, exact-interface MMIO/PCI readiness before vCPU entry, one bounded RX batch per pass, publication-safe staged TX, explicit partial counts, preserved MMDS/limiter/result order, and disable/drain/stop retirement. #1503 adds the exact checksum/TSO/UFO feature matrix, bounded software normalization, transactional merged RX, raw/direct backend envelopes, exact partial-batch and spoof-observation semantics, and detailed limiter/backend latency metrics. Signed MMIO and PCI MMDS-only guests acknowledge every published feature, turn one bounded TCP request into multiple host packets, receive a validated 49152-byte merged response, and progress after an RX limiter retry. #1498 matches the pinned 36-byte AES-256-GCM token envelope, standard Base64, TTL and current-key rotation semantics, binds immutable instance ID as AAD, removes the active-token table, keeps failed first-use and rotation nonmutating, redacts key/AAD/token state, and adds signed peer-token `401` rejection while preserving own-token validity across two processes. #1499 replaces the manual detour with one bounded interface-local MMDS stack: exact speculative target ownership, ARP-first output, 30 connections, 100 resets, 2,500-byte request buffers, one response, MSS/window flow control, ordered streams, segmentation, ACK/FIN/RST progress, eviction, 1.2-second retransmission, and fifteenth-timeout reset. One output frame is retained until guest RX commit, and future protocol deadlines merge with limiter deadlines in the generation-safe MMIO/PCI scheduler. Signed guests renew v2 tokens, receive segmented 49,152-byte responses, deliberately lose an ACK, and observe retransmission. #1497 adds deterministic startup/runtime MMIO/PCI owner traversal; detached queue, feature, limiter, generation, metrics, backend, and MMDS identity; one exact TX retry; generation-aware callback quiescence; and explicit normalization of cached RX, peer packets, active TCP/ARP/output/timers, callbacks, handles, tokens, borrows, and absolute clocks for a fresh lossy destination. Signed MMDS v1/v2 process and HVF lifecycle coverage proves pause/capture/resume, equality, limiter state, runtime generation reuse, rollback, and teardown. Positive external vmnet connectivity, host firewalling, network encoding/restore, and aggregate record certification remain deferred. |
| Virtio-vsock | implemented supported live MMIO-or-PCI startup/Unix-socket subset | unit, api socket, process e2e, signed HVF, signed executable, signed production bundle, docs | #541, #984, #1322, #1323, #1324, #1365, #1419 | Repeatable pre-boot `PUT /vsock` and stable post-start rejection, guest-visible MMIO/FDT attachment, process-local Unix-listener ownership/inode-safe cleanup, 256 retained connections per initiation direction, bounded handshakes and RW queues, dynamic 64-KiB credit windows with wrapping counters, partial/full shutdown, delivery-based two-second request/shutdown cleanup, reset/error handling, distinct read/write wakeup interest, `EVENT_IDX`, no-op event notifications, and Firecracker-shaped aggregate metrics for the implemented queue/packet/byte/cleanup/failure surface are covered. Direct signed executable cases verify ≥1 MiB in each direction for guest- and host-initiated streams, both peers' write-half-close/EOF sequence, terminal cleanup, path/payload-redacted diagnostics, and independent two-stream exchanges. Contained mode atomically claims one exact vsock directory plus safe child, publishes and supplies the main listener, routes guest initiation through a fixed session-bound launcher port facet returning connected fds without guest payloads, and preserves the same guest routing/credit/shutdown model. Signed normal-production cases prove a real guest initiates two granted-port streams, a real host completes deterministic 1-MiB bidirectional and half-close/EOF traffic through the granted main listener, exact entitlements remain unchanged, and no helper survives startup. Indirect descriptors are a supported bangbang extension. PATCH, DELETE, runtime hotplug, broader CID routing, general performance/artifact parity, runtime PCI hotplug/vhost/KVM, broader muxer metrics, and full event payloads remain outside the live subset. Native-v1 snapshot UDS override, event-queue `TRANSPORT_RESET`, and post-restore RX gating are the stable #543 exclusions; the live subset is not classified as snapshot-compatible. |
| Observability: logger, metrics, serial | implemented supported process-local stdio subset; production telemetry and global durability profile-limited | unit, api socket, process e2e, signed process, signed HVF, signed executable, signed production bundle, docs | #542, #918, #982, #984, #986, #988, #990, #992, #1008, #1010, #1024, #1056, #1074, #1088, #1090, #1276, #1340, #1341, #1342, #1343, #1476, #1479 | Logger configuration/filtering, unrestricted request/action records without bodies, the bounded ten-per-five-second boot-timer callsite with recovery warning, best-effort delivery, and missed/rate-limited counters are covered. Metrics use successful-write interval deltas for every implemented API/logger/signal/UART/device count, byte, failure, error, limiter field, and block `sum_us`; startup timing, boot status, latest action latency, and block min/max/sample count are stores. Lower/new generations, keyed disappearance/reappearance, sparse absent families, ambiguous at-least-once replay, and bangbang's `metrics_flush_count: 1` extension are tested. Configuration is silent; one retained-session initial attempt, 60-second Running/Paused attempts, fallible explicit action, and one best-effort normal-terminal attempt have focused/process proof, while existing API/config-file signed scenarios now observe the additional post-exit line. Nullable nonblocking configured serial files/FIFOs, contained output grants, default nonblocking stdout plus terminal/FIFO stdin, token-bucket drops, and a portable 64-byte RX FIFO with DR/OE/RDA/FCR behavior are covered. The owner run loop performs capacity-bounded reads, full-FIFO disarm and guest-drain rearm, EOF/error detach, retryable GIC delivery, Running-only consumption, paused capture exclusion, final-owner terminal/flag restoration, complete redacted capture-ready state, shared UART deltas, cleanup, signed launcher/App Sandbox flow, and multi-process isolation. There is no public streaming API, fake zero-filled absent-device schema, process-global panic/fatal writer, or rotation/syslog/journald/tracing/remote telemetry. Bangbang-native v1 keeps its six-register serial encoding, rejects nonrepresentable live RX/status/intent state, restores a fresh output pipeline, and excludes host endpoints, public path, TX/RX bytes, limiter state, and counters; generalized encoding and endpoint reconstruction remain with Wave 6. |
| VM lifecycle and run-loop control | Wave 2 lifecycle foundation complete; generalized snapshot/device and dynamic-topology work remains partial | unit, api socket, process e2e, signed HVF, signed executable, docs | #537, #1293, #1298, #1284, #1158, #1160, #1162, #1164, #1166, #1168, #1170, #1172, #1174, #1176, #1178, #1180, #1182, #1184, #1186, #1188, #1190, #1192, #1194, #1196, #1198, #1200, #1202, #1204, #1206, #1208, #1210, #1212, #1214, #1216, #1218, #1220, #1222, #1224, #1226, #1228, #1230, #1232, #1234, #1236, #1238, #1240, #1242, #1244, #1246, #1248, #1250, #1252, #1255, #1258, #1261, #1276, #1389, #1390, #1408 | Host-limited public multi-vCPU `InstanceStart`, Running transition, retained boot worker status, runtime `PATCH /vm` pause/resume for the current process-owned boot worker, native-v1 load commit as `Paused` followed by optional ordinary resume, guest PSCI `SYSTEM_OFF`/`SYSTEM_RESET` process exits, PSCI `CPU_OFF` with same-owner `CPU_ON` re-entry, and non-success terminal process failures are covered. #1293 adds exact non-returning CPU_OFF token consumption, last-online denial, scheduler-before-power commit ordering, narrow `SCTLR_EL1` warm-entry reset, and signed Linux sysfs CPU1 offline/online proof through both internal and public startup paths. #1298 adds exact retained CPU_SUSPEND transactions, timer-PPI-before-success ordering, online affinity preservation, lifecycle cancellation/rearm, and signed two-cycle CPU1 context-retention proof. #1389 makes pause acknowledgement a topology-wide active-run barrier across every online vCPU; signed dual-process coverage proves independent CPU0/CPU1 progress stops and resumes while an isolated peer continues. Ordinary paused commands and auxiliary work remain mutable outside a snapshot transaction. #1160 adds a scoped supervisor admission barrier: earlier FIFO commands finish, later ordinary commands and resume reject during its scope, and shutdown invalidates it out of band. #1162 introduced acknowledged block/entropy retry quiescence inside that scope. #1390 failure-atomically includes PMEM and network, drains tokens only after all four schedulers acknowledge, preserves in-flight/deferred/deadline work, and holds the same worker transaction through artifact verification, synchronization, exclusive memory-first/state-last commit, and a post-publication hook. Pre-seal signal cancellation cleans owned staging; post-seal shutdown preserves the publisher's exact typed visibility result. Synchronous API/MMDS/controller and periodic work cannot interleave. #1164 adds an internal runner command that captures immutable X0-X30, PC, and CPSR values on the owning thread with explicit conflict admission. #1170 adds a separate raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 command and shares one failure-atomic core-register admission domain with general-register capture. #1182 adds raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 capture in the same domain. #1184 adds raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 capture in that domain. #1186 adds raw ACTLR_EL1 and CPACR_EL1 capture there, with a macOS 15 ACTLR boundary. #1172 adds baseline Q0-Q31, FPCR, and FPSR capture through the same admission, preserves every 128-bit Q value, and proves boundary values in signed HVF. #1174 adds CPU-level IRQ/FIQ get/set and failure-atomic capture under generalized interrupt-operation admission, distinct from GIC state. #1176 adds raw TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture as a fourth command in the shared core-register admission domain. #1178 adds stopped-runner capture of Hypervisor.framework's stable, versioned opaque GIC device blob except CPU system registers, sharing generalized interrupt admission. #1180 adds a separate failure-atomic owner-thread command for all ten EL1 ICC CPU-interface registers exposed by the current SDK in that same interrupt domain. #1166 adds a separate owner-thread command for an immutable raw HVF virtual-timer mask/offset pair and serializes it with individual timer operations; #1168 extends the same value, capture order, and admission domain with raw control/CVAL access. #1188 adds raw CNTKCTL_EL1, CNTP_CTL_EL0, and CNTP_CVAL_EL0 capture under generalized timer admission, with macOS 15 and GIC-before-vCPU prerequisites. #1212 extends that capture with raw CNTP_TVAL_EL0 without treating the signed relative view as stable or simultaneous with CVAL. #1190 adds redacted five-key APIA/APIB/APDA/APDB/APGA capture from all ten SDK halves in the shared core-register domain. #1192 adds guest-visible MIDR/MPIDR and baseline PFR/DFR/ISAR/MMFR compatibility metadata in the same domain. #1194 adds observation-only raw MDCCINT_EL1/MDSCR_EL1 debug-control capture in the same domain without changing debug or trap behavior. #1196 adds observation-only raw CSSELR_EL1 cache-selection capture there without changing or interpreting cache state. #1198 adds DFR0-counted observation-only capture of every implemented raw DBGBVR/DBGBCR hardware-breakpoint pair in the same core-register domain without writes, enablement, trap changes, or guest execution. #1200 adds the corresponding DFR0-counted raw DBGWVR/DBGWCR hardware-watchpoint capture under the same admission and observation-only constraints. #1202 adds observation-only capture of Hypervisor.framework's debug-exception and debug-register-access trap-policy booleans in that domain without changing host policy or conflating it with guest EL1 debug state. #1204 adds a separate macOS 15.2+ ZFR0/SMFR0 SVE/SME identification-metadata capture there without changing the baseline identification command or enabling SVE/SME. #1206 adds a runtime-resolved macOS 15.2+ getter-only capture of mutable `PSTATE.SM`/`PSTATE.ZA` in the same domain without calling the setter or reading SME data. #1208 adds redacted getter-only capture of raw macOS 15.2+ SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 in that shared domain without writes or SME data reads. #1210 adds redacted getter-only capture of raw macOS 15.2+ SCXTNUM_EL0 and SCXTNUM_EL1 in the same domain without writes or guest execution. #1214 adds a runtime-resolved, configuration-wide maximum guest-usable SME SVL query before VM creation, outside VM/vCPU ownership and runner admission. #1216 adds a retained default-vCPU configuration query for raw CTR_EL0/CLIDR_EL1/DCZID_EL0 metadata under the same no-handle boundary. #1218 adds an independent retained default-vCPU query for the complete eight-entry data/unified and instruction CCSIDR arrays. #1220 adds a conditional macOS 15.2+ getter-only Z0-Z31 capture that preflights `PSTATE.SM`, uses maximum SVL only as an allocation width, and redacts all bytes. #1222 adds a separate conditional getter-only P0-P15 capture that derives each predicate width as maximum SVL divided by eight and redacts all bytes. #1224 adds a conditional getter-only ZA capture that requires `PSTATE.ZA` but not `PSTATE.SM`, checked-squares maximum SVL, and redacts bytes and dimensions. #1226 adds a separate conditional fixed 64-byte SME2 ZT0 capture under the same ZA-only preflight, without querying maximum SVL. #1228 adds ordered nontransactional restore of the complete typed general-register capture, with exact partial-write failure context. #1230 adds the paired restore for the complete typed SP_EL0/SP_EL1/ELR_EL1/SPSR_EL1 capture, with exact partial-write failure context. #1232 adds the paired restore for the complete typed AFSR0_EL1/AFSR1_EL1/ESR_EL1/FAR_EL1/PAR_EL1/VBAR_EL1 capture. #1234 adds the paired restore for the complete typed ACTLR_EL1/CPACR_EL1 capture. #1236 adds the paired restore for the complete typed TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture. #1238 adds the paired restore for the complete typed SCTLR_EL1/TTBR0_EL1/TTBR1_EL1/TCR_EL1/MAIR_EL1/AMAIR_EL1/CONTEXTIDR_EL1 capture. #1240 adds the paired restore for the complete typed Q0-Q31/FPCR/FPSR capture. #1242 adds the paired restore for the complete redacted APIA/APIB/APDA/APDB/APGA key state and forms a thirty-operation shared core-register admission domain. #1244 adds the paired restore for the complete redacted SCXTNUM_EL0/SCXTNUM_EL1 value and forms a thirty-one-operation shared core-register admission domain. #1246 adds the paired one-write restore for the complete CSSELR_EL1 selector and forms a thirty-two-operation shared core-register admission domain. #1248 adds paired IRQ-then-FIQ restore under generalized interrupt-operation admission without changing that core-register count. #1250 adds paired debug-exception-then-debug-register-access trap-policy restore and forms a thirty-three-operation shared core-register admission domain. #1252 adds paired MDCCINT-then-MDSCR debug-control restore and forms a thirty-four-operation shared core-register admission domain. #1255 adds independently loaded pre-first-run restore of the complete opaque GIC device blob under generalized interrupt admission. #1258 adds pre-first-run restore of nine mutable EL1 ICC registers plus derived-RPR validation in the same interrupt domain. Public native-v1 create/load use the production aggregate capture/restore commands; ordinary pause/resume and standalone lease diagnostics do not invoke the individual low-level operations. FDT idle-state discovery, non-timer suspend wake, dynamic CPU topology, generic optional-device or multi-vCPU snapshot-ready ownership, complete HVF state capture/restore, and fine-grained guest error exit-code parity remain deferred; peer-owned vmnet/vsock host/kernel buffers are explicitly outside snapshot state, and `SYSTEM_RESET` remains a terminal process outcome. |
| Snapshots and restore | partial; public native-v1 baseline plus complete live storage, balloon, memory-hotplug, entropy, and serial capture handoff plus PL031/VMGenID/VMClock restore | unit, api socket, process e2e, signed HVF capture equality, signed executable preflight, docs | #543, #1048, #1072, #1086, #1158, #1160, #1162, #1164, #1166, #1168, #1170, #1172, #1174, #1176, #1178, #1180, #1182, #1184, #1186, #1188, #1190, #1192, #1194, #1196, #1198, #1200, #1202, #1204, #1206, #1208, #1210, #1212, #1214, #1216, #1218, #1220, #1222, #1224, #1226, #1228, #1230, #1232, #1234, #1236, #1238, #1240, #1242, #1244, #1246, #1248, #1250, #1252, #1254, #1255, #1258, #1260, #1261, #1263, #1264, #1268, #1270, #1272, #1274, #1276, #1390, #1395, #1396, #1448, #1473, #1474, #1475, #1477, #1479 | Public `PUT /snapshot/create` supports native-v1 `Full` only from a paused one-vCPU VM with exactly one regular read-only root drive, default serial, and no optional devices/MMDS. Before applying optional-profile rejection, it traverses all live startup/runtime MMIO/PCI block and pmem owners, transactionally prepares Async state, and validates exact balloon, memory-hotplug, entropy, and serial owner state. Entropy preflight retains one pending throttled descriptor and a host-time-free retry disposition only when it agrees with the quiesced scheduler; no random bytes, `Instant`, endpoint, or guest-memory borrow escapes. Serial preflight pairs exact external configuration with complete UART registers, RX bytes, status, and pending intents while excluding host descriptors, terminal state, pipe buffers, TX bytes, counters, locks, and wakeup handles. Typed optional-device rejection still occurs without grants, staging, schema change, or load promise. The accepted baseline runs aggregate capture and the no-clobber memory-first/state-last kind-2 publisher inside one FIFO worker transaction that quiesces block, PMEM, network, and entropy retry schedulers through verification, synchronization, commit, and the post-publication hook, then leaves the source paused. VMClock capture reads and validates the complete live 112-byte ABI in that owner boundary and new `BANGDEV\0` 1.1.0 state must match the bound memory image; legacy 1.0.0 derives the same value from memory. A tracked source advances only after a visible Full commit and successful transactional re-protection; pre-visible failures retain its epoch, while incomplete rollback latches terminal failure before resume without changing the committed artifact outcome. Public `PUT /snapshot/load` accepts a committed pair through `File` or the deprecated sole `mem_file_path` alias only in a pristine fresh process except logger/metrics, validates before construction, commits a real session as `Paused`, optionally installs a clean destination epoch after baseline population and before mapping, VMGenID replacement, or VMClock update, and optionally uses ordinary resume. Snapshot-specific typed execution faults, latency/deprecation metrics, retryable/terminal dispositions, pre-seal cancellation and post-seal shutdown completion, path/value redaction, staging cleanup, orphan/committed-uncertain outcomes, exact local compatibility, external root identity, normalized timers, aggregate architecture/GIC/ICC/pending restore, fresh destination PL031, and ordered post-restore VMGenID then VMClock notifications are covered. Only fully cleaned precommit failures retry; any time/identity mutation makes failure terminal. Synchronous API/MMDS/controller mutation and periodic work cannot interleave with create. Signed executable coverage synchronizes on a tiny guest's UART metric, publicly creates, terminates the source, restores the immutable pair into two fresh processes, exercises explicit and automatic resume, and allows guest PSCI shutdown only after both VMGenID halves change, a stable even VMClock sequence exposes changed disruption/generation counters, and destination RTC does not regress. The format is bangbang-native, not Firecracker-compatible or authenticated. `Diff`, UFFD, x86-style clock adjustment, overrides, writable/additional drives, optional-device encoding/restore, active optional architecture/debug state, EL2 GIC state, and cross-host portability remain unsupported. Peer-owned vmnet/vsock host/kernel buffers are neither frozen nor persisted, and multi-vCPU native-v1 artifacts remain unsupported. |
| Memory hotplug | implemented supported MMIO-or-PCI startup, live metrics, and capture-ready state; optional serialization excluded | unit, api socket, process e2e, signed HVF, signed executable, signed production bundle, docs | #544, #942, #952, #1022, #1026, #1028, #1030, #1032, #1034, #1040, #1042, #1044, #1046, #1050, #1333, #1334, #1419, #1462, #1474 | Pre-boot `PUT /hotplug/memory`, public requested/plugged status, runtime requested-size PATCH, config-generation signaling, and the one-queue virtio-mem device over the selected startup transport are implemented. Valid `STATE`, `PLUG`, `UNPLUG`, and `UNPLUG_ALL` requests operate in configured block units over complete guest ranges. Exact active guest/HVF mappings can be split or combined; with vhost, they are offset views of one retained full-aperture shared reservation. Backend mutation precedes ACK publication, device state commits only after guest-visible completion, and partial or late failures remap the same retained views in reverse order. Committed unplug performs exact-offset best-effort discard and removes current dirty/accounting state without changing the vhost table. One producer exposes Firecracker's exact 18 singleton `memory_hotplug` metrics plus separate interrupt, rollback, owner-cleanup, and teardown counters. Paused capture validates available/negotiated features, exact queue geometry and cursors, pending notification/interrupt state, compact plugged ranges, the reservation's opaque mapping identity, guest owners, actual HVF maps, dirty tracking/epoch, and active/offline/current byte accounting. Native-v1 create performs this preflight before artifact publication but writes no optional-device encoding. Focused tests cover adjacent sequential plugs, partial multi-block unplug, one request crossing the conceptual slot boundary, metrics/failure ordering, exact mapping capture, limits, dirty state, same-address rollback/reuse, backend death, and pause/run without claiming Firecracker's KVM slot identity. Signed direct and contained MMIO/PCI coverage proves Linux binds `virtio_mem`, storage remains live, and public requested/plugged size completes `0 -> 128 MiB -> 0`. No DELETE route is exposed. Runtime device deletion, broader public guest-memory accounting, and optional-device serialization/restore remain deferred. |
| RTC | implemented Firecracker aarch64 no-interrupt subset | unit, signed HVF, signed executable, docs | #544, #944, #1052, #1074 | A PL031 RTC is registered as MMIO during HVF startup and emitted with Firecracker's `arm,pl031` / `arm,primecell` FDT shape and no interrupt property. The backend-neutral handler implements the current-time, load, match, control, mask, no-interrupt status/clear, and PrimeCell identity register surface with fixed-width validation and Firecracker-shaped error metrics. Signed executable direct-rootfs coverage proves `/dev/rtc0` and PL031 discovery. Alarm interrupts are an explicit boundary of the same upstream no-interrupt aarch64 subset, not a missing parity item. |
| Time and identity devices | PL031, VMGenID, VMClock, and public ARM PVTime live/capture-ready accounting implemented | unit, signed HVF, signed executable, aggregate signed executable, docs | #543, #544, #946, #1076, #1078, #1080, #1082, #1084, #1261, #1272, #1276, #1477, #1478, #1480, #1481 | Startup emits Firecracker-shaped DeviceTree VMGenID and VMClock nodes, initializes a nonzero generation and complete typed 112-byte VMClock ABI, and allocates deterministic SPI lines. Native-v1 capture records the live ABI as `BANGDEV\0` 1.1.0 under paused ownership while retaining 1.0.0 memory-derived loads. Restore reconstructs no-alarm PL031 from destination wall clock, completes aggregate runner/GIC state, writes/notifies a fresh VMGenID, then publishes/notifies the fenced VMClock sequence and counter update before resume. Clean precommit failures may retry; every committed write/notification failure is terminal. Signed two-destination guest polling proves both VMGenID halves, stable even VMClock sequence and counters, non-regressing RTC, and continuation. #1478 adds the aligned per-vCPU ABI and owner-thread HVF measurement primitive. #1480 adds bounded runnable wall-minus-execution accounting, dirty-aware atomic publication, fail-closed 64-bit SMCCC discovery, topology-ordered pause-gated capture values, and signed Linux proof of `stolen time PV`, nonzero monotonic contention steal, unchanged idle accounting, and unchanged paused captures. #1481 composes PL031/VMGenID/VMClock/PVTime with the remaining live devices over MMIO and product PCI and proves capture-ready ordering and resource reuse. PVTime artifact restore and clone/portability behavior remain with Wave 6 #1490. Firecracker v1.16.0's ACPI HID correction and x86 KVM clock adjustment are not applicable to this aarch64 DeviceTree/HVF device. |
| Remaining Firecracker devices | implemented supported subsets; transport and profile limits explicit | unit, api socket, process e2e, signed HVF, signed executable, signed production bundle, docs | #544, #797, #800, #802, #804, #806, #808, #810, #812, #814, #815, #818, #869, #873, #875, #877, #888, #890, #892, #894, #896, #898, #900, #902, #904, #905, #908, #910, #912, #914, #920, #922, #926, #928, #930, #932, #934, #936, #938, #940, #960, #962, #964, #968, #970, #972, #988, #990, #1000, #1002, #1016, #1018, #1024, #1329, #1328, #1330, #1331, #1335, #1336, #1337, #1338, #1362, #1418, #1419, #1420, #1421, #1422, #1444, #1473, #1474, #1475, #1477, #1478, #1479, #1480 | Balloon implements validated inflate/deflate with publication-safe compact paired PFN accounting, statistics polling and exact optional-field omission, hinting and reporting, best-effort Darwin discard, metrics, target-to-zero convergence, and bounded capture-ready MMIO/PCI owner state. Signed HVF proves exact paused owner traversal; signed public Linux guests prove live MMIO/PCI execution, pause/capture/resume, automatic hint acknowledgement, reporting, and cleanup. Balloon serialization/restore remains an exact Wave 6 handoff. Pmem implements selected-transport direct mapping, exact-prefix targeted rate-limited flush, deterministic root boot, exact contained backing ownership, and PCI-only transactional Running/Paused non-root PUT/DELETE with generation-safe metrics, failure rollback, and exact ID/slot/guest-range reuse. Signed HVF proves writable backing visibility before flush and read-only guest protection; signed root guests cover MMIO `ro`, PCI `rw`, and normal contained descriptor identity after pathname replacement. Network implements selected-transport startup and PCI-only runtime PUT/DELETE with independent limiter/retry/metrics/MMDS or vmnet ownership, coordinated teardown, and exact ID/MAC/slot reuse; networkless guests prove real MMDS exchange. Hidden and signed product PCI gates reuse the canonical device implementations. Entropy implements the 64-KiB request cap, 64-byte aarch64 seed, host OS source, optional dual-bucket limiting, exact single-descriptor retry, seven metrics, and detached capture-ready MMIO/PCI state. Failed post-consumption publication restores exact limiter state; signed owner evidence validates transport placement and scheduler agreement, while signed Linux guests prove marker-gated reads, throttling, pause/capture/resume, retry, and cleanup on both transports. Entropy serialization/restore remains an exact Wave 6 handoff. Serial implements default nonblocking stdout, terminal/FIFO stdin, configured-output stdin exclusion, bounded readiness/backpressure, retryable GIC delivery, Running/Paused behavior, metrics, final-owner descriptor restoration, and complete detached MMIO capture-ready state with signed direct and production-bundle proof. Serial artifact encoding and destination endpoint reconstruction remain an exact Wave 6 handoff. PL031/VMGenID/VMClock restore and public PVTime live/capture-ready accounting are implemented under the checked time/identity contract. Optional-device serialization/restore, pmem dirty-state capture, PVTime artifact restore/clone behavior, automatic PCI notification, and externally certified vmnet connectivity remain explicit limits. |
| macOS isolation and platform limits | production App Sandbox worker, lifecycle v5 credential/resource-limit/vmnet policy, fixed-code/current-user jailer outcomes, exact rlimits, signed daemon ownership, typed startup grants, adopted file/socket/snapshot consumers, and separate fixed vsock plus contained vhost-user connection facets implemented; exact Linux seccomp/cgroup/network/PID mechanisms certified impossible; general brokerage incomplete | unit, docs, process e2e, signed App Sandbox HVF and process, signed production bundle | #545, #924, #1102, #1302, #1351, #1354, #1356, #1358, #1360, #1362, #1364, #1365, #1368, #1370, #1376, #1377, #1384, #1420, #1421, #1449 | The ordinary CLI remains uncontained. Production has a fixed unsandboxed launcher without App Sandbox/HVF authority and one separately signed nested worker whose default networkless profile has exactly App Sandbox plus Hypervisor; an explicit vmnet profile has exactly those claims plus documented vmnet and profile-derived application/team identifiers. Both use Hardened Runtime. Assembly remains private, inspected, no-clobber, exclusive, and explicitly excludes the integration-only grant probe. Suspended default-close spawn constructs a marker-only environment and retains standard streams plus fixed lifecycle-stream, grant-datagram, dormant vsock-broker, and dedicated vhost-user-broker endpoints. Static/live code validation, real/effective credentials/direct-parent PID and session identity, random SessionId/BatchId values, exact sequences, closed states, authenticated `Start(WorkerPolicy)`, exact soft/hard `RLIMIT_FSIZE`/`RLIMIT_NOFILE`, descriptor-entered private cwd, mandatory empty or populated atomic grant acknowledgment, and an independently validated empty namespace gate public work. Lifecycle v5 adds a canonical immutable host/shared/exact-bridge allowlist and separate 1-through-4 active vmnet maximum. Contained final InstanceStart enforces the complete non-MMDS-only set before resources/backend construction; direct mode is unchanged and all-MMDS needs no authority. Static/live validation accepts only the exact profile-absent networkless shape or exact five-key/profile-present vmnet shape, and binds positive authority only to vmnet. Vmnet publication requires bounded open-once profile capture, exact relationship and signing-leaf checks, and a disposable same-authorization current-host launch; this does not claim contained connectivity. The outer `--bangbang-jailer-v1` envelope binds the exact executable/current credentials, validates and injects ID/timing, applies last-value limits with default no-file 2048, and nests the unchanged grant envelope. Its pre-delimiter parser returns a closed fixed-name error for all exact/attached cgroup, network-namespace, and PID-namespace inputs before grants, profile/staging, sessions, spawn, publication, or worker execution; signed tests prove no value, output, socket, or session mutation. Same-code default-close `SETSID` re-exec with `/dev/null` and a closed Ready/PID/ack handoff provides daemon caller detach while retaining one supervisor; parent loss before ack cancels the unpublished session. Strict bounded manifests prepare no-follow existing resources before spawn; regular files use SCM_RIGHTS with exact access/type/device/inode checks, while four mutable-directory roles use fragmented one-session implicit bookmarks plus exact anchors and balanced scope; API/vsock/snapshot directories carry create-children access and vhost-user directories carry connect-only access. The worker exposes only a bounded redacted one-time typed registry after exact Commit; sender close is cleanup, not revocation. Contained config, metadata, kernel, initrd, and snapshot describe/state/memory consumers claim exact read-only roles. Block/pmem consumers bind repeatable exact IDs/access, retain opened backings through deferred startup, support failure-atomic same-ID replacement, and never reopen tags; live block or pmem insertion may consume only unused startup authority, while limiter-only updates retain ownership. Logger/metrics/serial consumers claim singleton exact-ID `WriteOnly` files after validation, preserve write-only access while normalizing append/nonblocking status, retain logger/metrics sinks, and move serial output once into startup. Snapshot load preinspects state without consuming it, discovers a persisted grant-tagged read-only root, atomically adopts state/memory/root, and completes from supplied files. Snapshot create retains repeatable output anchors with bounded UTF-8 children, publishes no-clobber relative to those anchors, and records exact staging identities so launcher recovery after worker death removes only matching current-user regular `0600` single-link files while preserving replacements. API/vsock use the distinct exact `bangbang-grant:<GrantId>/<SocketChild>` grammar with a bounded one-component ASCII child, exact singleton role/access, owner-thread scope/anchor lifetime, and no ambient fallback. A short-lived default-close signed binder creates one fixed private staging listener and is reaped before exposure; the worker requires matching filesystems, publishes exclusively between exact anchors, supplies the listener to the API/runtime, records only strict role/child/socket identity, and removes only the matching vnode. Contained vsock host initiation uses the supplied main listener; guest initiation activates the dormant launcher facet once, then exchanges only monotonic ports and connected stream fds under exact peer/session/anchor/child/target checks, with no guest payloads or `network.client` entitlement. Contained vhost-user block retains an exact directory anchor by GrantId, leases exact children per drive, and uses a separate fixed 256-byte session/sequence/grant/child-bound launcher facet for bounded anchored connects; retryable failure restores a fresh claim, PATCH reuses the stream, and DELETE releases only the child lease. Signed Apple Silicon proof covers policy grammar/redaction, networkless vmnet rejection, environment and unexpected-fd closure, exact limits plus `EMFILE`/`SIGXFSZ`, private cwd, daemon readiness/concurrency/pre-ack parent loss/post-ack signal cleanup, malformed bootstrap, outside-path denial, typed grant rollback/cancellation/deadline behavior, crash/concurrent noninterchangeability, normal-build probe absence, external no-API plus delayed API startup, pathname replacement identity, authorized config reads, redacted failures, read-only block denial, writable block I/O, pmem read/flush, logger records, initial/terminal metrics, real guest serial bytes, concurrent output isolation, live block swap, outside-container API connectivity, both real granted-vsock initiation directions, two real contained vhost-user children sharing one connect-only grant with active PATCH and no steady-state helper, granted native-v1 create/describe/root-bound restore, snapshot staging crash cleanup, unchanged entitlements, and real sandboxed HVF guests. Authentication remains asymmetric, and same-identifier workers share cooperative container cleanup authority. The snapshot staging create-before-record interval and simultaneous uncatchable process deaths can leave residue; App Sandbox may also deny descriptor-relative writes after the authorized output directory itself is moved. The exact Linux seccomp, cgroup, parent-cgroup, network-namespace, and PID-namespace identities are terminal platform exclusions rather than native aliases. Arbitrary uid/gid transition, configurable chroot ownership, general dynamic post-Ready brokerage, hard revocation, cross-filesystem socket publication, real vmnet start/connectivity/cleanup evidence and repository-owned approved credentials, launch constraints, Developer ID possession/notarization, and automatic restart remain #1351 work. |
| Native-v1 baseline device state | public baseline component | unit, signed HVF ownership, signed executable, signed production bundle, docs | #543, #1268, #1276, #1368, #1477 | Exact bounded `BANGDEV\0` state persists one read-only root transport, queue/cursor/interrupt state, frozen limiter/retry time, UART registers, VMGenID/VMClock topology, and in 1.1.0 the validated complete VMClock ABI. Direct load reopens the root regular file read-only/no-follow; contained load instead adopts the persisted exact read-only `DriveBacking` descriptor, and both require the complete captured identity before installing drop-safe resources without boot writes. Version 1.1.0 requires ABI/memory agreement; legacy 1.0.0 derives it from bound memory. Public signed coverage exercises both paths and time/identity restore across fresh processes; optional devices remain deferred. |
| Native-v1 composite capture | public baseline component | unit, supervisor ownership, signed HVF, signed executable, docs | #543, #1270, #1276, #1390, #1396 | Kind-2 `BANGCMT\0` binds memory to the exact five-component `BANGHVF\0` baseline. Aggregate runner admission plus the supervisor's block/PMEM/network/entropy transaction spans encoding, cancellable memory streaming, verification/synchronization, exclusive commit, and the post-publication hook. Public Full create invokes this path; pre-seal cancellation and other recoverable failures leave the source paused/retryable, while post-seal shutdown preserves exact artifact visibility. For a tracked source, the same held transaction resets protection and the shared bitmap only after visible publication; recoverable reset failure retains the conservative epoch and poisoned rollback terminates safely. Optional profiles and portability beyond exact local compatibility remain deferred. |
| Native-v1 paused restore | public baseline component | unit, process lifecycle, signed HVF, signed executable, signed production bundle, docs | #543, #1272, #1276, #1368, #1396, #1477 | Public File load validates the committed pair/platform/cache/root before fresh VM construction, installs the baseline runtime without boot writes, performs exact never-run architecture/GIC/ICC/timer/pending restore, optionally installs tracking after baseline population and before mapping/owners, reconstructs fresh PL031, then writes/notifies VMGenID followed by VMClock before committing a real `Paused` session and optional ordinary resume. Contained load preinspects state, atomically adopts exact state/memory/root inputs, and never reopens a tag. Only fully cleaned precommit failures retry; any time/identity write or notification commit is terminal. Optional profiles, overrides, and Firecracker artifact compatibility remain unsupported. |
| Native-v1 composite publication | public baseline component | unit, process lifecycle, signed HVF, signed executable, signed production bundle, docs | #543, #1274, #1276, #1368 | The pathless move-only staging writer, closure proof, output/binding match, barriers, and exclusive memory-first/state-last renames back public Full create. Contained output grants use retained exact anchors and strict crash-cleanup records. Existing finals are not replaced; producer failures clean private staging, late memory finals remain typed orphans, and state-directory sync uncertainty remains committed success. |
| Native-v1 public endpoint activation | implemented narrow subset | runtime, API socket, process lifecycle, signed executable, docs | #543, #1276, #1396 | Public Full create and File load route through the production transactions. Load commits `Paused` before either returning or applying `resume_vm`; destination tracking through `track_dirty_pages` or deprecated `enable_diff_snapshots`, source Full reset, deprecated `mem_file_path`, metrics, latency, redaction, collision, retryable/terminal errors, explicit resume, automatic resume, VMGenID replacement, and cross-process continuation are covered. |
| Complete shared dirty epochs | implemented and publicly activatable; Diff artifacts remain deferred | unit, API socket, process e2e, signed HVF, signed executable, docs | #1395, #1396 | One backend-neutral bitmap covers every current boot, VMM, device, discard, dynamic-memory, and exact owned guest-CPU writer. HVF accepts only lower-EL write faults with WnR set, CM/S1PTW clear, and signed-observed DFSC `0x07` on initial protection or `0x0f` after re-protection; IPA ownership remains mandatory and retry does not advance PC or dispatch MMIO. Host-dirty pages remain protected until the first guest write. New RAM is protected and wholly dirty; exact removal drops its metadata. Normal boot starts before population, tracked load starts after image population, and a visible Full commit performs failure-atomic coalesced re-protection before epoch clear/increment. Complete rollback retains the old conservative epoch; incomplete rollback poisons the paused VM and prevents resume. Signed evidence covers normal boot, VMGenID device writes, two vCPUs, two exact epochs, destination load override, cancellation, and teardown. |
| Validation matrix maintenance | implemented | docs | #546 | Future capability PRs should update this matrix when support status or validation layers change. Full upstream Firecracker test-suite mapping remains deferred. |

#1481 adds the aggregate remaining-device matrix gate across the capability,
snapshot, product-PCI, time/identity, and remaining-device rows above. The
checked selector contains exactly 85 identities (52 balloon, 19 memory-hotplug,
seven entropy, six serial, and one time/identity): 77 are terminal and eight
remain `audit-required`, all under Wave 6 #1490. Focused validation pins the
balloon -> memory-hotplug -> entropy -> serial -> time/identity
snapshot-preflight order, failure short-circuiting, unchanged state/artifacts, retry, and
MMIO/PCI owner release/reuse. Signed executable validation composes the live
device set over both transports; signed production validation isolates two
default-stdio launcher/App-Sandbox-worker sessions through pause, FIFO input,
EOF, independent termination, and cleanup. The private capture-ready values are
not optional-device encodings, restored-device evidence, or portability proof.
Wave 7 #1491 owns no record in this selector, and the global inventory remains
191/207/3/17.

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

## Update Rule

When a PR changes Firecracker-facing behavior, update this matrix if it changes
support status, adds or removes a validation layer, or moves work between
implemented, partial, deferred, recognized unsupported, or platform-limited
states.
