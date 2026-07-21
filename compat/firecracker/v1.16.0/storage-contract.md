# Firecracker v1.16.0 storage closure contract

This ledger is the checked closure record for #1471, the final aggregate child
of #1450 under #1348. It covers exactly 40 directly owned Firecracker v1.16.0
storage identities. Thirty-eight are `implemented-and-verified`; exactly
`corpus:pmem` and
`semantic.storage:pmem-root-mapping-flush-and-state` remain `audit-required`
for Wave 6 optional-device snapshot serialization and restore.

The generated source manifest remains 381 identities, the overlay retains 37
local semantic identities and 418 total records, and this reconciliation moves
the global disposition counts from 86/312/3/17 to 114/284/3/17.

## Evidence keys

The table uses these exact aggregate validation anchors in addition to its
row-specific focused evidence:

- **Direct aggregate** —
  `crates/bangbang/tests/executable_hvf_e2e.rs::macos_arm64::signed_executable_certifies_aggregate_storage_semantics_over_product_pci`.
- **Contained aggregate** —
  `crates/launcher/tests/production_bundle_e2e.rs::normal_bundle_certifies_aggregate_storage_semantics_through_contained_grants`.
- **VMM ordering** —
  `crates/bangbang/src/vmm.rs::runtime_pmem_owner_preflight_precedes_grant_claim_mapping_and_config_commit`
  plus the contained vhost zero-request owner-preflight test.
- **Focused storage** — the parser/controller, `crates/runtime/src/block.rs`,
  `crates/runtime/src/block/async_executor.rs`,
  `crates/runtime/src/pmem.rs`, `crates/vhost-user/src/frontend.rs`, and
  `crates/hvf/src/{startup,memory,backend}.rs` unit tests named by the relevant
  implementation surface.

Both aggregate tests run a read-only Sync root, writable Sync control, writable
portable-Async data drive, writable vhost-user drive, writable pmem device, and
virtio-mem in one signed product-PCI VM. They prove marker-based discovery,
initial and continuing I/O, Writeback/pmem/vhost persistence, disjoint
concurrent block/pmem/vhost updates, paused Async replacement, memory
grow/shrink, serialized runtime block and pmem attach/remove/reuse, exact PCI
slot or pmem guest-range reuse, and final configuration. The direct branch then
proves terminal vhost-backend death and process cleanup. The production branch
proves exact grant and child authority, pathname-replacement resistance,
frontend/session/helper cleanup, redaction, and unchanged entitlements.

## Exact 40-record ledger

