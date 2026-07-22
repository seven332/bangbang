# Firecracker v1.16.0 aggregate remaining-device contract

This is the checked closure ledger for #1481, the final aggregate child of
#1440 under #1348. It joins the five authoritative family ledgers without
replacing their row-specific semantic contracts. The selector contains exactly
85 identities: 52 balloon, 19 memory-hotplug, seven entropy, six serial, and
one time/identity aggregate.

Exactly 77 rows are `implemented-and-verified`. Eight rows remain
`audit-required` because complete snapshot serialization, restore,
clone/migration, portability, and restored-guest outcomes belong to
[Wave 6 #1490](https://github.com/seven332/bangbang/issues/1490). The
repository-wide observability, tools, and specification work belongs to
[Wave 7 #1491](https://github.com/seven332/bangbang/issues/1491), but zero rows
in this 85-record selector are handed to Wave 7. Global inventory totals remain
191 implemented, 207 audit-required, three missing-platform-feasible, and 17
proven-platform-impossible.

## Evidence keys

- `AGG-FOCUSED` — `crates/bangbang/src/vmm.rs::aggregate_remaining_device_snapshot_preflight_failures_preserve_order_and_reuse`
  and `crates/hvf/src/startup.rs::remaining_device_owner_budget_covers_mmio_and_pci_and_reuses_resources`.
- `AGG-SIGNED` — `crates/bangbang/tests/executable_hvf_e2e.rs::macos_arm64::signed_executable_certifies_remaining_devices_over_mmio`
  and `signed_executable_certifies_remaining_devices_over_product_pci`.
  Both use the same guest phase contract in
  `scripts/fetch-firecracker-rootfs.sh::check_remaining_device_certification`.
- `PRODUCTION-SERIAL` —
  `crates/launcher/tests/production_bundle_e2e.rs::normal_bundle_isolates_concurrent_default_serial_stdio_sessions`,
  plus `normal_bundle_streams_default_serial_stdio_across_launcher_worker_boundary`
  and the configured-output grant isolation/redaction tests in that module.
- `B-IMPL` — `crates/api/src/http.rs`,
  `crates/bangbang/src/{api_server,vmm}.rs`,
  `crates/runtime/src/balloon.rs`, and
  `crates/{runtime,hvf}/src/startup.rs`.
- `B-FOCUSED` —
  `balloon_mmio_capture_retains_complete_detached_live_state`,
  `balloon_notification_signal_dispatch_signals_queued_inflate_descriptor`, and
  `balloon_notification_signal_dispatch_signals_reporting_descriptor_and_records_metrics`;
  also `AGG-FOCUSED`.
- `B-SIGNED` —
  `capture_ready_balloon_traverses_signed_mmio_and_pci_owners`,
  `signed_executable_exposes_virtio_balloon_to_direct_rootfs_guest`, and
  `signed_executable_runs_all_startup_virtio_devices_over_product_pci`;
  also `AGG-SIGNED`.
- `M-IMPL` — `crates/api/src/http.rs`,
  `crates/bangbang/src/{api_server,vmm}.rs`,
  `crates/runtime/src/{memory,memory_hotplug}.rs`, and
  `crates/hvf/src/{memory,startup}.rs`.
- `M-FOCUSED` —
  `memory_hotplug_notification_signal_dispatch_signals_queued_request`,
  `memory_hotplug_runtime_dispatch_uses_injected_mutation_executor`, and
  `memory_hotplug_teardown_failure_is_recorded_exactly_once`;
  also `AGG-FOCUSED`.
- `M-SIGNED` —
  `capture_ready_memory_hotplug_traverses_signed_mmio_and_pci_owners`,
  `signed_executable_hotplugs_memory_from_direct_rootfs_guest`, and
  `signed_executable_runs_all_startup_virtio_devices_over_product_pci`;
  also `AGG-SIGNED`.
- `E-IMPL` — `crates/api/src/http.rs`,
  `crates/bangbang/src/{api_server,vmm}.rs`,
  `crates/runtime/src/entropy.rs`, and
  `crates/{runtime,hvf}/src/startup.rs`.
- `E-FOCUSED` —
  `entropy_notification_signal_dispatch_signals_queued_request`,
  `entropy_retry_capture_maps_none_immediate_and_delayed_state`, and
  `limiter_retry_session_quiescence_rolls_back_when_entropy_is_stopped`;
  also `AGG-FOCUSED`.
- `E-SIGNED` —
  `capture_ready_entropy_traverses_signed_mmio_and_pci_owners`,
  `signed_executable_captures_throttled_entropy_lifecycle_over_mmio`, and
  `signed_executable_captures_throttled_entropy_lifecycle_over_product_pci`;
  also `AGG-SIGNED`.
- `S-IMPL` — `crates/api/src/http.rs`,
  `crates/bangbang/src/{api_server,vmm}.rs`,
  `crates/runtime/src/serial.rs`, `crates/hvf/src/startup.rs`, and
  `crates/launcher/src/supervisor.rs`.
- `S-FOCUSED` —
  `serial_input_dispatch_bounds_rearms_and_detaches_after_eof`,
  `serial_input_interrupt_intent_is_taken_only_after_successful_signal`, and
  `aggregate_remaining_device_snapshot_preflight_failures_preserve_order_and_reuse`.
- `S-SIGNED` —
  `signed_executable_streams_default_serial_stdio_across_lifecycle_boundaries`
  and `signed_executable_isolates_concurrent_default_serial_stdio_streams`;
  also `AGG-SIGNED`.
- `T-IMPL` — `crates/runtime/src/{pvtime,rtc,snapshot_device,vmclock}.rs`
  and `crates/hvf/src/{ffi,pvtime,runner,psci,startup,snapshot_restore}.rs`.
- `T-FOCUSED` —
  `vmgenid_restore_replaces_before_signaling`,
  `vmclock_restore_updates_before_signaling`,
  `time_identity_vmclock_failure_is_terminal_after_vmgenid_commit`, and
  `AGG-FOCUSED`.
- `T-SIGNED` —
  `signed_executable_exposes_rtc_to_direct_rootfs_guest`,
  `signed_executable_exposes_vmclock_to_direct_rootfs_guest`,
  `signed_executable_creates_and_restores_native_v1_snapshot_across_processes`,
  `guest_boot::certifies_linux_pvtime_contention_idle_and_paused_accounting`,
  and `AGG-SIGNED`.
- `W6-BALLOON` — exact owner
  [#1490](https://github.com/seven332/bangbang/issues/1490): encode and restore
  balloon queues, features, accounting, statistics, hinting/reporting, timer
  continuation, clone/migration policy, portability, and signed restored-guest
  behavior.
- `W6-MEMORY` — exact owner
  [#1490](https://github.com/seven332/bangbang/issues/1490): encode and restore
  virtio-mem geometry, requested/plugged blocks, mapping/accounting state,
  clone/migration policy, portability, and signed restored-guest behavior.
- `W6-ENTROPY` — exact owner
  [#1490](https://github.com/seven332/bangbang/issues/1490): encode and restore
  queues, limiter buckets, retained retry timing, fresh scheduler ownership,
  clone/migration policy, portability, and signed restored-guest reads.
- `W6-SERIAL` — exact owner
  [#1490](https://github.com/seven332/bangbang/issues/1490): encode and restore
  UART/RX/pending-intent state, reconstruct fresh authorized endpoints, define
  terminal/FIFO portability, and prove signed restored-guest behavior.
- `W6-TIME` — exact owner
  [#1490](https://github.com/seven332/bangbang/issues/1490): encode and restore
  PVTime state, define repeated-clone and cross-host time-source portability,
  and prove signed restored-guest behavior.
- `W7` — exact out-of-selector owner
  [#1491](https://github.com/seven332/bangbang/issues/1491): repository-wide
  observability, public tools, and applicable specification outcomes only.

## Exact 85-record index

| Identity | Authoritative family ledger | Disposition | Implementation | Focused validation | Signed validation | Downstream |
| --- | --- | --- | --- | --- | --- | --- |
| `api-operation:GET /balloon` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-operation:GET /balloon/hinting/status` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-operation:GET /balloon/statistics` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-operation:PATCH /balloon` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-operation:PATCH /balloon/hinting/start` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-operation:PATCH /balloon/hinting/stop` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-operation:PATCH /balloon/statistics` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-operation:PUT /balloon` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-path:/balloon` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-path:/balloon/hinting/start` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-path:/balloon/hinting/status` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-path:/balloon/hinting/stop` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-path:/balloon/statistics` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:Balloon.amount_mib` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:Balloon.deflate_on_oom` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:Balloon.free_page_hinting` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:Balloon.free_page_reporting` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:Balloon.stats_polling_interval_s` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonHintingStatus.guest_cmd` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonHintingStatus.host_cmd` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStartCmd.acknowledge_on_stop` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.actual_mib` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.actual_pages` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.alloc_stall` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.async_reclaim` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.async_scan` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.available_memory` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.direct_reclaim` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.direct_scan` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.disk_caches` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.free_memory` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.hugetlb_allocations` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.hugetlb_failures` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.major_faults` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.minor_faults` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.oom_kill` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.swap_in` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.swap_out` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.target_mib` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.target_pages` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStats.total_memory` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonStatsUpdate.stats_polling_interval_s` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:BalloonUpdate.amount_mib` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-property:FullVmConfiguration.balloon` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-schema:Balloon` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-schema:BalloonHintingStatus` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-schema:BalloonStartCmd` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-schema:BalloonStats` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-schema:BalloonStatsUpdate` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `api-schema:BalloonUpdate` | [balloon](balloon-contract.md) | `implemented-and-verified` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `terminal` |
| `corpus:ballooning` | [balloon](balloon-contract.md) | `audit-required` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `W6-BALLOON` |
| `semantic.memory-device:balloon-oom-stats-hinting-and-reporting` | [balloon](balloon-contract.md) | `audit-required` | `B-IMPL` | `B-FOCUSED` | `B-SIGNED` | `W6-BALLOON` |
| `api-operation:GET /hotplug/memory` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-operation:PATCH /hotplug/memory` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-operation:PUT /hotplug/memory` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-path:/hotplug/memory` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:FullVmConfiguration.memory-hotplug` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:MemoryHotplugConfig.block_size_mib` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:MemoryHotplugConfig.slot_size_mib` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:MemoryHotplugConfig.total_size_mib` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:MemoryHotplugSizeUpdate.requested_size_mib` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:MemoryHotplugStatus.block_size_mib` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:MemoryHotplugStatus.plugged_size_mib` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:MemoryHotplugStatus.requested_size_mib` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:MemoryHotplugStatus.slot_size_mib` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-property:MemoryHotplugStatus.total_size_mib` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-schema:MemoryHotplugConfig` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-schema:MemoryHotplugSizeUpdate` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `api-schema:MemoryHotplugStatus` | [memory-hotplug](memory-hotplug-contract.md) | `implemented-and-verified` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `terminal` |
| `corpus:memory-hotplug` | [memory-hotplug](memory-hotplug-contract.md) | `audit-required` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `W6-MEMORY` |
| `semantic.memory-device:virtio-mem-lifecycle-accounting-and-state` | [memory-hotplug](memory-hotplug-contract.md) | `audit-required` | `M-IMPL` | `M-FOCUSED` | `M-SIGNED` | `W6-MEMORY` |
| `api-operation:PUT /entropy` | [entropy](entropy-contract.md) | `implemented-and-verified` | `E-IMPL` | `E-FOCUSED` | `E-SIGNED` | `terminal` |
| `api-path:/entropy` | [entropy](entropy-contract.md) | `implemented-and-verified` | `E-IMPL` | `E-FOCUSED` | `E-SIGNED` | `terminal` |
| `api-property:EntropyDevice.rate_limiter` | [entropy](entropy-contract.md) | `implemented-and-verified` | `E-IMPL` | `E-FOCUSED` | `E-SIGNED` | `terminal` |
| `api-property:FullVmConfiguration.entropy` | [entropy](entropy-contract.md) | `implemented-and-verified` | `E-IMPL` | `E-FOCUSED` | `E-SIGNED` | `terminal` |
| `api-schema:EntropyDevice` | [entropy](entropy-contract.md) | `implemented-and-verified` | `E-IMPL` | `E-FOCUSED` | `E-SIGNED` | `terminal` |
| `corpus:entropy` | [entropy](entropy-contract.md) | `audit-required` | `E-IMPL` | `E-FOCUSED` | `E-SIGNED` | `W6-ENTROPY` |
| `semantic.device:entropy-queues-limits-metrics-and-state` | [entropy](entropy-contract.md) | `audit-required` | `E-IMPL` | `E-FOCUSED` | `E-SIGNED` | `W6-ENTROPY` |
| `api-operation:PUT /serial` | [serial](serial-contract.md) | `implemented-and-verified` | `S-IMPL` | `S-FOCUSED` | `S-SIGNED + PRODUCTION-SERIAL` | `terminal` |
| `api-path:/serial` | [serial](serial-contract.md) | `implemented-and-verified` | `S-IMPL` | `S-FOCUSED` | `S-SIGNED + PRODUCTION-SERIAL` | `terminal` |
| `api-property:SerialDevice.rate_limiter` | [serial](serial-contract.md) | `implemented-and-verified` | `S-IMPL` | `S-FOCUSED` | `S-SIGNED + PRODUCTION-SERIAL` | `terminal` |
| `api-property:SerialDevice.serial_out_path` | [serial](serial-contract.md) | `implemented-and-verified` | `S-IMPL` | `S-FOCUSED` | `S-SIGNED + PRODUCTION-SERIAL` | `terminal` |
| `api-schema:SerialDevice` | [serial](serial-contract.md) | `implemented-and-verified` | `S-IMPL` | `S-FOCUSED` | `S-SIGNED + PRODUCTION-SERIAL` | `terminal` |
| `semantic.device:serial-stdin-stdout-rx-and-restore` | [serial](serial-contract.md) | `audit-required` | `S-IMPL` | `S-FOCUSED` | `S-SIGNED + PRODUCTION-SERIAL` | `W6-SERIAL` |
| `semantic.device:rtc-vmclock-vmgenid-and-pvtime` | [time-identity](time-identity-contract.md) | `audit-required` | `T-IMPL` | `T-FOCUSED` | `T-SIGNED` | `W6-TIME` |

## Closure boundary

The aggregate signed profile proves live coexistence, the selected transport,
concurrent public and guest activity, dual-bucket entropy pressure, virtio-mem
grow/shrink, balloon statistics/hinting/reporting, PL031/VMGenID/VMClock/PVTime
discovery, greater-than-FIFO default serial input across pause, capture-ready
preflight, normal shutdown, and deterministic socket/control-resource reuse.
The focused aggregate tests prove exact preflight ordering, injected failure
short-circuiting, mutation preservation, retryability, bounded MMIO/PCI
endpoint and interrupt ownership, release, and reuse. The production test proves
two launcher/App-Sandbox-worker sessions remain isolated and leave no steady
helper, socket, or session root after independent EOF and termination.

Capture-ready values remain private validated live state. This contract does
not claim Firecracker artifact compatibility, restored optional devices,
PVTime clone portability, or Wave 7 aggregate observability.
