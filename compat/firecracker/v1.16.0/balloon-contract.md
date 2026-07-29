# Firecracker v1.16.0 balloon closure contract

This ledger is the checked terminal closure record for #1473 and #1681 under
#1490 and #1348. It covers exactly 52 directly owned Firecracker v1.16.0
balloon identities. All 52 API operation, path, property, schema, corpus, and
semantic identities are `implemented-and-verified`.

The generated source manifest remains 381 identities, the overlay retains 37
local semantic identities and 418 total records. #1473 first moved the 50
bounded live/API identities from 114/284/3/17 to 164/234/3/17. #1681 closes the
two retained aggregates and moves the current global disposition counts from
238/160/3/17 to 240/158/3/17.

## Evidence keys

- **API/model** — strict request parsing and response serialization in
  `crates/api/src/http.rs`, API conversion in
  `crates/bangbang/src/api_server.rs`, and lifecycle/configuration ownership in
  `crates/bangbang/src/vmm.rs`.
- **Runtime** — queue parsing, feature negotiation, statistics, hinting,
  reporting, prepared PFN accounting, metrics, and detached capture state in
  `crates/runtime/src/balloon.rs`; Linux-compatible pre-activation statistics
  notification admission in `crates/runtime/src/virtio_mmio.rs` and retained
  pre-activation PCI notification dispatch in `crates/runtime/src/balloon.rs`;
  MMIO/PCI
  attachment and exact selected-owner traversal in `crates/runtime/src/startup.rs`,
  `crates/runtime/src/virtio_pci.rs`, and `crates/hvf/src/startup.rs`.
- **Native-v2 2.9 artifact and restore** — exact optional component kind 10,
  hostile-input codec, cross-component validation, transport reconstruction,
  and fresh-owner restore planning in
  `crates/runtime/src/snapshot_balloon_v2_9.rs`,
  `crates/hvf/src/snapshot_v2_balloon_platform.rs`,
  `crates/hvf/src/startup.rs`, and `crates/bangbang/src/vmm.rs`.
- **Focused validation** — balloon route/model tests in
  `crates/api/src/http.rs` and `crates/bangbang/src/{api_server,vmm}.rs`, plus
  queue, parser, failure-order, accounting, hinting, statistics, reporting,
  metrics, reset, and capture-invariant tests in
  `crates/runtime/src/balloon.rs`.
- **Signed owner validation** —
  `crates/hvf/tests/hvf_lifecycle.rs::capture_ready_balloon_traverses_signed_mmio_and_pci_owners`.
- **Signed public validation** —
  `crates/bangbang/tests/executable_hvf_e2e.rs::macos_arm64::signed_executable_certifies_native_v2_balloon_snapshot_continuation`
  and
  `crates/launcher/tests/production_bundle_e2e.rs::macos_arm64::normal_bundle_certifies_native_v2_balloon_snapshot_continuation_and_containment`.

The signed direct and normal-production/App-Sandbox matrices run both MMIO and
PCI. They configure all optional live features; observe Linux inflate, periodic
optional statistics, a nonzero-to-nonzero polling update, hinting guest STOP
plus host DONE, reporting, and exact 2,048-page accounting; capture exact
native-v2 `2.9.0`; and load the same immutable state/memory pair into explicit
Paused and automatic-resume destinations. They prove no polling while Paused,
one complete destination-local interval before a retained statistics
descriptor completes, latest-stat preservation, normalized DONE followed by a
fresh hint run, post-restore inflate/deflate/API/reporting behavior, fresh
metrics, recapture, independent clones, malformed-artifact rejection,
cancellation/death cleanup, pathname-replacement resistance, and session
namespace cleanup. The direct runtime test also proves that Linux's
statistics-queue kick before `DRIVER_OK` is retained on PCI just as it is on
MMIO, then dispatched after activation.

## Exact 52-record ledger

