# Firecracker v1.16.0 machine and lifecycle closure audit

This is the review ledger for issue #1408, the final closure slice of #1388
under #1348. It records the disposition of every capability that belonged to
the original Wave 2 family set and the directly related API aggregates reviewed
after all eight implementation children merged.

The generated `source-manifest.json` contains 381 Firecracker source identities.
The human-owned delivery overlay contains those identities plus 37 local
`semantic.*` identities, for 418 records total. The original Wave 2 baseline at
commit `ed60a1abe850db7dbddc836d6316e3663381e8b9` contained 417 overlay records;
issue #1392 later added `semantic.boot:arm64-cache-fdt`. That local cache
record is implemented and verified, but is not one of the original 28 below.

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
| Snapshot API aggregates | `api-operation:PUT /snapshot/create`; `api-operation:PUT /snapshot/load`; `api-path:/snapshot/create`; `api-path:/snapshot/load`; `api-schema:SnapshotCreateParams`; `api-schema:SnapshotLoadParams` | `audit-required`; Wave 6 owns generalized Full/Diff artifacts, merge/restore, overrides, backends, and portability beyond the native-v1 baseline. |
| Snapshot semantics | `semantic.snapshot:diff-dirty-tracking-and-memory-backends`; `semantic.snapshot:full-create-load-and-public-lifecycle`; `semantic.snapshot:multi-vcpu-drives-devices-and-mmds`; `semantic.snapshot:network-vsock-overrides-portability-and-clones` | `audit-required`; Wave 6 owns their incomplete generalized artifact and profile outcomes. |
| Snapshot tracking leaves | `api-property:SnapshotLoadParams.enable_diff_snapshots`; `api-property:SnapshotLoadParams.track_dirty_pages` | Already `implemented-and-verified`; they select complete destination dirty tracking but do not imply Diff artifact support. |
| Broad specifications | `corpus:specification`; `semantic.specification:api-availability-stability-and-failure-information`; `semantic.specification:performance-resource-and-telemetry-outcomes` | `audit-required`; applicable repository-wide outcomes remain Wave 7 work after their producers stabilize. |
| Cross-capability certification | `semantic.cross-capability:state-errors-metrics-security-and-snapshots` | `audit-required`; Wave 8 owns the final interaction audit after the individual lifecycle, error, telemetry, security, device, network, and snapshot producers stabilize. |
| External isolation gates | `semantic.isolation:host-resource-authority-and-brokerage`; `semantic.isolation:jailer-seccomp-and-macos-containment-outcomes`; `semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity` | Unchanged `missing-platform-feasible`; #1351 retains its independent external root, vmnet, credential, and deployment evidence gates. |

Those exact identities establish the following non-overlapping handoffs:

- Wave 6 owns generalized Full and Diff snapshot create/load artifacts,
  multi-vCPU and optional-device state, dirty-image serialization and merging,
  restore overrides, memory backends, portability, and schema evolution. The
  terminal load tracking properties and complete dirty epochs are prerequisites,
  not proof of those artifacts.
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

## Count reconciliation and validation

The 21 promotions above move the current overlay from 49/349/3/17 to:

| Disposition | Records |
| --- | ---: |
| `implemented-and-verified` | 70 |
| `audit-required` | 328 |
| `missing-platform-feasible` | 3 |
| `proven-platform-impossible` | 17 |
| **Total** | **418** |

`tools/firecracker-capability-audit/tests/checked_inventory.rs` pins the
original ledger, Wave 7 set, promoted identity set, counts, and absence of a
future-#1388 reconciliation placeholder. Generic source coverage, evidence
reference, and disposition rules remain owned by the existing validator.

Delivery validation must pass, and the generated manifest must still compare
byte-for-byte with a clean Firecracker checkout at
`d83d72b710361a10294480131377b1b00b163af8`. Final #1348 validation continues
to reject the intentionally nonterminal Wave 6/7/8 records until their owners
complete them.
