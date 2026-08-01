# Firecracker v1.16.0 network and MMDS closure contract

This checked #1496 closure ledger owns exactly 35 Firecracker v1.16.0 network
and MMDS identities. Thirty-three are `implemented-and-verified`, including
the exact native-v2 2.11 snapshot/session rows. Two broad rows remain
`audit-required` for work owned by
[#1378](https://github.com/seven332/bangbang/issues/1378),
[#1491](https://github.com/seven332/bangbang/issues/1491).

## Evidence keys

- **API-MMDS** — strict request parsing and response conversion in
  `crates/api/src/http.rs`, process action ownership in
  `crates/bangbang/src/{api_server,vmm}.rs`, and bounded data/config behavior in
  `crates/runtime/src/mmds.rs`.
- **API-NET** — strict PUT/PATCH/DELETE parsing and projection in
  `crates/api/src/http.rs`, transactional process ownership in
  `crates/bangbang/src/vmm.rs`, and network configuration/update state in
  `crates/runtime/src/network.rs`.
- **NET-CORE** — virtio queue/header/feature/merged-buffer/limiter/capture logic
  in `crates/runtime/src/{network,network_packet,metrics}.rs`, MMIO/PCI owner
  integration in `crates/{runtime,hvf}/src/startup.rs`, and typed vmnet
  lifecycle/readiness/batches in
  `crates/bangbang/src/host_network/{vmnet,virtio_vmnet}.rs`.
- **MMDS-CORE** — stateless instance-bound tokens in
  `crates/runtime/src/mmds_token.rs`, the source-attributed bounded TCP core in
  `crates/runtime/src/mmds_tcp/`, and per-interface packet/scheduler integration
  in `crates/runtime/src/mmds_stack.rs`.
- **FOCUSED** — API, controller, runtime, packet, token, TCP, metrics, vmnet,
  callback, batch, owner, capture, rollback, redaction, and teardown tests next
  to those implementations.
- **SIGNED-TRANSPORT** —
  `crates/hvf/tests/guest_boot.rs::boots_signed_mmio_guest_with_complete_virtio_network_semantics`
  and `boots_signed_pci_guest_with_complete_virtio_network_semantics`. Together
  they inspect negotiated checksum/TSO/UFO/merged/ring features, observe bounded
  segmentation, deliver a 49,152-byte merged response under limiter pressure,
  renew a V2 token, deliberately lose an ACK, and observe retransmission.
- **SIGNED-CAPTURE** —
  `crates/hvf/tests/hvf_lifecycle.rs::capture_ready_network_traverses_signed_mmio_and_pci_owners`
  plus the signed executable paused snapshot-preflight branches. They prove
  deterministic selected-owner capture, runtime generation reuse, retry state,
  fresh lossy MMDS identity, resume, and cleanup without defining encoding.
- **SNAPSHOT-CODEC** — exact 2.11 kind-12 encoding and fresh reconstruction in
  `crates/runtime/src/{snapshot_network_v2_11,snapshot_network_restore_v2_11}.rs`,
  artifact admission in `crates/runtime/src/snapshot_artifact/`, process
  override/resource composition in `crates/bangbang/src/vmm.rs`, and MMIO/PCI
  owner reconstruction in
  `crates/hvf/src/snapshot_v2_network_platform.rs`.
- **SIGNED-PROCESS** — signed cases in
  `crates/bangbang/tests/executable_hvf_e2e.rs`, including
  `signed_executable_serves_mmds_with_configured_mtu_to_direct_rootfs_guest`,
  `signed_executable_retries_rate_limited_mmds_rx_without_second_guest_notification`,
  `signed_executable_serves_mmds_on_two_isolated_guest_interfaces`,
  `signed_executable_keeps_concurrent_mmds_processes_isolated`,
  `signed_executable_hotplugs_mmds_network_and_reuses_product_pci_slot`, and the
  API/no-API MMDS v1/v2 cases.
- **SIGNED-CONTAINED** —
  `crates/launcher/tests/production_bundle_e2e.rs::normal_bundle_hotplugs_mmds_network_without_vmnet_authority`
  and `networkless_bundle_rejects_every_positive_vmnet_mode_before_session_creation`.
  They prove exact lifecycle-v5 ownership, credential-free MMDS-only execution,
  runtime reuse, policy denial before session creation, and unchanged
  App-Sandbox/Hypervisor authority.
- **SIGNED-SNAPSHOT** —
  `crates/bangbang/tests/executable_hvf_e2e.rs::signed_executable_certifies_native_v2_network_mmds_snapshot_continuation`
  and
  `crates/launcher/tests/production_bundle_e2e.rs::normal_bundle_certifies_native_v2_network_mmds_snapshot_continuation_and_containment`.
  They cover MMIO/PCI and V1/V2 MMDS, explicit-Paused recapture and automatic
  destinations, exact overrides, empty/reseeded data, source TCP/token loss,
  fresh sessions, immutable artifacts, redacted failures, cancellation and
  death cleanup, retry, and all-MMDS containment without vmnet authority.
- **EXTERNAL-GATE** — the missing-credential production preflight exits 3 and
  prints exactly `bangbang vmnet preflight: blocked`. #1378 owns the first real
  Apple-approved start, packet-connectivity, service-error, teardown, crash,
  retry, and concurrent-session results; a non-success local gate is never a
  passing skip.
- **W7** — #1491 owns the excluded `corpus:network-performance` row and final
  repository-wide metrics/schema/timing/performance reconciliation.

## Exact 35-record ledger

| Identity | Disposition | Implementation | Focused validation | Signed validation | Downstream |
| --- | --- | --- | --- | --- | --- |
| `api-operation:GET /mmds` | `implemented-and-verified` | `API-MMDS` | `FOCUSED` | `SIGNED-PROCESS` | `terminal` |
| `api-operation:PATCH /mmds` | `implemented-and-verified` | `API-MMDS` | `FOCUSED` | `SIGNED-PROCESS` | `terminal` |
| `api-operation:PATCH /network-interfaces/{iface_id}` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-operation:PUT /mmds` | `implemented-and-verified` | `API-MMDS` | `FOCUSED` | `SIGNED-PROCESS` | `terminal` |
| `api-operation:PUT /mmds/config` | `implemented-and-verified` | `API-MMDS + MMDS-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-operation:PUT /network-interfaces/{iface_id}` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS + SIGNED-CONTAINED` | `terminal` |
| `api-path:/mmds` | `implemented-and-verified` | `API-MMDS` | `FOCUSED` | `SIGNED-PROCESS` | `terminal` |
| `api-path:/mmds/config` | `implemented-and-verified` | `API-MMDS + MMDS-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-path:/network-interfaces/{iface_id}` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS + SIGNED-CONTAINED` | `terminal` |
| `api-property:FullVmConfiguration.mmds-config` | `implemented-and-verified` | `API-MMDS` | `FOCUSED` | `SIGNED-PROCESS` | `terminal` |
| `api-property:FullVmConfiguration.network-interfaces` | `implemented-and-verified` | `API-NET` | `FOCUSED` | `SIGNED-PROCESS + SIGNED-CONTAINED` | `terminal` |
| `api-property:MmdsConfig.imds_compat` | `implemented-and-verified` | `API-MMDS + MMDS-CORE` | `FOCUSED` | `SIGNED-PROCESS` | `terminal` |
| `api-property:MmdsConfig.ipv4_address` | `implemented-and-verified` | `API-MMDS + MMDS-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-property:MmdsConfig.network_interfaces` | `implemented-and-verified` | `API-MMDS + MMDS-CORE` | `FOCUSED` | `SIGNED-PROCESS + SIGNED-CONTAINED` | `terminal` |
| `api-property:MmdsConfig.version` | `implemented-and-verified` | `API-MMDS + MMDS-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-property:NetworkInterface.guest_mac` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-property:NetworkInterface.host_dev_name` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-PROCESS + SIGNED-CONTAINED + EXTERNAL-GATE` | `terminal` |
| `api-property:NetworkInterface.iface_id` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-PROCESS + SIGNED-CONTAINED` | `terminal` |
| `api-property:NetworkInterface.mtu` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-property:NetworkInterface.rx_rate_limiter` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-property:NetworkInterface.tx_rate_limiter` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-property:PartialNetworkInterface.iface_id` | `implemented-and-verified` | `API-NET` | `FOCUSED` | `SIGNED-PROCESS` | `terminal` |
| `api-property:PartialNetworkInterface.rx_rate_limiter` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-property:PartialNetworkInterface.tx_rate_limiter` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-schema:MmdsConfig` | `implemented-and-verified` | `API-MMDS + MMDS-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `api-schema:MmdsContentsObject` | `implemented-and-verified` | `API-MMDS` | `FOCUSED` | `SIGNED-PROCESS` | `terminal` |
| `api-schema:NetworkInterface` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS + SIGNED-CONTAINED` | `terminal` |
| `api-schema:PartialNetworkInterface` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `corpus:mmds-design` | `implemented-and-verified` | `API-MMDS + MMDS-CORE + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `corpus:mmds-user-guide` | `implemented-and-verified` | `API-MMDS + MMDS-CORE + SNAPSHOT-CODEC` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS + SIGNED-SNAPSHOT` | `terminal` |
| `corpus:network-setup` | `audit-required` | `API-NET + NET-CORE` applicable live subset | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS + SIGNED-CONTAINED + EXTERNAL-GATE` | `EXTERNAL-GATE` |
| `corpus:patch-network-interface` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS` | `terminal` |
| `non-swagger-route:DELETE /network-interfaces/{iface_id}` | `implemented-and-verified` | `API-NET + NET-CORE` | `FOCUSED` | `SIGNED-PROCESS + SIGNED-CONTAINED` | `terminal` |
| `semantic.mmds:tcp-token-session-and-isolation` | `implemented-and-verified` | `MMDS-CORE + SNAPSHOT-CODEC` | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS + SIGNED-CAPTURE + SIGNED-SNAPSHOT` | `terminal` |
| `semantic.network:virtio-net-vmnet-policy-and-connectivity` | `audit-required` | `API-NET + NET-CORE + MMDS-CORE` live and capture-ready subset | `FOCUSED` | `SIGNED-TRANSPORT + SIGNED-PROCESS + SIGNED-CAPTURE + SIGNED-CONTAINED + EXTERNAL-GATE` | `EXTERNAL-GATE + W7` |

## Observable live contract

- Strict host APIs own committed process-local JSON/MMDS config and requested
  network config. The public projection changes only after the corresponding
  data, active limiter, runtime endpoint, provider, and owner transaction
  succeeds.
- One immutable network profile fixes requested and realized MAC/MTU, backend
  packet bounds, direct-header envelope, per-bit offload support, and batch
  limits before publication. Requested API values remain distinct from
  realized backend values.
- Guest TX validates headers and normalizes supported checksum/TSO/UFO work
  before MMDS classification or vmnet delivery. Guest RX supplies canonical
  headers, supports transactional merged chains with exact `num_buffers`, and
  retains source ownership across capacity, limiter, memory, publication, or
  interrupt failure.
- Callback code only publishes coalesced generation-tagged readiness. All queue,
  packet, limiter, metric, MMDS, and configuration mutation remains on the
  owner thread. Teardown retires readiness, disables and drains the callback,
  stops vmnet, and releases the generation in that order; uncertain cleanup is
  terminal.
- Each selected interface owns an independent MMDS ARP/TCP/output state while
  sharing the VM data store and token authority. MMDS output precedes backend
  RX, protocol deadlines share the owner scheduler, and one retained output is
  consumed only after guest RX commit.
- V2 tokens are opaque, bounded, expiry-authenticated, and bound to immutable
  instance identity. Fresh processes reject peer tokens. Keys, tokens, instance
  IDs, packet bytes, MACs, interface names, UUIDs, and raw framework values do
  not enter ordinary diagnostics or capture state.

## Capture and production boundaries

Exact native-v2 2.11 capture retains deterministic requested/realized/backend
configuration, inert captured selector identity, transport placement, queues,
negotiated features, limiters, retry intent, and MMDS
selection/version/IP/IMDS compatibility. It excludes raw host handles,
providers, packet-I/O owners, callbacks, cached or peer packets, active
TCP/ARP/reset/response bytes, MMDS data, token keys, tokens, metrics, and
wall-clock values. Every load requires an exact unique clone-local override
set, constructs fresh backend/network/MMDS owners, starts with an empty MMDS
store, and deliberately loses source connections and tokens. Signed direct and
contained guests prove reseeding and fresh-session success before publication
is treated as certified.

Contained mode authenticates the exact lifecycle-v5 session and complete
vmnet mode/count authority before backend work. All-MMDS execution consumes no
vmnet authority; the launcher never receives frames, packet metadata, vmnet
handles/callbacks/properties, tokens, or guest diagnostics. Networkless remains
the default signed profile and rejects positive host/shared/bridged policy
before session creation.

## Explicit nonclaims and handoffs

- #1378 remains open. This contract does not claim an Apple-approved production
  vmnet start, external packet connectivity, service failure, crash reclamation,
  or credentialed concurrent connectivity.
- #1490 retains Diff/native-v2-Uffd work, artifact editing/version evolution,
  and unconstrained migration/host portability. Exact 2.11 network/MMDS bytes,
  reconstruction, overrides, and clone-session freshness are terminal here;
  current 2.12 composes them unchanged with optional vsock. Peer-owned vmnet
  packets and active MMDS TCP sessions are intentionally not persisted.
- #1491 owns the separate network-performance corpus, global metric schema and
  timing reconciliation, and performance validation. Correctness-critical
  network/MMDS producers already exist; this ledger does not claim throughput
  parity or Linux TAP/epoll/timerfd mechanism identity.
- macOS vmnet modes replace Linux TAP/NAT/bridge mechanisms. Operators remain
  responsible for mode selection and host firewall policy; the MMDS classifier
  is not an egress firewall.
