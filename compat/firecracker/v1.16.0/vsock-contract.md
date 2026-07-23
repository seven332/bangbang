# Firecracker v1.16.0 vsock closure contract

This is the checked closure ledger for #1518, the final aggregate child of
#1494 under #1348. It covers exactly 14 directly vsock-named Firecracker
v1.16.0 identities. Eight independently complete API and live-device rows are
`implemented-and-verified`; six aggregate snapshot rows remain
`audit-required` for exact work owned by
[#1490](https://github.com/seven332/bangbang/issues/1490).

The immutable baseline is Firecracker commit
`d83d72b710361a10294480131377b1b00b163af8`. The generated source manifest
remains 381 identities and the human overlay remains 418 identities. This
reconciliation moves the global disposition counts from 220/178/3/17 to
228/170/3/17.

## Evidence keys

- **FC-API** — pinned
  `src/firecracker/swagger/firecracker.yaml`,
  `src/firecracker/src/api_server/request/vsock.rs`, and
  `src/vmm/src/vmm_config/vsock.rs`. They define PUT-only `/vsock`, required
  `guest_cid >= 3` and `uds_path`, optional deprecated `vsock_id`, unknown-field
  rejection, backend creation, and readback without the deprecated field.
- **FC-LIVE** — pinned `docs/vsock.md`,
  `src/vmm/src/devices/virtio/vsock/`, and
  `tests/integration_tests/functional/test_vsock.py`. They define the Unix
  backend, three queues, both initiation directions, a shared 1023 active
  connection limit, round-robin local ports, credit, shutdown/reset, metrics,
  traffic, and cleanup.
- **FC-SNAPSHOT** — pinned
  `docs/snapshotting/snapshot-support.md`,
  `src/vmm/src/devices/virtio/vsock/persist.rs`,
  `src/vmm/src/vmm_config/snapshot.rs`, `src/vmm/src/persist.rs`, and snapshot
  integration tests. They define captured CID/selector/port/virtio state,
  dropped live connections, `TRANSPORT_RESET`, RX acknowledgement gating,
  override-before-construction, no-device rejection, and restored routing.
- **API** — strict parsing/projection in `crates/api/src/http.rs`, validated
  preboot configuration in `crates/runtime/src/{lib,vsock}.rs`, and
  transactional process routing in
  `crates/bangbang/src/{api_server,vmm}.rs`.
- **LIVE** — the bounded virtio-vsock implementation in
  `crates/runtime/src/{vsock,metrics}.rs`, MMIO/PCI owner assembly in
  `crates/{runtime,hvf}/src/startup.rs`, and process dispatch in
  `crates/bangbang/src/vmm.rs`.
- **AUTHORITY** — direct inode-safe listener/connector ownership and exact
  contained directory, supplied-listener, authenticated launcher connector,
  no-ambient-fallback, and cleanup ownership in
  `crates/bangbang/src/{anchored_socket,contained_session,vmm}.rs` and
  `crates/launcher/src/macos/socket_broker.rs`.
- **CAPTURE** — immutable redacted MMIO/PCI state and reconstruction in
  `crates/runtime/src/vsock.rs`, plus quiesced controller/process/HVF ownership
  in `crates/bangbang/src/vmm.rs` and `crates/hvf/src/startup.rs`.
- **RESTORE-PREP** — pure override selection in
  `crates/runtime/src/snapshot.rs`, direct preparation in
  `crates/runtime/src/vsock/direct_restore.rs`, and direct/contained
  transaction/adoption in `crates/bangbang/src/{vsock_restore,vmm}.rs`.
- **FOCUSED-API** — API tests
  `parses_put_vsock_with_minimal_body`,
  `parses_put_vsock_with_deprecated_vsock_id`,
  `parses_put_vsock_with_null_vsock_id`, the malformed/method/path cases,
  runtime `handles_put_vsock_config` and replacement/no-mutation/state cases,
  and process `configures_vsock_over_unix_socket`.
- **FOCUSED-LIVE** — the vsock module's packet, queue, routing, capacity, port,
  credit, deadline, shutdown, reset, metrics, redaction, failure, and cleanup
  tests, including
  `virtio_vsock_transport_reset_publishes_event_and_mmio_interrupt` and
  `virtio_vsock_restored_gate_keeps_tx_live_and_buffers_generated_rx`.
- **FOCUSED-CAPTURE** —
  `virtio_vsock_mmio_capture_is_exact_repeatable_and_redacted_while_inactive`,
  `virtio_vsock_capture_rejects_malformed_device_transport_and_ring_state`,
  `virtio_vsock_active_reconstruction_restores_cursors_empty_work_and_rx_gate`,
  and process owner/cancellation/normalization tests.
- **FOCUSED-RESTORE** —
  `snapshot_vsock_selectors_resolve_before_resource_access_and_redact_values`,
  direct stale/live/replacement/cleanup tests in
  `crates/runtime/src/vsock/direct_restore.rs`, and exact direct/contained
  preparation, rollback, adoption, and no-fallback tests in
  `crates/bangbang/src/vsock_restore.rs`.
- **SIGNED-DIRECT** —
  `signed_executable_runs_async_block_over_mmio_with_live_patch`,
  `signed_executable_handles_guest_initiated_vsock_from_direct_rootfs`,
  `signed_executable_handles_guest_initiated_vsock_multistream_from_direct_rootfs`,
  `signed_executable_handles_host_initiated_vsock_to_direct_rootfs`,
  `signed_executable_handles_host_initiated_vsock_multistream_to_direct_rootfs`,
  `signed_executable_resets_live_vsock_before_unsupported_snapshot_over_mmio`,
  and
  `signed_executable_resets_live_vsock_before_unsupported_snapshot_over_product_pci`.
- **SIGNED-CAPTURE** —
  `capture_ready_vsock_resets_signed_mmio_and_pci_owners`; it proves
  source-side reset, capture, resume, event acknowledgement, fresh traffic, and
  exact owner traversal, not artifact restore.
- **SIGNED-CONTAINED** —
  `normal_bundle_routes_guest_vsock_through_launcher_broker_without_helpers`
  and `normal_bundle_routes_host_vsock_through_supplied_granted_listener`.
  They prove real App Sandbox authority, both initiation directions,
  deterministic multistream/1-MiB traffic, half-close/EOF, cleanup, unchanged
  entitlements, and no steady-state helper.
- **W6** — [#1490](https://github.com/seven332/bangbang/issues/1490) owns
  optional-device artifact encoding and placement, aggregate public load
  invocation, restored event acknowledgement/reconnect/override proof,
  clone/version policy, and host/artifact portability.
- **W7** — [#1491](https://github.com/seven332/bangbang/issues/1491) owns final
  repository-wide performance and observability reconciliation. It does not
  retain or hide a directly owned live vsock identity.

## Exact 14-record ledger

| Identity | Disposition | Upstream | Implementation | Focused validation | Signed validation | Downstream |
| --- | --- | --- | --- | --- | --- | --- |
| `api-operation:PUT /vsock` | `implemented-and-verified` | `FC-API` | `API + AUTHORITY + LIVE` | `FOCUSED-API + FOCUSED-LIVE` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-path:/vsock` | `implemented-and-verified` | `FC-API` | `API` | `FOCUSED-API` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-property:FullVmConfiguration.vsock` | `implemented-and-verified` | `FC-API` | `API` | `FOCUSED-API` | `SIGNED-DIRECT` | `terminal` |
| `api-property:SnapshotLoadParams.vsock_override` | `audit-required` | `FC-SNAPSHOT` | `RESTORE-PREP` producer only | `FOCUSED-RESTORE` | `SIGNED-CAPTURE` source only | `W6` ([#1490](https://github.com/seven332/bangbang/issues/1490)) |
| `api-property:Vsock.guest_cid` | `implemented-and-verified` | `FC-API + FC-LIVE` | `API + LIVE` | `FOCUSED-API + FOCUSED-LIVE` | `SIGNED-DIRECT + SIGNED-CAPTURE + SIGNED-CONTAINED` | `terminal` |
| `api-property:Vsock.uds_path` | `implemented-and-verified` | `FC-API + FC-LIVE` | `API + AUTHORITY + LIVE` | `FOCUSED-API + FOCUSED-LIVE` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-property:Vsock.vsock_id` | `implemented-and-verified` | `FC-API` | `API` | `FOCUSED-API` | `SIGNED-DIRECT` | `terminal` |
| `api-property:VsockOverride.uds_path` | `audit-required` | `FC-SNAPSHOT` | `RESTORE-PREP` producer only | `FOCUSED-RESTORE` | `SIGNED-CAPTURE` source only | `W6` ([#1490](https://github.com/seven332/bangbang/issues/1490)) |
| `api-schema:Vsock` | `implemented-and-verified` | `FC-API` | `API + AUTHORITY + LIVE` | `FOCUSED-API + FOCUSED-LIVE` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-schema:VsockOverride` | `audit-required` | `FC-SNAPSHOT` | `RESTORE-PREP` producer only | `FOCUSED-RESTORE` | `SIGNED-CAPTURE` source only | `W6` ([#1490](https://github.com/seven332/bangbang/issues/1490)) |
| `corpus:vsock` | `audit-required` | `FC-API + FC-LIVE + FC-SNAPSHOT` | `API + LIVE + AUTHORITY + CAPTURE + RESTORE-PREP` producer subset | `FOCUSED-API + FOCUSED-LIVE + FOCUSED-CAPTURE + FOCUSED-RESTORE` | `SIGNED-DIRECT + SIGNED-CAPTURE + SIGNED-CONTAINED` source/live subset | `W6` ([#1490](https://github.com/seven332/bangbang/issues/1490)) |
| `semantic.snapshot:network-vsock-overrides-portability-and-clones` | `audit-required` | `FC-SNAPSHOT` | `CAPTURE + RESTORE-PREP` producer subset | `FOCUSED-CAPTURE + FOCUSED-RESTORE` | `SIGNED-CAPTURE` source only | `W6` ([#1490](https://github.com/seven332/bangbang/issues/1490)) |
| `semantic.vsock:live-routing-credit-events-and-cleanup` | `implemented-and-verified` | `FC-LIVE` | `LIVE + AUTHORITY + CAPTURE` | `FOCUSED-LIVE + FOCUSED-CAPTURE` | `SIGNED-DIRECT + SIGNED-CAPTURE + SIGNED-CONTAINED` | `terminal` |
| `semantic.vsock:snapshot-override-reset-and-rx-gating` | `audit-required` | `FC-SNAPSHOT` | `CAPTURE + RESTORE-PREP` producer subset | `FOCUSED-LIVE + FOCUSED-CAPTURE + FOCUSED-RESTORE` | `SIGNED-CAPTURE` source only | `W6` ([#1490](https://github.com/seven332/bangbang/issues/1490)) |

## Observable API and live contract

- PUT `/vsock` is a strict preboot replacement. Parsing and runtime validation
  complete before direct path or contained grant access; process state commits
  only after resource preparation succeeds. Post-start, malformed, invalid,
  unsupported-method, authority, and backend failures preserve the previous
  public/private state.
- `guest_cid` is an unsigned 32-bit value at least 3 and becomes the exact
  guest-visible 64-bit virtio CID. `uds_path` accepts validated relative or
  absolute logical selectors; a selector is not host authority. Direct startup
  binds and conditionally cleans one exact inode. Contained startup requires an
  exact directory grant, supplied listener, and authenticated launcher
  connector and never falls back to ambient paths.
- Deprecated `vsock_id` accepts ordinary strings or `null`, never selects or
  names the backend/device, and is omitted from `GET /vm/config`. Bangbang
  deliberately rejects empty or control-character identifiers earlier than
  Firecracker's ignored-string path. This ledger certifies the useful
  deprecated input-only behavior, not acceptance of every pathological string.
- The live MMIO-or-PCI device owns three 256-entry queues, one shared 1023
  active-connection budget, a separate bounded incomplete-host queue,
  round-robin host-local ports, bounded directional packet backlogs, wrapping
  credit, both initiation directions, partial/full shutdown, two-second
  delivery-based cleanup, reset/error handling, `EVENT_IDX`, aggregate metrics,
  and exact teardown. Indirect descriptors are a supported Bangbang extension.

## Capture, authority, and Wave 6 boundary

Paused capture publishes `TRANSPORT_RESET`, records exact used-ring/interrupt
effects, validates one configured MMIO or PCI owner plus memory/metrics, captures
immutable redacted CID/features/queues/cursor/selector state, and detaches live
connections, accepts, packets, wakeups, and deadlines under one lease. The
signed source test resumes that same source session, acknowledges the event,
and opens fresh traffic. It does not decode an artifact or construct a restored
VM.

Destination preparation resolves the captured selector and optional override
before access, then produces a real direct or exact contained listener,
connector, and cleanup owner for single-use reconstruction. No path, grant ID,
child, session token, or serialized string substitutes for that authority.
Public native-v1 state still contains no vsock optional-device bytes or
placement, and public load still rejects `vsock_override`.

Therefore the six W6 rows remain open. #1490 must encode and place the state,
invoke reconstruction through the public load transaction, prove a restored
guest observes and acknowledges reset before RX, reconnect in both directions
through original and overridden selectors, reject no-device overrides, and
certify clone/version/portability behavior. Only those results may terminate
the retained rows.

## Explicit nonclaims

- There is no PATCH, DELETE, runtime hotplug, broader CID-routing, unspecified
  event type, vhost/KVM mechanism, or automatic guest-notification claim.
- Source-side reset/capture/resume and destination-resource unit reconstruction
  are not public artifact restore, clone, or portability evidence.
- W7 performance/observability work may refine repository-wide measurement and
  schema closure, but the directly owned live routing, credit, events, metrics,
  authority, failure, and cleanup behavior in this ledger is terminal.
