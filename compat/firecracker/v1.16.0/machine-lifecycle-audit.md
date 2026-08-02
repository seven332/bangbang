# Firecracker v1.16.0 machine and lifecycle closure audit

This is the review ledger for issue #1408, the final closure slice of #1388
under #1348. It records the disposition of every capability that belonged to
the original Wave 2 family set and the directly related API aggregates reviewed
after all eight implementation children merged.

The immutable upstream baseline is Firecracker commit
`d83d72b710361a10294480131377b1b00b163af8`. The original Wave 2 selector
contains the 28 records below; later local semantic records remain outside this
ledger.

## Original 28-record ledger

The final split is five `implemented-and-verified`, 22 `audit-required` records
with one explicit Wave 7 owner, and one `proven-platform-impossible` record.

| Identity | Final disposition | Evidence or later owner |
| --- | --- | --- |
| `corpus:cpu-boot-protocol` | implemented and verified | Applicable arm64 template-before-boot ordering and PSTATE/PC/X0 overrides are implemented in `crates/hvf/src/{cpu_template,startup}.rs` and signed in `crates/hvf/tests/{guest_boot,hvf_lifecycle}.rs`; the x86 MSR section is architecture-inapplicable. |
| `corpus:cpu-template-helper` | audit required | Wave 7 owns the helper executable, artifact formats, dump/strip/verify/fingerprint behavior, and persistence/host comparison. |
| `corpus:cpu-templates` | audit required | Wave 7 owns the whole-corpus heterogeneous-fleet, helper, portability, expert-guidance, and multi-architecture outcomes; Wave 2 supplies the terminal bounded arm64 runtime policy. |
| `corpus:hugepages` | proven platform impossible | #1391 records the strict Linux hugetlbfs `2M` contract, public XNU/HVF blocker, stable rejection, alternatives, and signed/focused evidence while ordinary memory remains supported. |
| `corpus:rootfs-and-kernel` | audit required | Wave 7 owns host-side Linux construction recipes, other-architecture guidance, and the FreeBSD artifact flow; Wave 2 supplies public arm64 loading and signed boot. |
| `semantic.boot:kernel-rootfs-fdt-and-cache` | implemented and verified | Runtime boot/FDT/startup plus HVF startup implement public kernel/initrd/rootfs/arguments, checked placement, current FDT/cache topology, and failure ordering; signed guest and executable tests cover the boundary. |
| `semantic.cpu:configuration-templates-and-feature-state` | audit required | Wave 7 owns the aggregate because it includes helper, fleet, persisted-artifact, and portability outcomes; Wave 2 supplies its bounded arm64 model, modifiers, boot precedence, and capture/apply primitives. |
| `semantic.lifecycle:pause-resume-quiescence-and-failure` | implemented and verified | #1389/#1390 provide topology-wide idempotent pause/resume and the current complete quiescence/publication transaction with unit and signed evidence. |
| `semantic.lifecycle:smp-psci-and-vcpu-ownership` | implemented and verified | Fixed owner-thread SMP, all-MPIDR FDT input, indexed PSCI, timer suspend, interrupt routing, topology-wide pause ordering, guest terminal outcomes, and cleanup are covered by HVF unit and signed tests. |
| `semantic.memory:machine-sizing-hugepages-and-dirty-tracking` | implemented and verified | #1391/#1395/#1396 provide target-bounded configured-equals-realized sizing, exact `2M` policy, mapped-memory ownership, and complete failure-atomic dirty epochs. |
| `tool-argument:cpu-template-helper/fingerprint/compare/curr` | audit required | Wave 7 owns the persisted current-fingerprint input. |
| `tool-argument:cpu-template-helper/fingerprint/compare/filters` | audit required | Wave 7 owns helper comparison filtering. |
| `tool-argument:cpu-template-helper/fingerprint/compare/prev` | audit required | Wave 7 owns the persisted previous-fingerprint input. |
| `tool-argument:cpu-template-helper/fingerprint/dump/config` | audit required | Wave 7 owns helper preboot configuration input. |
| `tool-argument:cpu-template-helper/fingerprint/dump/output` | audit required | Wave 7 owns fingerprint artifact publication. |
| `tool-argument:cpu-template-helper/fingerprint/dump/template` | audit required | Wave 7 owns helper template application. |
| `tool-argument:cpu-template-helper/template/dump/config` | audit required | Wave 7 owns helper preboot configuration input. |
| `tool-argument:cpu-template-helper/template/dump/output` | audit required | Wave 7 owns template artifact publication. |
| `tool-argument:cpu-template-helper/template/dump/template` | audit required | Wave 7 owns helper template selection. |
| `tool-argument:cpu-template-helper/template/strip/paths` | audit required | Wave 7 owns persisted-template path input. |
| `tool-argument:cpu-template-helper/template/strip/suffix` | audit required | Wave 7 owns strip output naming. |
| `tool-argument:cpu-template-helper/template/verify/config` | audit required | Wave 7 owns helper preboot configuration input. |
| `tool-argument:cpu-template-helper/template/verify/template` | audit required | Wave 7 owns helper template verification input. |
| `tool-operation:cpu-template-helper/fingerprint/compare` | audit required | Wave 7 owns deterministic persisted-fingerprint comparison. |
| `tool-operation:cpu-template-helper/fingerprint/dump` | audit required | Wave 7 owns preboot capture, host fingerprinting, and artifact publication. |
| `tool-operation:cpu-template-helper/template/dump` | audit required | Wave 7 owns preboot CPU-view capture and artifact publication. |
| `tool-operation:cpu-template-helper/template/strip` | audit required | Wave 7 owns persisted-JSON strip transformation. |
| `tool-operation:cpu-template-helper/template/verify` | audit required | Wave 7 owns preboot apply/capture verification. |