| Identity | Final disposition | Implementation evidence | Validation or retained owner |
| --- | --- | --- | --- |
| `api-operation:PATCH /drives/{drive_id}` | implemented and verified | Strict request parsing in `crates/api/src/http.rs`; failure-atomic file/Async and ID-only vhost update in `crates/bangbang/src/vmm.rs` and `crates/runtime/src/block.rs`. | Focused PATCH/rollback tests; Direct aggregate; Contained aggregate. |
| `api-operation:PATCH /pmem/{id}` | implemented and verified | Strict matching-ID limiter PATCH in `crates/api/src/http.rs`; owner-first live update in `crates/bangbang/src/vmm.rs` and `crates/runtime/src/pmem.rs`. | Focused limiter/update tests; Direct aggregate; Contained aggregate. |
| `api-operation:PUT /drives/{drive_id}` | implemented and verified | Preboot configuration, live replacement, and PCI insertion in `crates/bangbang/src/vmm.rs`, `crates/runtime/src/block.rs`, and `crates/hvf/src/startup.rs`; contained block control stays descriptor-bound. | Focused transaction/hotplug tests; Direct aggregate; Contained aggregate. |
| `api-operation:PUT /pmem/{id}` | implemented and verified | Preboot/root configuration and capacity-preflighted PCI insertion in `crates/bangbang/src/vmm.rs`, `crates/runtime/src/pmem.rs`, and `crates/hvf/src/{startup,memory}.rs`. | VMM ordering; focused mapping/rollback tests; Direct aggregate; Contained aggregate. |
| `non-swagger-route:DELETE /drives/{drive_id}` | implemented and verified | Bodyless route plus owner-thread PCI teardown, Async quiescence, and exact lease release in API/VMM/HVF/runtime block code. | Focused teardown rollback/reuse tests; Direct aggregate; Contained aggregate. |
| `non-swagger-route:DELETE /pmem/{id}` | implemented and verified | Bodyless route plus endpoint withdrawal, exact-prefix flush, mapping removal, and lease release in VMM/HVF/pmem code. | Focused mapping/teardown tests; Direct aggregate; Contained aggregate. |
| `api-path:/drives/{drive_id}` | implemented and verified | Complete strict PUT/PATCH/DELETE routing and file-versus-vhost transaction ownership. | API path tests; Direct aggregate; Contained aggregate. |
| `api-path:/pmem/{id}` | implemented and verified | Complete strict PUT/PATCH/DELETE routing and pmem mapping/owner transaction. | API path tests; VMM ordering; Direct aggregate; Contained aggregate. |
| `api-property:Drive.cache_type` | implemented and verified | `DriveCacheType`, virtio feature negotiation, file FLUSH, and vhost cache negotiation in `crates/runtime/src/block.rs`. | Focused Unsafe/Writeback/FLUSH tests; Direct aggregate; Contained aggregate. |
| `api-property:Drive.drive_id` | implemented and verified | Matching path/body validation, same-family uniqueness, ordered ownership, metrics, and teardown in runtime/VMM. | Focused identity/type-scope tests; Direct aggregate; Contained aggregate. |
| `api-property:Drive.io_engine` | implemented and verified | Sync plus bounded portable Async generation execution in block/runtime/HVF code; vhost omits the field. | Focused executor/queue tests and signed Sync/Async families; Direct aggregate; Contained aggregate. |
| `api-property:Drive.is_read_only` | implemented and verified | Exact file/grant access and `VIRTIO_BLK_F_RO`; vhost requires absence. | Focused field/access/queue tests; signed read-only aggregate root in both modes. |
| `api-property:Drive.is_root_device` | implemented and verified | One cross-family root and stable boot argument in `crates/runtime/src/{lib,startup}.rs`; runtime root mutation rejects. | Focused atomic root tests; Direct aggregate; Contained aggregate. |
| `api-property:Drive.partuuid` | implemented and verified | Optional retained block/vhost field and root `PARTUUID` command-line selection. | Runtime partuuid/root tests and signed block-root evidence. |
| `api-property:Drive.path_on_host` | implemented and verified | File-versus-socket matrix, direct regular/block-special opening, exact contained grants, live candidate preparation, and redaction. | Focused replacement/grant rollback tests; Direct aggregate; Contained aggregate. |
| `api-property:Drive.rate_limiter` | implemented and verified | Bandwidth/ops buckets, rollback, bounded retry, and incremental live update for Sync/Async file drives. | Focused limiter tests; Direct aggregate; Contained aggregate. |
| `api-property:Drive.socket` | implemented and verified | Direct or exact contained vhost connection, bounded frontend negotiation, immutable shared aperture, CONFIG refresh, and cleanup. | VMM zero-request preflight/broker tests; Direct aggregate; Contained aggregate. |
| `api-property:FullVmConfiguration.drives` | implemented and verified | Ordered committed drive projection from `VmmController::vm_config` through API response conversion. | API serialization/rollback tests; exact final projection in both aggregate modes. |
| `api-property:FullVmConfiguration.pmem` | implemented and verified | Ordered committed pmem projection from `VmmController::vm_config` through API response conversion. | API serialization/rollback tests; exact final projection in both aggregate modes. |
| `api-property:PartialDrive.drive_id` | implemented and verified | Strict matching-ID validation before backing, broker, limiter, or owner work. | Parser/VMM no-mutation tests; Direct aggregate. |
| `api-property:PartialDrive.path_on_host` | implemented and verified | Optional failure-atomic file/Async candidate replacement; vhost rejects the field. | Focused direct/contained replacement rollback; both aggregate modes. |
| `api-property:PartialDrive.rate_limiter` | implemented and verified | Incremental preserve/clear semantics without backing or engine replacement. | Focused limiter tests; concurrent disjoint update in both aggregate modes. |
| `api-property:PartialPmem.id` | implemented and verified | Strict matching-ID validation before owner lookup or mutation. | Parser/runtime no-mutation tests; concurrent aggregate pmem update. |
| `api-property:PartialPmem.rate_limiter` | implemented and verified | Incremental preserve/clear semantics committed after live owner success. | Focused limiter/retry tests; both aggregate modes. |
| `api-property:Pmem.id` | implemented and verified | Type-scoped identity binds mapping, range, metrics, PCI owner, grant, teardown, and reuse. | Focused mixed-identity tests; exact aggregate range/slot reuse. |
| `api-property:Pmem.path_on_host` | implemented and verified | Required nonzero regular backing, exact contained descriptor, file/private-tail map, cloned HVF lease, and redaction. | Focused map/protection tests; Direct aggregate; pathname-resistant Contained aggregate. |
| `api-property:Pmem.rate_limiter` | implemented and verified | Exact-file-prefix byte charging, ops rollback, coalesced notifications, retry, PATCH, and disable. | Focused limiter tests; concurrent aggregate update and persistence in both modes. |
| `api-property:Pmem.read_only` | implemented and verified | Shared writable or private host/read-only guest mapping with exact-prefix synchronization. | Signed write-protection/coherence tests; aggregate writable pmem in both modes. |
| `api-property:Pmem.root_device` | implemented and verified | Default false, one cross-family root, stable pmem ordering, `ro`/`rw` boot argument, and runtime root rejection. | Focused root tests and signed root guests; aggregate cross-family root validation. |
| `api-schema:Drive` | implemented and verified | Complete strict file/vhost Drive request, runtime model, response, and lifecycle field matrix. | Parser/model tests; Direct aggregate; Contained aggregate. |
| `api-schema:PartialDrive` | implemented and verified | Exact ID plus optional path and limiter, with backend-specific update policy. | Parser/model/rollback tests; Direct aggregate; Contained aggregate. |
| `api-schema:Pmem` | implemented and verified | Complete strict Pmem request/model/response, mapping, limiter, root, runtime lifecycle, and projection. | Parser/model/HVF tests; VMM ordering; Direct aggregate; Contained aggregate. |
| `api-schema:PartialPmem` | implemented and verified | Exact ID plus optional incremental limiter, with live owner commit ordering. | Parser/model tests; Direct aggregate; Contained aggregate. |
| `corpus:block-caching` | implemented and verified | Applicable Unsafe/Writeback, FLUSH, Sync/Async, replacement, and vhost trust-boundary outcomes in runtime block code. | Focused cache/flush tests; Direct aggregate; Contained aggregate. |
| `corpus:block-io-engine` | implemented and verified | Bounded portable Async executor with generation-safe completion plus Sync default and explicit native-v1 boundary. | Executor/queue/lifecycle tests and signed Sync/Async families; both aggregate modes. |
| `corpus:block-vhost-user` | implemented and verified | Complete applicable frontend protocol, shared-memory aperture, MMIO/PCI lifecycle, CONFIG, runtime reuse, contained brokerage, death, and snapshot rejection. | Focused protocol/broker tests; Direct aggregate terminal branch; Contained aggregate orderly branch. |
| `corpus:patch-block` | implemented and verified | Cooperative failure-atomic file/Async refresh, incremental limiter update, and ID-only existing-stream vhost CONFIG refresh. | Focused update/rollback tests; concurrent and paused aggregate phases in both modes. |
| `corpus:pmem` | audit required | Live API, mapping, root, protection, flush, limiter, capture-ready state, hotplug/reuse, and containment are implemented. | **Wave 6** owns optional-device snapshot serialization/restore against the same external backing plus artifact, migration, portability, and signed restore outcomes. |
| `semantic.storage:block-sync-async-vhost-and-limits` | implemented and verified | Complete applicable block aggregate across Sync, portable Async, vhost, cache/flush, limiting, replacement, shared aperture, PCI lifecycle, failure, and cleanup. | Focused owner/resource tests; Direct aggregate; Contained aggregate. |
| `semantic.storage:pmem-root-mapping-flush-and-state` | audit required | Live root/mapping/protection/flush/limiter/capture-ready/runtime/cleanup behavior is implemented. | **Wave 6** owns optional-pmem serialization/restore, generalized artifact state, migration/portability, and signed restore evidence. |

