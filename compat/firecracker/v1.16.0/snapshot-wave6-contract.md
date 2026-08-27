# Firecracker v1.16.0 Wave 6 snapshot certification contract

This is the checked umbrella ledger for the exact 70 identities selected by
Wave 6 of the Firecracker v1.16.0 audit. The immutable upstream baseline is
Firecracker commit `d83d72b710361a10294480131377b1b00b163af8`.
At the Wave 6 checkpoint, sixty-eight identities were
`implemented-and-verified` and the two broad network aggregates were
`audit-required`, with explicit owners outside this wave. #1930 later moves
those rows to `missing-platform-feasible` without rewriting this historical
ledger. This contract composes the narrower producer contracts; it does not
replace their detailed invariants or tests.

## Pinned source boundary

- **FC-API** — `src/firecracker/swagger/firecracker.yaml` and
  `src/firecracker/src/api_server/request/snapshot.rs`: strict snapshot create
  and load schemas, pre-boot load admission, Paused-first publication, and
  optional resume. `SnapshotLoadParams` has network and vsock overrides but no
  block- or per-drive-override field.
- **FC-SUPPORT** — `docs/snapshotting/snapshot-support.md` and
  `src/vmm/src/vstate/vm.rs`: complete state, Full and Diff memory, external
  disks, dirty tracking, lossy live resources, architecture-specific state,
  and compatible destination requirements.
- **FC-VERSION** — `docs/snapshotting/versioning.md` and
  `src/firecracker/src/main.rs`: versioned upstream artifacts and the early
  `--snapshot-version` command. Bangbang never relabels its native envelope as
  Firecracker bitcode or v10.
- **FC-CLONES** — `docs/snapshotting/random-for-clones.md`,
  `docs/snapshotting/network-for-clones.md`, and
  `docs/snapshotting/handling-page-faults-on-snapshot-resume.md`: fresh clone
  identity, clone-local network authority, and external demand paging.
- **FC-TOOLS** — `docs/snapshotting/snapshot-editor.md`,
  `src/snapshot-editor/{main,info,edit_memory,edit_vmstate}.rs`, and
  `src/rebase-snap/src/main.rs`: inspection, reviewed register removal, and
  memory rebase command shapes.

## Local implementation and evidence keys

- **API-LOAD** — strict parsing and redaction in `crates/api/src/http.rs`,
  request projection in `crates/bangbang/src/api_server.rs`, pristine-load and
  backend policy in `crates/runtime/src/snapshot.rs`, and one-open prepared
  family/resource transactions in `crates/bangbang/src/vmm.rs`.
- **ARTIFACT** — native envelope, exact state/memory binding, retained
  File/COW mapping, Full publication, version dispatch, and failure policy in
  `crates/runtime/src/{snapshot_format_v2,snapshot_memory_v2,
  snapshot_artifact,snapshot_commit}.rs` and `crates/hvf/src/snapshot_v2.rs`.
- **PAGER** — frozen native-v1 Uffd profile through
  `crates/{pager,runtime,hvf}/src` and
  `compat/firecracker/v1.16.0/snapshot-paging-contract.md`. It is the reviewed
  `bangbang-pager-v1` macOS mechanism, not Linux UFFD descriptor or wire
  compatibility; current native-v2 rejects Uffd before resource adoption.
- **DIFF** — exact native-v2 2.13 lineage, selection, publication, load, and
  both rebase commands in `crates/runtime/src/{snapshot_diff_v2_13,
  snapshot_lineage,snapshot_rebase}.rs`, `crates/bangbang/src/vmm.rs`, and
  `tools/snapshot-tools`; the checked owner is
  `snapshot-diff-rebase-contract.md`.
- **TOOLS** — bounded native inspection, reviewed aarch64 register removal,
  no-clobber state editing, and process outcome policy in
  `crates/hvf/src/snapshot_document`,
  `crates/runtime/src/snapshot_state_edit.rs`, and `tools/snapshot-tools`; the
  checked owner is `snapshot-editor-contract.md`.