| Identity | Final disposition | Exact contract and evidence |
| --- | --- | --- |
| `api-operation:GET /balloon` | implemented and verified | Returns the committed five-field configuration or the stable unsupported-device fault. API/model and signed public validation. |
| `api-operation:GET /balloon/hinting/status` | implemented and verified | Returns host and optional guest command state only when hinting is configured. Focused hint-state tests and signed automatic-ack/stop validation. |
| `api-operation:GET /balloon/statistics` | implemented and verified | Returns required target/actual page and MiB fields plus exactly the optional fields received from the guest. Focused parser/omission tests and signed periodic statistics. |
| `api-operation:PATCH /balloon` | implemented and verified | Runtime-only target replacement validates memory bounds, updates config space, signals configuration change, and preserves all other fields. Focused rollback tests and signed inflate-to-zero convergence over MMIO/PCI. |
| `api-operation:PATCH /balloon/hinting/start` | implemented and verified | Allocates a nonreserved wrapping command ID, stores `acknowledge_on_stop`, updates config space, and signals the guest. Focused command tests and signed Linux execution. |
| `api-operation:PATCH /balloon/hinting/stop` | implemented and verified | Publishes DONE while preserving the last guest command for status. Focused command tests and signed explicit stop. |
| `api-operation:PATCH /balloon/statistics` | implemented and verified | Replaces a nonzero polling interval while rejecting runtime enable/disable transitions and preserving latest optional statistics. Focused timer/state tests and signed 1-to-2 update. |
| `api-operation:PUT /balloon` | implemented and verified | Strict preboot replacement validates target versus guest memory and transactionally preserves prior machine/balloon state on failure. API/model and signed startup validation. |
| `api-path:/balloon` | implemented and verified | Complete strict GET/PUT/PATCH route, method, state, JSON, and error behavior. API route tests and signed public validation. |
| `api-path:/balloon/hinting/start` | implemented and verified | Complete strict PATCH-only hint-start route and request behavior. API route tests and signed guest validation. |
| `api-path:/balloon/hinting/status` | implemented and verified | Complete strict GET-only hint-status route and response behavior. API route tests and signed guest validation. |
| `api-path:/balloon/hinting/stop` | implemented and verified | Complete strict bodyless PATCH-only hint-stop route. API route tests and signed guest validation. |
| `api-path:/balloon/statistics` | implemented and verified | Complete strict GET/PATCH statistics route, request, response, and state behavior. API route tests and signed guest validation. |
| `api-property:Balloon.amount_mib` | implemented and verified | Required u32 MiB target, page conversion, guest-memory bound, exact GET/config projection, live replacement, and zero target. Focused boundary/rollback tests and signed convergence. |
| `api-property:Balloon.deflate_on_oom` | implemented and verified | Required boolean is retained, reported, and mapped to negotiated `VIRTIO_BALLOON_F_DEFLATE_ON_OOM`. Focused feature/layout tests and signed configuration projection. |
| `api-property:Balloon.free_page_hinting` | implemented and verified | Optional/default-false boolean controls feature negotiation, queue layout, hint API availability, command config, and capture invariants. Focused tests and signed hint run. |
| `api-property:Balloon.free_page_reporting` | implemented and verified | Optional/default-false boolean controls feature negotiation, reporting queue layout, validated writable ranges, best-effort discard, and metrics. Focused tests and signed reporting count. |
| `api-property:Balloon.stats_polling_interval_s` | implemented and verified | Optional/default-zero u16 controls statistics queue presence and periodic scheduling and is projected exactly. Focused layout/timer tests and signed nonzero polling. |
| `api-property:BalloonHintingStatus.guest_cmd` | implemented and verified | Nullable u32 is absent until a 4-byte guest command completes and then retains the latest exact command. Focused malformed/partial tests and signed guest STOP. |
| `api-property:BalloonHintingStatus.host_cmd` | implemented and verified | Required u32 reflects STOP, active command IDs, and DONE, including automatic acknowledgement. Focused wrap/transition tests and signed status. |
| `api-property:BalloonStartCmd.acknowledge_on_stop` | implemented and verified | Optional/default-true boolean controls automatic DONE after guest STOP and is captured with hint state. Parser/transition tests and signed automatic acknowledgement. |
| `api-property:BalloonStats.actual_mib` | implemented and verified | Required u32 is derived from compact host-accounted inflated pages without trusting guest config `actual`. Focused range tests and signed nonzero/zero convergence. |
| `api-property:BalloonStats.actual_pages` | implemented and verified | Required u32 is the compact paired inflate-minus-deflate PFN count, committed only after used publication. Failure-order tests and signed convergence. |
| `api-property:BalloonStats.alloc_stall` | implemented and verified | Optional u64 tag 11 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.async_reclaim` | implemented and verified | Optional u64 tag 14 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.async_scan` | implemented and verified | Optional u64 tag 12 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.available_memory` | implemented and verified | Optional u64 tag 6 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.direct_reclaim` | implemented and verified | Optional u64 tag 15 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.direct_scan` | implemented and verified | Optional u64 tag 13 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.disk_caches` | implemented and verified | Optional u64 tag 7 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.free_memory` | implemented and verified | Optional u64 tag 4 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused tests and signed optional-statistics preservation. |
| `api-property:BalloonStats.hugetlb_allocations` | implemented and verified | Optional u64 tag 8 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.hugetlb_failures` | implemented and verified | Optional u64 tag 9 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.major_faults` | implemented and verified | Optional u64 tag 2 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.minor_faults` | implemented and verified | Optional u64 tag 3 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.oom_kill` | implemented and verified | Optional u64 tag 10 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.swap_in` | implemented and verified | Optional u64 tag 0 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.swap_out` | implemented and verified | Optional u64 tag 1 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStats.target_mib` | implemented and verified | Required u32 is the exact committed target MiB. Focused response tests and signed 8-to-0 update. |
| `api-property:BalloonStats.target_pages` | implemented and verified | Required u32 is the checked MiB-to-4-KiB-page target used in guest config space. Focused overflow/bound tests and signed 2048-to-0 update. |
| `api-property:BalloonStats.total_memory` | implemented and verified | Optional u64 tag 5 is parsed, merged when present, omitted when absent, serialized exactly, and captured. Focused parser/merge/response tests. |
| `api-property:BalloonStatsUpdate.stats_polling_interval_s` | implemented and verified | Required u16 PATCH value replaces only an already-enabled nonzero interval. Strict parser/state tests and signed update/preservation. |
| `api-property:BalloonUpdate.amount_mib` | implemented and verified | Required u32 PATCH value changes only target size after complete validation and owner delivery. Focused rollback tests and signed zero convergence. |
| `api-property:FullVmConfiguration.balloon` | implemented and verified | Nullable committed balloon object appears exactly in `/vm/config` and follows successful transaction state. API/VMM projection tests and signed validation. |
| `api-schema:Balloon` | implemented and verified | Complete strict five-field request/response schema with defaults, unknown-field/type rejection, lifecycle rules, and selected transport execution. API/model tests and signed public validation. |
| `api-schema:BalloonHintingStatus` | implemented and verified | Complete required-host/nullable-guest u32 response schema. API serialization tests and signed status transitions. |
| `api-schema:BalloonStartCmd` | implemented and verified | Complete strict optional/default-true acknowledgement request schema. API parser tests and signed acknowledged run. |
| `api-schema:BalloonStats` | implemented and verified | Complete four required plus sixteen optional u64 fields, exact omission, queue parsing/merging, and detached capture. Focused parser/serialization tests and signed optional fields. |
| `api-schema:BalloonStatsUpdate` | implemented and verified | Complete strict required-u16 polling update schema and runtime transition policy. Focused parser/state tests and signed update. |
| `api-schema:BalloonUpdate` | implemented and verified | Complete strict required-u32 target update schema and runtime transaction. Focused parser/rollback tests and signed MMIO/PCI zero convergence. |
| `corpus:ballooning` | implemented and verified | Complete applicable live/API behavior plus exact native-v2 2.9 optional kind-10 serialization and MMIO/PCI restoration. Direct and contained signed guests certify explicit/automatic resume, statistics/hint/report continuation, post-restore inflate/deflate, immutable independent clones, recapture, failure boundaries, and cleanup. |
| `semantic.memory-device:balloon-oom-stats-hinting-and-reporting` | implemented and verified | DEFLATE_ON_OOM, publication-safe paired accounting, polling/statistics, hinting/reporting, best-effort Darwin reclaim, metrics, exact transport ownership, bounded artifact state, fresh destination owners, restored Linux behavior, clone isolation, failures, and cleanup are implemented and verified. |