The 22 Wave 7 handoffs are nonterminal because their complete public behavior
does not exist yet. They are not platform exclusions. Wave 2's CPU model and
paused capture/apply primitives are dependencies, not evidence that the helper
or whole-corpus fleet workflows have been delivered.

## Related terminal API identities

The following 18 identities are single-purpose on the supported target and have
direct parser/controller/backend implementation plus current focused and signed
validation. They move from `audit-required` to
`implemented-and-verified` in this reconciliation.

| Surface | Promoted identities | Evidence boundary |
| --- | --- | --- |
| Boot source (7) | `api-operation:PUT /boot-source`; `api-path:/boot-source`; `api-schema:BootSource`; `api-property:BootSource.boot_args`; `api-property:BootSource.initrd_path`; `api-property:BootSource.kernel_image_path`; `api-property:FullVmConfiguration.boot-source` | Strict API/config parsing, transactional retained authority, value-redacted faults, kernel/initrd/rootfs/argument loading, FDT publication, GET serialization, and signed public startup. |
| Machine configuration (6) | `api-operation:GET /machine-config`; `api-operation:PUT /machine-config`; `api-operation:PATCH /machine-config`; `api-path:/machine-config`; `api-schema:MachineConfiguration`; `api-property:FullVmConfiguration.machine-config` | Defaults, replacement/partial update, target vCPU and configured-equals-realized memory bounds, SMT/static-template/dirty/exact-2M policy, state admission, serialization, and failure-atomic balloon compatibility. |
| CPU configuration (2) | `api-operation:PUT /cpu-config`; `api-path:/cpu-config` | The already-terminal `CpuConfig` arm64 schema, finite reviewed modifier execution on every vCPU, transactional replacement, value redaction, and stable outcomes for KVM/static/non-executable categories. X86 CPUID/MSR leaves remain separate Wave 7 audit work. |
| VM state (3) | `api-path:/vm`; `api-schema:Vm`; `api-property:Vm.state` | The already-terminal PATCH operation is the path's only operation; Paused/Resumed parsing, idempotent process-owned topology-wide transitions, errors, latency, and signed SMP isolation are covered. |

The public boot-source configuration is not an internal placeholder: the same
accepted state is consumed by startup, and signed executable tests boot its
kernel/initrd or direct rootfs. Similarly, the machine and CPU aggregate
promotions do not infer generalized snapshot or portability support from their
terminal configuration leaves.

## Related retained ownership

The audit deliberately retains broader records even when one of their leaves is
terminal. The directly reviewed identities that do not change disposition are:

