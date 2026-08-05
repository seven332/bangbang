# Firecracker v1.16.0 vsock closure contract

This is the checked closure ledger for the 14 directly vsock-named identities
in the pinned Firecracker v1.16.0 inventory. All 14 are
`implemented-and-verified`; native-v2 2.12 supplies the six snapshot rows.
The immutable upstream baseline is Firecracker commit
`d83d72b710361a10294480131377b1b00b163af8`.

## Evidence keys

- **FC-API** — pinned `src/firecracker/swagger/firecracker.yaml`,
  `src/firecracker/src/api_server/request/vsock.rs`, and
  `src/vmm/src/vmm_config/vsock.rs`: strict PUT `/vsock`, CID/path shape,
  deprecated `vsock_id`, and configuration readback.
- **FC-LIVE** — pinned `docs/vsock.md`,
  `src/vmm/src/devices/virtio/vsock/`, and
  `tests/integration_tests/functional/test_vsock.py`: three virtqueues, both
  connection directions, local ports, credit, shutdown/reset, metrics,
  traffic, and cleanup.
- **FC-SNAPSHOT** — pinned
  `docs/snapshotting/snapshot-support.md`,
  `src/vmm/src/devices/virtio/vsock/persist.rs`,
  `src/vmm/src/vmm_config/snapshot.rs`, `src/vmm/src/persist.rs`, and snapshot
  integration tests: captured CID/selector/cursor/virtio state, empty restored
  live work, `TRANSPORT_RESET`, event-queue acknowledgement before RX,
  TX progress, override-before-construction, no-device rejection, and clones.
- **API** — strict parsing, projection, validation, response omission, and
  redaction in `crates/api/src/http.rs`,
  `crates/runtime/src/{snapshot,vsock}.rs`, and
  `crates/bangbang/src/{api_server,vmm}.rs`.
- **LIVE** — bounded MMIO/PCI routing, queues, credit, deadlines, events,
  metrics, and teardown in `crates/runtime/src/{vsock,metrics}.rs`,
  `crates/hvf/src/startup.rs`, and `crates/bangbang/src/vmm.rs`.
- **AUTHORITY** — exact direct listener/connector ownership and exact
  App Sandbox grant/listener/broker adoption with no ambient fallback in
  `crates/bangbang/src/{anchored_socket,contained_session,vsock_restore,vmm}.rs`
  and `crates/launcher/src/macos/socket_broker.rs`.
- **SNAPSHOT** — exact native-v2 2.12 kind 13, MMIO/PCI placement, public
  create/load, normalized empty work, reset/RX gate, cursor, direct/contained
  resource transactions, rollback, and cleanup in
  `crates/runtime/src/snapshot_vsock_{v2_12,restore_v2_12}.rs`,
  `crates/hvf/src/{snapshot_v2,snapshot_v2_vsock_platform}.rs`, and
  `crates/bangbang/src/vmm.rs`.
- **FOCUSED-API** — `parses_put_vsock_with_deprecated_vsock_id`, strict
  `VsockOverride` parsing, request projection, invalid selector/no-device
  rejection, and Debug/public-fault redaction.
- **FOCUSED-LIVE** —
  `virtio_vsock_transport_reset_publishes_event_and_mmio_interrupt`,
  `virtio_vsock_restored_gate_keeps_tx_live_and_buffers_generated_rx`, and the
  packet, queue, routing, capacity, port, credit, deadline, shutdown, metrics,
  failure, redaction, and cleanup matrices.
- **FOCUSED-SNAPSHOT** —
  `snapshot_vsock_selectors_resolve_before_resource_access_and_redact_values`,
  exact kind-13 fixture/format/topology/placement tests,
  `virtio_vsock_active_reconstruction_restores_cursors_empty_work_and_rx_gate`,
  `virtio_vsock_restored_gate_preserves_host_request_until_event_ack`, and the
  direct/contained resource, cancellation, retry/terminal, replacement,
  adoption, death, and cleanup matrices.
- **SIGNED-DIRECT** —
  `signed_executable_handles_guest_initiated_vsock_from_direct_rootfs`,
  `signed_executable_handles_guest_initiated_vsock_multistream_from_direct_rootfs`,
  `signed_executable_handles_host_initiated_vsock_to_direct_rootfs`,
  `signed_executable_handles_host_initiated_vsock_multistream_to_direct_rootfs`,
  `signed_executable_resets_live_vsock_before_unsupported_snapshot_over_mmio`,
  `signed_executable_resets_live_vsock_before_unsupported_snapshot_over_product_pci`,
  `capture_ready_vsock_resets_signed_mmio_and_pci_owners`,
  `signed_executable_certifies_native_v2_vsock_snapshot_over_mmio`, and
  `signed_executable_certifies_native_v2_vsock_snapshot_over_product_pci`.
- **SIGNED-CONTAINED** —
  `normal_bundle_routes_guest_vsock_through_launcher_broker_without_helpers`,
  `normal_bundle_routes_host_vsock_through_supplied_granted_listener`, and
  `normal_bundle_certifies_native_v2_vsock_restored_guest_lifecycle_and_containment`.
  The last test crosses the normal launcher/nested App Sandbox boundary for
  MMIO/PCI and also covers malformed artifacts, missing authority, cancellation,
  worker-first death, launcher-first death, retry, redaction, and cleanup.

## Exact 14-record ledger