## Observable live and capture-ready contract

- Inflate and deflate validate a complete descriptor and fallibly prepare the
  next compact PFN accounting value before publishing its used entry. Once
  publication succeeds, accounting commits by move with no allocation. A
  preparation or publication failure preserves prior accounting; a later
  descriptor failure preserves only the already committed prefix.
- Guest config `actual_pages` and host compact accounting are captured as
  separate facts. A cooperative guest normally converges them, but capture does
  not reject a transient or malicious mismatch.
- Detached state includes available/negotiated features, config values, exact
  queue layout and active cursors, pending statistics head, latest optional
  statistics, polling interval, full hint state, and compact PFN ranges.
  Feature/layout/activation/ring/head/range invariants are checked against
  mapped guest memory with fallible allocation. No guest-memory borrow, lock,
  endpoint, host handle, or wall-clock value escapes.
- A paused process-supervisor transaction quiesces auxiliary work and requires
  exactly one configured MMIO or PCI owner. MMIO captures under dispatcher
  ownership; PCI captures device and canonical transport under one endpoint
  lock. The result carries MMIO region/IRQ or PCI SBDF/BAR placement.
- Darwin discard remains best effort. Only complete mapped guest ranges and
  inward-aligned host-page interiors are advised; guest-visible queue
  completion and paired accounting do not promise synchronous RSS reduction.