## Observable storage contract

### Ordinary block and PATCH

- Sync remains the default. Async is a bounded portable per-session executor,
  not a claim of Linux io_uring identity. Both publish guest status, used-ring,
  dirty, interrupt, limiter, and metrics outcomes on the owner thread.
- Unsafe omits virtio FLUSH. Writeback advertises it and synchronizes the exact
  opened backing. Block-special direct control uses public Darwin ioctls;
  contained control remains on the launcher's retained descriptor.
- PATCH is cooperative. Host backing replacement plus guest notification is
  not an atomic filesystem transaction, and already admitted I/O may cross a
  pause/update boundary. The transaction does guarantee that candidate
  preparation, Async-generation quiescence, owner publication, grant commit,
  and public configuration occur in failure-atomic order.
- File-backed GET_ID is the exact 20-byte decimal identity derived from the
  opened backing metadata; it is independent of `drive_id` and changes with a
  successful replacement.

### Vhost-user block

- Bangbang supplies the frontend, not a production backend. Each drive owns one
  bounded Unix control connection and one virtqueue. The backend owns its
  internal caching, limiting, resource policy, jail, health, and availability.
- The immutable memory table contains boot RAM and the complete virtio-mem
  aperture. Offline aperture bytes stay outside guest CPU/HVF/current memory
  accounting but remain accessible to the trusted backend. Darwin provides no
  Linux memfd-seal identity.