| Boundary | Exact identities | Final disposition or owner |
| --- | --- | --- |
| Exported configuration | `api-operation:GET /vm/config`; `api-path:/vm/config`; `api-schema:FullVmConfiguration` | `audit-required`; Wave 8 owns final cross-capability certification after every exported device field has a terminal result. |
| Snapshot create API leaves | `api-operation:PUT /snapshot/create`; `api-path:/snapshot/create`; `api-schema:SnapshotCreateParams`; `api-property:SnapshotCreateParams.snapshot_type`; `api-property:SnapshotCreateParams.snapshot_path`; `api-property:SnapshotCreateParams.mem_file_path` | `implemented-and-verified`; strict paused-only Full/Diff parsing and the two-output public transaction have portable, signed real-HVF, ordinary-production, and App Sandbox evidence. |
| Remaining snapshot load aggregates | `api-operation:PUT /snapshot/load`; `api-path:/snapshot/load`; `api-schema:SnapshotLoadParams` | `audit-required`; #1543 owns the broad load/schema certification beyond the already implemented exact native-family profiles and leaves. |
| Snapshot semantics | `semantic.snapshot:full-create-load-and-public-lifecycle` | `implemented-and-verified`; the public lineage advances from device-free native-v2 2.3 through exact 2.12 Full with all 64 optional-device products, while exact 2.13 Diff adds mandatory base/selection/result kind 14 and retains all earlier readers. Zero-root and matching rebased results load through direct, contained, ordinary-production, and App Sandbox boundaries. |
| Snapshot network/vsock clone semantics | `semantic.snapshot:network-vsock-overrides-portability-and-clones` | `implemented-and-verified`; exact 2.11 supplies complete clone-local network overrides and fresh network/MMDS sessions, while exact 2.12 supplies captured-or-overridden vsock authority, reset/RX/TX, preserved listeners, multistream/half-close, clone-local cursors, immutable independent clones, containment, redaction, and cleanup. Portability is bounded to compatible Bangbang-native File/COW destinations and excludes Firecracker bytes, live-peer migration, automatic socket/grant migration, and unconstrained cross-host execution. |
| Snapshot Diff/rebase leaves | Six snapshot-create leaves plus `tool-operation:rebase-snap/rebase`, `tool-operation:snapshot-editor/edit-memory/rebase`, and their four arguments | `implemented-and-verified`; exact-2.13 tracked/untracked/repeated Diff and both public rebase commands are closed by the checked [15-record ledger](snapshot-diff-rebase-contract.md). |
| Snapshot editor state | Three `info-vmstate` operations and path arguments; `edit-vmstate remove-regs` and its three arguments; `semantic.snapshot:editor-rebase-and-inspection`; `corpus:snapshot-editor` | `implemented-and-verified`; deterministic redacted inspection, finite reviewed editing, immutable no-clobber publication, and signed Full/Diff MMIO/PCI product restoration are closed by the checked [twelve-record ledger](snapshot-editor-contract.md). |
| Retained mixed snapshot aggregates | `semantic.snapshot:diff-dirty-tracking-and-memory-backends`; `corpus:snapshot-versioning` | `audit-required`; #1543 retains native-v2 Uffd, broad version/backend/portability, and aggregate certification. |
| Remaining snapshot composition | `semantic.snapshot:multi-vcpu-drives-devices-and-mmds` | `audit-required`; #1543 owns per-drive overrides and broader cross-profile composition beyond the certified exact complete-set products. |
| Snapshot tracking leaves | `api-property:SnapshotLoadParams.enable_diff_snapshots`; `api-property:SnapshotLoadParams.track_dirty_pages` | Already `implemented-and-verified`; they select complete destination dirty tracking and compose with the separately certified exact-2.13 create/rebase boundary. |
| Broad specifications | `corpus:specification`; `semantic.specification:api-availability-stability-and-failure-information`; `semantic.specification:performance-resource-and-telemetry-outcomes` | `audit-required`; applicable repository-wide outcomes remain Wave 7 work after their producers stabilize. |
| Cross-capability certification | `semantic.cross-capability:state-errors-metrics-security-and-snapshots` | `audit-required`; Wave 8 owns the final interaction audit after the individual lifecycle, error, telemetry, security, device, network, and snapshot producers stabilize. |
| External isolation gates | `semantic.isolation:host-resource-authority-and-brokerage`; `semantic.isolation:jailer-seccomp-and-macos-containment-outcomes`; `semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity` | Unchanged `missing-platform-feasible`; #1351 retains its independent external root, vmnet, credential, and deployment evidence gates. |

Those exact identities establish the following non-overlapping handoffs:

- The snapshot producer now has terminal exact-2.12 Full and exact-2.13 Diff
  create/load behavior for the complete 64-product graph, plus both public
  memory-rebase commands, deterministic state inspection, finite reviewed
  register editing, and retained exact 2.3–2.12/frozen-native-v1 readers.
  #1543 still owns native-v2 Uffd, per-drive overrides, broad
  version/backend/portability work, remaining load/schema aggregates, and
  final cross-profile certification; completed editor leaves do not promote
  those mixed records.
- Wave 7 owns `cpu-template-helper`, host-side kernel/rootfs construction,
  heterogeneous-fleet CPU-template outcomes, and applicable repository-wide
  specification outcomes after producers stabilize.
- Wave 8 owns final cross-capability certification of `GET /vm/config`,
  `api-path:/vm/config`, and `api-schema:FullVmConfiguration`. Their terminal
  boot, machine, and CPU properties do not certify unrelated device fields;
  `semantic.cross-capability:state-errors-metrics-security-and-snapshots`
  remains part of the same final interaction gate.
- #1351 retains only its independent external root/vmnet evidence gates. This
  audit does not change those records or their public behavior.

## Validation

`tools/firecracker-capability-audit/tests/checked_inventory.rs` pins the
original selector, Wave 7 set, promoted identity set, and absence of a stale
future-#1388 handoff. Generic source coverage, global disposition totals,
evidence references, and disposition rules remain owned by the validator and
`capabilities.json`.

Delivery validation must pass, and the generated manifest must still compare
byte-for-byte with a clean Firecracker checkout at
`d83d72b710361a10294480131377b1b00b163af8`. Final #1348 validation continues
to reject the intentionally nonterminal Wave 6/7/8 records until their owners
complete them.