## Exact native-v2 2.9 terminal profile and limits

- The current writer emits exact native-v2 `2.9.0`: required serial component
  kind 8, optional unchanged profile-3 storage kind 7, optional unchanged
  entropy kind 9, and optional balloon kind 10. Balloon-only,
  storage-plus-balloon, entropy-plus-balloon, and
  storage-plus-entropy-plus-balloon products are admitted with or without each
  optional predecessor, over one coherent MMIO or PCI transport.
- Kind 10 is capped at 4 MiB and at 262,144 canonical inflated-PFN ranges; the
  complete native-v2 state file remains capped at 16 MiB. The serialized
  accounting is the exact host-side inflate-minus-deflate PFN set and every
  range is revalidated against destination RAM before owner construction.
- Balloon PFNs are always guest 4 KiB pages. Darwin reclaim separately rounds
  inward to and coalesces destination host pages, normally 16 KiB. Reclaim is
  best effort: queue completion and exact guest accounting do not guarantee an
  immediate or synchronous resident-set-size reduction.
- Capture normalizes an active free-page-hinting host command to DONE. Restore
  retains the latest guest command/history but never resumes a source hint run;
  the destination may start a new independent run.
- Polling timers and absolute deadlines are not serialized. A retained pending
  statistics descriptor is scheduled only after one full destination-local
  interval after resume, and no interval elapses while the destination remains
  Paused.
- Guest memory, interrupt/notifier/dispatcher authority, timers, reclaim
  advisers, metrics, and cleanup owners are fresh per destination. Repeated
  destinations are isolated; state/memory artifacts remain immutable.

Firecracker artifact-byte compatibility, cross-host execution portability,
guest-independent convergence, and synchronous host-footprint reduction are
not claimed.