| Identity | Disposition | Upstream | Implementation | Focused validation | Signed validation | Downstream |
| --- | --- | --- | --- | --- | --- | --- |
| `api-operation:PUT /vsock` | `implemented-and-verified` | `FC-API` | `API + AUTHORITY + LIVE` | `FOCUSED-API + FOCUSED-LIVE` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-path:/vsock` | `implemented-and-verified` | `FC-API` | `API` | `FOCUSED-API` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-property:FullVmConfiguration.vsock` | `implemented-and-verified` | `FC-API` | `API` | `FOCUSED-API` | `SIGNED-DIRECT` | `terminal` |
| `api-property:SnapshotLoadParams.vsock_override` | `implemented-and-verified` | `FC-SNAPSHOT` | `API + AUTHORITY + SNAPSHOT` | `FOCUSED-API + FOCUSED-SNAPSHOT` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-property:Vsock.guest_cid` | `implemented-and-verified` | `FC-API + FC-LIVE + FC-SNAPSHOT` | `API + LIVE + SNAPSHOT` | `FOCUSED-API + FOCUSED-LIVE + FOCUSED-SNAPSHOT` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-property:Vsock.uds_path` | `implemented-and-verified` | `FC-API + FC-LIVE` | `API + AUTHORITY + LIVE` | `FOCUSED-API + FOCUSED-LIVE` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-property:Vsock.vsock_id` | `implemented-and-verified` | `FC-API` | `API` | `FOCUSED-API` | `SIGNED-DIRECT` | `terminal` |
| `api-property:VsockOverride.uds_path` | `implemented-and-verified` | `FC-SNAPSHOT` | `API + AUTHORITY + SNAPSHOT` | `FOCUSED-API + FOCUSED-SNAPSHOT` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-schema:Vsock` | `implemented-and-verified` | `FC-API` | `API + AUTHORITY + LIVE` | `FOCUSED-API + FOCUSED-LIVE` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `api-schema:VsockOverride` | `implemented-and-verified` | `FC-SNAPSHOT` | `API + AUTHORITY + SNAPSHOT` | `FOCUSED-API + FOCUSED-SNAPSHOT` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `corpus:vsock` | `implemented-and-verified` | `FC-API + FC-LIVE + FC-SNAPSHOT` | `API + LIVE + AUTHORITY + SNAPSHOT` | `FOCUSED-API + FOCUSED-LIVE + FOCUSED-SNAPSHOT` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `semantic.snapshot:network-vsock-overrides-portability-and-clones` | `implemented-and-verified` | `FC-SNAPSHOT` | `AUTHORITY + SNAPSHOT` | `FOCUSED-SNAPSHOT` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `semantic.vsock:live-routing-credit-events-and-cleanup` | `implemented-and-verified` | `FC-LIVE` | `LIVE + AUTHORITY` | `FOCUSED-LIVE` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |
| `semantic.vsock:snapshot-override-reset-and-rx-gating` | `implemented-and-verified` | `FC-SNAPSHOT` | `AUTHORITY + SNAPSHOT` | `FOCUSED-LIVE + FOCUSED-SNAPSHOT` | `SIGNED-DIRECT + SIGNED-CONTAINED` | `terminal` |

## Observable live and snapshot contract

- PUT `/vsock` remains a strict preboot replacement. `guest_cid >= 3`;
  `uds_path` is a logical selector rather than ambient host authority; and
  deprecated `vsock_id` is input-only, unused for identity, and omitted from
  readback.
- The live device has three 256-entry queues, one shared 1023 active budget,
  bounded incomplete-host and packet queues, round-robin host-local ports,
  wrapping credit, both initiation directions, shutdown/reset, `EVENT_IDX`,
  aggregate metrics, and exact teardown. Its 20 pinned metrics share one
  coherent saturating owner across configuration, device, MMIO/PCI, HVF
  readiness, and restore; queue counts follow admitted source work, packet and
  byte counts follow actual connection delivery, and readiness failures retain
  muxer-versus-connection attribution. The 128-entry deadline queue rejects
  stale entries, rebuilds until synchronized after overflow, and expires at the
  exact deadline. Indirect descriptors are a supported Bangbang extension.
- Full/File native-v2 2.12 captures exactly one optional kind-13 device with
  CID, selector, host-local cursor, features, queues, interrupts, and coherent
  MMIO/PCI placement. Connections, accepts, packets, wakeups, deadlines,
  metrics, descriptors, grants, sessions, and host handles are not serialized.
- Snapshot capture closes source connections and queues `TRANSPORT_RESET`.
  A restored guest starts with empty live work; its listening sockets persist.
  RX waits for event-queue acknowledgement of reset while TX remains live.
- `vsock_override` replaces the selector of the one captured device and is
  rejected when no vsock exists. Selection and validation precede authority
  access. Direct load owns one exact inode; contained load consumes one exact
  directory/listener/connector transaction with no fallback.
- Every clone receives fresh process, socket, metrics, dispatcher, interrupt,
  connection, and cleanup ownership. The saved cursor is immutable; each clone
  allocates `saved + 1` and progresses locally. Paused load queues host work
  without guest progress and completes it after resume/reset acknowledgement.

## Portability and exclusions

The terminal clone/portability claim is deliberately bounded: immutable
Bangbang-native File/COW state-memory pairs may be loaded repeatedly when the
destination supplies compatible machine resources and explicit network/vsock
authority. It does not claim Firecracker snapshot bytes, preservation of live
peers, automatic migration of Unix sockets or grants, unconstrained cross-host
execution, Diff/native-v2 Uffd, or snapshot editing/rebase tools.

There is no PATCH/DELETE vsock API, runtime vsock hotplug, vhost/KVM mechanism,
or broader CID router. Repository-wide performance and observability work
remains tracked by [#1491](https://github.com/seven332/bangbang/issues/1491);
it does not retain any of these 14 functional identities.