- **TIME** — PL031, VMGenID, complete 112-byte VMClock v1 ABI, per-vCPU PVTime,
  ordered restore/notification, and terminal mutation boundary in
  `crates/runtime/src/{snapshot_device,pvtime,vmclock,startup}.rs` and
  `crates/hvf/src/{startup,snapshot_v2_platform}.rs`; the checked owner is
  `time-identity-contract.md`.
- **DEVICES** — exact optional serial 2.7, entropy 2.8, balloon 2.9,
  virtio-mem 2.10, network/MMDS 2.11, and vsock 2.12 codecs plus coherent
  MMIO/PCI restoration. Canonical owners are `serial-contract.md`,
  `entropy-contract.md`, `balloon-contract.md`,
  `memory-hotplug-contract.md`, `network-mmds-contract.md`,
  `vsock-contract.md`, and the storage contracts.
- **API-TESTS** — parser, projection, controller, load-classifier, family,
  backend, override, state/memory binding, malformed-input, redaction, and
  retry/terminal tests beside the owning modules.
- **FORMAT-TESTS** — fixed native-v1 fixtures and exact native-v2 2.3 through
  2.13 codec, compatibility, topology, corruption, future-minor, profile,
  binding, File/COW, Diff, and device-product tests.
- **TOOL-TESTS** — `crates/runtime/src/{snapshot_rebase,
  snapshot_state_edit}/tests.rs`, `crates/hvf/src/snapshot_document/*/tests.rs`,
  and `tools/snapshot-tools/tests/cli.rs`, including actual command processes,
  cancellation, path substitution, cleanup, and committed uncertainty.
- **SIGNED-DIRECT** — `crates/bangbang/tests/executable_hvf_e2e.rs` and the
  signed HVF suites: immutable cross-process Full clones, repeated time and
  identity restore, storage epochs, every stateful optional-device family,
  network/vsock overrides, paging, Diff/rebase, recapture, and guest
  continuation.
- **SIGNED-PRODUCTION** — `crates/launcher/tests/production_bundle_e2e.rs`
  through the fixed launcher and nested App Sandbox + Hypervisor worker. It
  covers direct/contained grants, pathname replacement, Paused and automatic
  destinations, MMIO/PCI, death order, cancellation, redaction, and cleanup.
  `normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci` also
  invokes `assert_production_snapshot_time_identity_transition` for every
  rooted/rootless x MMIO/PCI Paused recapture. That comparison normalizes only
  numeric device limiter ages and retry countdowns, retaining their
  presence/type and every other stable device fact.

## Exact 70-record ledger