- Contained mode can connect only one authorized child below an exact
  connect-only directory grant. The launcher facet is session/sequence/child
  bound and leaves no steady helper. Duplicate or capacity rejection occurs
  before any broker request.
- Vhost-user devices remain incompatible with native-v1 snapshots and reject
  snapshot create before artifact mutation. That explicit incompatibility is
  the applicable supported outcome, not an unimplemented vhost schema leaf.

### Pmem

- One nonzero regular file is mapped directly. The persistent prefix is the
  exact file length; 2-MiB alignment padding is a volatile private tail and is
  never flushed to the backing. Writable mappings are shared; read-only guest
  mappings use a private write-capable host view required by HVF.
- Capacity, root, duplicate, PCI function, BAR, MSI-X, dispatcher, inventory,
  and metrics preflight runs before a contained grant is claimed or a direct
  file is opened and mapped. Configuration commits only after mapping and
  endpoint publication succeed.
- DAX is a guest/filesystem choice. Host page faults, huge-page realization,
  page-cache/RSS accounting, eviction, same-backing physical-page sharing and
  side channels, and performance must be profiled on the deployed macOS/HVF
  system; Firecracker's Linux numeric observations are not portable promises.
- Capture-ready traversal retains the live owner and mapping identity but does
  not serialize optional pmem state. Wave 6 must implement and certify restore
  with the same external backing before the two retained composites can move.

## Explicit exclusions and later owners

This closure claims no native-v1 optional-device persistence, generalized
migration or portable Firecracker artifacts, bundled/managed vhost backend,
physical-disk certification, Darwin memfd seals, Linux cgroup or io_uring
mechanism identity, automatic guest PCI notification, or new entitlement.
Wave 6 owns exactly the two pmem composite records above. Wave 7 retains
repository-wide metrics/schema/timing closure, #1351 retains credentialed
production and vmnet gates, and Wave 8 retains the final cross-capability
export audit.