| Identity | Disposition | Upstream | Implementation | Focused/core validation | Signed/product validation | Result |
| --- | --- | --- | --- | --- | --- | --- |
| `api-operation:PUT /snapshot/create` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + DIFF` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-operation:PUT /snapshot/load` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + PAGER + DIFF` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-path:/snapshot/create` | `implemented-and-verified` | `FC-API` | `API-LOAD + ARTIFACT + DIFF` | `API-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-path:/snapshot/load` | `implemented-and-verified` | `FC-API` | `API-LOAD + ARTIFACT + PAGER + DIFF` | `API-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotCreateParams.mem_file_path` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + DIFF` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotCreateParams.snapshot_path` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + DIFF` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotCreateParams.snapshot_type` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + DIFF` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotLoadParams.clock_realtime` | `implemented-and-verified` | `FC-API` | `API-LOAD + TIME` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotLoadParams.enable_diff_snapshots` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + DIFF` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT` | `terminal` |
| `api-property:SnapshotLoadParams.mem_backend` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + PAGER` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotLoadParams.mem_file_path` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotLoadParams.network_overrides` | `implemented-and-verified` | `FC-API + FC-CLONES` | `API-LOAD + DEVICES` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotLoadParams.resume_vm` | `implemented-and-verified` | `FC-API` | `API-LOAD` | `API-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotLoadParams.snapshot_path` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:SnapshotLoadParams.track_dirty_pages` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + DIFF` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT` | `terminal` |
| `api-property:SnapshotLoadParams.vsock_override` | `implemented-and-verified` | `FC-API + FC-CLONES` | `API-LOAD + DEVICES` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:MemoryBackend.backend_path` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + PAGER` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:MemoryBackend.backend_type` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + PAGER` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:NetworkOverride.host_dev_name` | `implemented-and-verified` | `FC-API + FC-CLONES` | `API-LOAD + DEVICES` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:NetworkOverride.iface_id` | `implemented-and-verified` | `FC-API + FC-CLONES` | `API-LOAD + DEVICES` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-property:VsockOverride.uds_path` | `implemented-and-verified` | `FC-API + FC-CLONES` | `API-LOAD + DEVICES` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-schema:SnapshotCreateParams` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + DIFF` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-schema:SnapshotLoadParams` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + PAGER + DIFF + DEVICES` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-schema:MemoryBackend` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + PAGER` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-schema:NetworkOverride` | `implemented-and-verified` | `FC-API + FC-CLONES` | `API-LOAD + DEVICES` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `api-schema:VsockOverride` | `implemented-and-verified` | `FC-API + FC-CLONES` | `API-LOAD + DEVICES` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:snapshot-editor` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS + DIFF` | `TOOL-TESTS` | `SIGNED-PRODUCTION + SIGNED-DIRECT` | `terminal` |
| `corpus:snapshot-network-clones` | `implemented-and-verified` | `FC-CLONES` | `API-LOAD + DEVICES` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:snapshot-page-faults` | `implemented-and-verified` | `FC-CLONES` | `API-LOAD + PAGER` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:snapshot-random-clones` | `implemented-and-verified` | `FC-CLONES` | `TIME + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:snapshot-support` | `implemented-and-verified` | `FC-SUPPORT` | `API-LOAD + ARTIFACT + PAGER + DIFF + DEVICES + TIME` | `API-TESTS + FORMAT-TESTS + TOOL-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:snapshot-versioning` | `implemented-and-verified` | `FC-VERSION + FC-SUPPORT` | `ARTIFACT + PAGER + DIFF + TOOLS` | `FORMAT-TESTS + TOOL-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.snapshot:diff-dirty-tracking-and-memory-backends` | `implemented-and-verified` | `FC-SUPPORT + FC-CLONES` | `API-LOAD + ARTIFACT + PAGER + DIFF` | `API-TESTS + FORMAT-TESTS + TOOL-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.snapshot:editor-rebase-and-inspection` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS + DIFF` | `TOOL-TESTS` | `SIGNED-PRODUCTION + SIGNED-DIRECT` | `terminal` |
| `semantic.snapshot:full-create-load-and-public-lifecycle` | `implemented-and-verified` | `FC-API + FC-SUPPORT` | `API-LOAD + ARTIFACT + DEVICES + TIME` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.snapshot:multi-vcpu-drives-devices-and-mmds` | `implemented-and-verified` | `FC-SUPPORT + FC-CLONES` | `ARTIFACT + DEVICES + TIME` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.snapshot:network-vsock-overrides-portability-and-clones` | `implemented-and-verified` | `FC-API + FC-CLONES` | `API-LOAD + DEVICES + ARTIFACT` | `API-TESTS + FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `tool-argument:rebase-snap/base-file` | `implemented-and-verified` | `FC-TOOLS` | `DIFF + TOOLS` | `TOOL-TESTS` | `SIGNED-DIRECT` | `terminal` |
| `tool-argument:rebase-snap/diff-file` | `implemented-and-verified` | `FC-TOOLS` | `DIFF + TOOLS` | `TOOL-TESTS` | `SIGNED-DIRECT` | `terminal` |
| `tool-argument:snapshot-editor/edit-memory/rebase/diff-path` | `implemented-and-verified` | `FC-TOOLS` | `DIFF + TOOLS` | `TOOL-TESTS` | `SIGNED-DIRECT` | `terminal` |
| `tool-argument:snapshot-editor/edit-memory/rebase/memory-path` | `implemented-and-verified` | `FC-TOOLS` | `DIFF + TOOLS` | `TOOL-TESTS` | `SIGNED-DIRECT` | `terminal` |
| `tool-argument:snapshot-editor/edit-vmstate/remove-regs/output-path` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `tool-argument:snapshot-editor/edit-vmstate/remove-regs/regs` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `tool-argument:snapshot-editor/edit-vmstate/remove-regs/vmstate-path` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `tool-argument:snapshot-editor/info-vmstate/vcpu-states/vmstate-path` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `tool-argument:snapshot-editor/info-vmstate/version/vmstate-path` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `tool-argument:snapshot-editor/info-vmstate/vm-state/vmstate-path` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `tool-operation:rebase-snap/rebase` | `implemented-and-verified` | `FC-TOOLS` | `DIFF + TOOLS` | `TOOL-TESTS` | `SIGNED-DIRECT` | `terminal` |
| `tool-operation:snapshot-editor/edit-memory/rebase` | `implemented-and-verified` | `FC-TOOLS` | `DIFF + TOOLS` | `TOOL-TESTS` | `SIGNED-DIRECT` | `terminal` |
| `tool-operation:snapshot-editor/edit-vmstate/remove-regs` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `tool-operation:snapshot-editor/info-vmstate/vcpu-states` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `tool-operation:snapshot-editor/info-vmstate/version` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `tool-operation:snapshot-editor/info-vmstate/vm-state` | `implemented-and-verified` | `FC-TOOLS` | `TOOLS` | `TOOL-TESTS` | `SIGNED-PRODUCTION` | `terminal` |
| `firecracker-argument:snapshot-version` | `implemented-and-verified` | `FC-VERSION` | `ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:ballooning` | `implemented-and-verified` | `FC-SUPPORT` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.memory-device:balloon-oom-stats-hinting-and-reporting` | `implemented-and-verified` | `FC-SUPPORT` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:memory-hotplug` | `implemented-and-verified` | `FC-SUPPORT` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.memory-device:virtio-mem-lifecycle-accounting-and-state` | `implemented-and-verified` | `FC-SUPPORT` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:entropy` | `implemented-and-verified` | `FC-SUPPORT + FC-CLONES` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.device:entropy-queues-limits-metrics-and-state` | `implemented-and-verified` | `FC-SUPPORT + FC-CLONES` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.device:serial-stdin-stdout-rx-and-restore` | `implemented-and-verified` | `FC-SUPPORT` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.device:rtc-vmclock-vmgenid-and-pvtime` | `implemented-and-verified` | `FC-CLONES + FC-SUPPORT` | `TIME + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:pmem` | `implemented-and-verified` | `FC-SUPPORT` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.storage:pmem-root-mapping-flush-and-state` | `implemented-and-verified` | `FC-SUPPORT` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:mmds-user-guide` | `implemented-and-verified` | `FC-SUPPORT + FC-CLONES` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `corpus:network-setup` | `audit-required` | `FC-CLONES` | `DEVICES snapshot subset` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION snapshot subset` | [#1378](https://github.com/seven332/bangbang/issues/1378) |
| `semantic.mmds:tcp-token-session-and-isolation` | `implemented-and-verified` | `FC-SUPPORT + FC-CLONES` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.network:virtio-net-vmnet-policy-and-connectivity` | `audit-required` | `FC-CLONES` | `DEVICES snapshot subset` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION snapshot subset` | [#1378](https://github.com/seven332/bangbang/issues/1378) + [#1491](https://github.com/seven332/bangbang/issues/1491) |
| `corpus:vsock` | `implemented-and-verified` | `FC-SUPPORT + FC-CLONES` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |
| `semantic.vsock:snapshot-override-reset-and-rx-gating` | `implemented-and-verified` | `FC-SUPPORT + FC-CLONES` | `DEVICES + ARTIFACT` | `FORMAT-TESTS` | `SIGNED-DIRECT + SIGNED-PRODUCTION` | `terminal` |

## Version and product closure

The frozen native-v1 reader retains eager File and reviewed external-pager
Uffd profiles. Native-v2 has an explicit monotonic ladder: exact 2.3 introduces
portable topology/time; 2.4 adds the singleton rooted block profile; 2.5 adds
the block vector; 2.6 adds complete regular-file block/pmem profile-3 storage;
2.7 adds required serial; 2.8 adds optional entropy; 2.9 adds optional balloon;
2.10 adds optional virtio-mem; 2.11 adds optional network/MMDS; 2.12 adds
optional vsock and is the current Full writer; 2.13 adds the mandatory Diff
binding and is the current Diff writer. Readers accept only their exact
reviewed compatible profiles; malformed, future, wrong-architecture,
wrong-page-size, feature, component, topology, version, and binding mismatches
fail before publication.

The combined acceptance matrix is closed by these independently attributable
owners:

- Full/File and Paused/automatic load: API/load tests,
  `signed_executable_creates_and_restores_native_v2_snapshot_across_processes`,
  and `normal_bundle_adopts_native_v2_snapshot_grants_for_create_describe_and_restore`.
- Rooted/rootless storage, MMIO/PCI, immutable repeated loads, and recapture:
  `signed_executable_certifies_native_v2_multi_block_epochs_over_mmio_and_pci`
  and `normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci`.
- Time/identity and multi-vCPU: the time contract, signed three-vCPU restored
  guest observation, and
  `assert_production_snapshot_time_identity_transition` in all four fixed
  production storage cells.
- Optional-device products: each checked producer contract, its focused codec
  tests and signed guest continuation, plus
  `exact_minor_thirteen_diff_closes_all_sixty_four_mmio_and_pci_products`.
- Diff and rebase: `current_dynamic_add_and_remove_topologies_write_canonically`,
  `repeated_complete_application_handles_add_then_remove`,
  `diff_publication_commits_and_zero_root_loads_as_one_closed_pair`,
  `diff_load_accepts_a_complete_rebased_result_image`,
  `signed_native_v2_diff_process_loads_zero_root_and_rebased_products`, and
  `normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox`.
- Editor and tools: the exact state/rebase contracts, focused transaction and
  CLI process suites, and signed Full/Diff production inspection/edit/load.
- External paging: the exact paging contract and signed host/guest first-fault,
  removal-generation, peer-death, entitlement, App Sandbox, and cleanup matrix.
- Overrides and lossy peers: exact network and vsock contracts, complete
  clone-local selector validation, fresh owners, old-session loss, reset/RX
  gating, redaction, failure, and cleanup through signed direct and production
  processes.

## Portability statement and retained owners

Bangbang's compatibility policy is deterministic: a destination must satisfy
the encoded architecture, page size, native family/minor, CPU/GIC feature,
machine/topology, transport, device-profile, memory-binding, lineage, and
explicit external-authority constraints before any guest-visible mutation.
Tests reject each mismatch and prove immutable repeated loads into distinct
same-host processes, including the fixed production launcher and nested worker.

The certification has **zero tested distinct-physical-host success pairs**.
Accordingly it claims same-host cross-process portability and deterministic
destination rejection, not unconstrained cross-host success. Issue
[#1491](https://github.com/seven332/bangbang/issues/1491) owns explicit
CPU/host/fleet pair selection and any future cross-host success evidence.
Issue [#1378](https://github.com/seven332/bangbang/issues/1378) owns the first
credentialed production vmnet connectivity, service/crash failure, concurrent
session, and cleanup evidence. The later
[Wave 8 cross-capability aggregate](wave8-certification-contract.md) is now
terminal and does not reopen these 68 terminal snapshot identities or close
the external #1378 evidence.

No row claims Firecracker artifact bytes, Linux UFFD wire identity, an
invented per-drive load override, live socket/TCP/packet/backend migration,
state merging, arbitrary KVM register vectors, authentication or encryption
of snapshot artifacts, immutable external writable disks, or portability
beyond the checked destination policy. Operators must protect state, memory,
backing files, selectors, grants, and descriptors as one confidential
authority set.
