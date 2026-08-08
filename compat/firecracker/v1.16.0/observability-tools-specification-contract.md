# Firecracker v1.16.0 observability, tools, and specification contract

This is the checked ownership and evidence ledger for Wave 7 parent
[#1491](https://github.com/seven332/bangbang/issues/1491). It is pinned to
Firecracker v1.16.0 commit
`d83d72b710361a10294480131377b1b00b163af8` and Bangbang's supported macOS
arm64/Hypervisor.framework target.

The starting boundary is exact: 93 records belong to #1491 and nine records
remain named #1373, #1378, or Wave 8 handoffs. Ownership is stable even as an
owning child changes its rows from `audit-required` to a supported terminal
disposition. Producer-only children #1785, #1788, and #1789 intentionally own
no aggregate disposition; their merged work feeds the terminal #1786, #1790,
and #1799 certification boundaries respectively.

## Evidence keys

- **FC-API**: pinned
  [`firecracker.yaml`](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/firecracker/swagger/firecracker.yaml)
  operations and schemas.
- **FC-ACTIONS**: pinned strict
  [action parser](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/firecracker/src/api_server/request/actions.rs)
  including the aarch64 `SendCtrlAltDel` rejection.
- **FC-X86**: pinned
  [x86 custom CPU-template implementation](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/cpu_config/x86_64/custom_cpu_template.rs)
  and Swagger's explicit x86-only CPUID/MSR descriptions.
- **FC-ARM**: pinned arm64
  [`CustomCpuTemplate`](https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/cpu_config/aarch64/custom_cpu_template.rs#L29-L48)
  accepts only the arm64 fields and denies unknown architecture fields.
- **APPLE-HVF**: public arm64 HVF exposes typed
  [general-register](https://developer.apple.com/documentation/hypervisor/hv_vcpu_get_reg%28_%3A_%3A_%3A%29),
  [system-register](https://developer.apple.com/documentation/hypervisor/hv_vcpu_get_sys_reg%28_%3A_%3A_%3A%29),
  and
  [feature-register](https://developer.apple.com/documentation/hypervisor/hv_vcpu_config_get_feature_reg%28_%3A_%3A_%3A%29)
  interfaces, not an x86 CPUID leaf/MSR namespace.
- **LOCAL-API**: strict request/response types and routes in
  `crates/api/src/{http,route}.rs`.
- **LOCAL-DISPATCH**: Unix-socket handling and controller dispatch in
  `crates/bangbang/src/api_server.rs`.
- **LOCAL-STATE**: process-owned instance/action/configuration behavior in
  `crates/runtime/src/lib.rs` and `crates/bangbang/src/vmm.rs`.
- **FOCUSED**: API parser/response and controller tests in
  `crates/api/src/http.rs` and `crates/bangbang/src/api_server.rs`.
- **PROCESS**: real executable socket, malformed-route survival,
  configuration, action, state, and redaction tests in
  `crates/bangbang/tests/process_e2e.rs`.
- **SIGNED**: signed real-HVF lifecycle in
  `crates/bangbang/tests/executable_hvf_e2e.rs`, plus App Sandbox and production
  API boundaries in the owning signed targets.
- **METRICS-AUTHORITY**: strict source/policy envelope and exact scoped gate in
  `compat/firecracker/v1.16.0/metrics-schema.json` and
  `tools/firecracker-capability-audit/src/metrics_certify.rs`.
- **METRICS-PRODUCT**: strict parser and direct privacy cases plus the signed
  ordinary-production/App Sandbox real-period lifecycle in
  `crates/api/src/http.rs`, `crates/bangbang/src/api_server.rs`, and
  `crates/launcher/tests/production_bundle_e2e.rs`.
- **METRICS-LIFECYCLE**: exact ten-scenario aggregate authority and certifier in
  `compat/firecracker/v1.16.0/metrics-lifecycle-audit.json` and the audit
  tool's lifecycle validator/certifier modules.
- **TRACING-AUTHORITY**: exact opt-in feature, record, limit, privacy, delivery,
  and eight-call production scope authority in
  `compat/firecracker/v1.16.0/tracing-audit.json`, with its AST validator and
  terminal certifier in the capability-audit tool.

## Core API certification

The four operations expose Firecracker-shaped instance, version, optional
full-configuration, and synchronous-action behavior. Bodyless GETs, exact
method/path matching, strict action JSON, deterministic 200/204/400/413
responses, fixed `fault_message` JSON, state admission, failure atomicity, and
process survival are covered at the appropriate library/process/signed layers.
The instance states are exactly `Not started`, `Running`, and `Paused`.

`FullVmConfiguration` has no required properties in the pinned schema.
Bangbang therefore implements deterministic supported-field export and
optional omission while the separately tracked optional `.logger` and
`.metrics` properties remain owned by #1786 and #1787. This schema result does
not close the Wave 8 interaction semantic.

The terminal API specification semantic is similarly bounded. It covers API
socket availability while the process control loop is alive, strict
request/response/state behavior, survival of malformed or external failures,
value-safe failure information, and current signed lifecycle evidence. It does
not claim comprehensive failure logging (#1786), formal proofs (#1797),
numeric startup/resource/performance or telemetry outcomes (#1798), or final
cross-capability interactions (Wave 8).

The merged #1785 delivery foundation may strengthen logger internals without
changing this ledger: closed 512-byte records, selector templates, bounded
atomic boot admission, one fixed queue/sink owner, exact loss accounting, and
failure-atomic stable-worker replacement are prerequisites for the separately
owned #1786 logger audit. They do not by themselves promote any logger row,
change an evidence owner, or claim comprehensive producer/failure coverage.

## Metrics API/schema certification

The exact twelve #1787 rows are terminal under one fail-closed scoped gate.
Strict `Metrics`, `RateLimiter`, and `TokenBucket` parsing rejects duplicates,
unknowns, negative/fractional/overflowing numbers, and accepts the complete
`u64` domain. Direct and contained `GET /vm/config` responses remain unchanged
after metrics configuration and omit sink paths, grant authority, descriptors,
and private failures. The schema-runtime timestamp profile is implemented, and
one signed product guest observes canonical initial, real 60-second Paused and
Running periodic, explicit, and terminal lines.

The gate requires the exact #1787 row set, one implemented schema-runtime
profile, two implemented process-lifecycle profiles, one complete nonhybrid
device transition shape, and a coherent #1790 aggregate pair. The checked
authority uses ten implemented #1789 device profiles. The separate
`validate --metrics-process-final` gate applies final-mode validation to all
69 exact #1788 producer records, while `validate --metrics-device-final`
additionally requires the exact 231-record 212/2/17 device census and
resolvable common evidence.

The terminal `validate --metrics-final` gate composes those scopes with the
checked ten-record lifecycle matrix. It requires exact initial, real
60-second, explicit, terminal, backpressure, transaction, configured
cardinality, snapshot-destination, hotplug/reuse, and process-isolation rows.
The transaction row alone owns complete-line commit atomicity,
previous-success retry, concurrent-cut ownership, lost-output accounting, and
one-shot final behavior. Both #1790 capability rows and owner rows are now
`implemented-and-verified`; partial promotion, claim/evidence drift, or a
regressed producer census fails closed.

## Developer-tracing certification

The single #1791 row is terminal under `validate --tracing-final`. The gate
composes the terminal logger foundation with an exact AST scan of eight
literal API, VMM, controller, virtio-MMIO, and snapshot-tool scopes. Tracing is
absent from default and default-release builds; enabled VMM scopes still obey
the configured Trace level and module prefix, while standalone tools also
require `BANGBANG_TRACE` runtime admission. Each thread has a 32-entry fixed
stack and each scope emits at most two 512-byte records through bounded host,
bounded tool, or nonblocking guest delivery.

Only literal module/scope, opaque Rust thread identity, and enter/exit phase
are admitted. Result-preservation, loss accounting, unwind and forgotten-scope
recovery, thread isolation, redaction, default expression removal, and marker
absence are directly tested. The contract does not claim Firecracker's source
rewrite mechanism, durable delivery, tracing enabled by default, sensitive
dynamic fields, or portable timing thresholds.

## CPU-template helper certification

The exact seven #1792 rows are terminal under
`validate --cpu-template-helper-final`. The gate requires one coherent
transition, exact anchored implementation and validation evidence, the strict
public binary, and the signed real-HVF five-case harness. Effective capture
uses one disposable VM and the complete requested vCPU topology, production
template apply/readback, all 80 descriptor slots, explicit platform-version
availability, and teardown-before-success. Canonical dump publication is
private and absent-only; verify success is silent; every failure is bounded
and value/path-redacted.

The independent `validate --cpu-template-strip-final` gate requires the seven
dump/verify dependencies and promotes the exact three #1793 path, suffix, and
operation rows. It certifies portable normalized arm64 common-bit stripping,
strict inputs, canonical outputs, and per-path atomic multi-directory
publication with explicit rollback and uncertainty semantics. It does not
claim a global or crash-atomic multi-path transaction.

The ordered transition additionally preserves an independent
`validate --cpu-template-fingerprint-dump-final` gate for #1866's exact three
dump arguments and operation, and a
`validate --cpu-template-fingerprint-compare-final` gate for #1867's exact
three compare arguments and operation. Dump certifies the versioned tagged
document, reviewed macOS facts, signed effective capture, strict reparse, and
absent-only publication. Compare certifies portable strict persisted inputs,
platform-honest filters, deterministic bounded selected-value diagnostics,
and exact guest common-bit stripping without calling either provider.

All earlier scoped gates remain valid after these ordered transitions. The
aggregate `validate --cpu-template-final` gate then promotes exactly the three
#1795 corpus/semantic rows from one versioned checked producer ledger. Its five
operation records own all thirteen arguments and five operations exactly once;
its closed scenarios compose the installed CLI, canonical artifact pipelines,
runtime selection, real all-vCPU apply/readback, boot precedence, native-v1
no-template snapshot boundary, and applicable fleet workflow.

The gate separately requires the exact implemented CPU/machine/config-export
foundations and `corpus:cpu-boot-protocol`, plus the exact terminal CPUID/MSR,
KVM-vCPU, and complete static-template platform exclusions. The signed
five-command helper flow proves artifact composition; existing signed lifecycle
and guest-boot tests retain authority for effective state and boot behavior.
Terminal heterogeneous-fleet coverage means the applicable creation,
inspection, strip, verify, and platform-tagged comparison workflow plus expert
and platform guidance. It does not infer distinct-host equivalence or safety,
template correctness, artifact authenticity, x86/KVM mechanism identity,
snapshot portability, or migration safety. The detailed command, format,
publication, and security contracts are in
[CPU-template helper dump and verify](cpu-template-helper-contract.md) and
[CPU-template strip](cpu-template-strip-contract.md),
[CPU-template fingerprint dump](cpu-template-fingerprint-contract.md), and
[CPU-template fingerprint compare](cpu-template-fingerprint-compare-contract.md).

## X86 CPUID/MSR platform boundary

All 13 CPUID/MSR identities are executable x86_64 contracts. ARM register
reinterpretation would change their architecture and bitmap semantics; an x86
emulator or Linux/KVM sidecar would change the native backend and one-process
boundary; silently accepting them would report success without enforcing the
requested CPU state. None is an identity-preserving implementation on macOS
arm64/HVF.

Bangbang retains Firecracker's architecture-strict behavior: complete
`cpuid_modifiers` or `msr_modifiers` shapes are rejected before conversion,
runtime mutation, backend construction, or start. The response is the fixed
malformed-request fault and contains no leaf, subleaf, flags, register,
address, or bitmap value. The focused tests are
`rejects_complete_x86_cpu_config_shapes_without_retaining_values` and the x86
cases in `executable_configures_vm_before_start`.

The current platform decision is the #1784
[Plan Challenge](https://github.com/seven332/bangbang/issues/1784#issuecomment-5161129449).
The existing arm64 `kvm_capabilities`, `vcpu_features`, and reviewed
`reg_modifiers` behavior is unchanged; its own exact platform/safety outcomes
remain in the CPU-template contract.

## Wave 7 aggregate certification

The terminal [Wave 7 aggregate authority](wave7-aggregate-audit.json) closes
#1799 without hiding a producer or importing live sibling state. It binds the
pinned design, device API, and v1.16.0 changelog blobs to five complete
ledgers: all 37 semantic identities; all 958 device-table relations, including
62 exact required producer mappings and 896 explicit optional cells; all 261
generated API identities; all 21 ordered release entries; all 55 public-tool
leaves; and the common virtio-MMIO transport plus every supported device
profile. Historical device-API schema names and its `block_size_mi` spelling
are normalized explicitly, while arm64 `SendCtrlAltDel` remains an intentional
rejection.

The scoped terminal gate is:

```console
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --wave7-final
```

It composes every earlier Wave 7 authority and requires the exact 93-row parent
distribution: 80 implemented and 13 proven-platform-impossible. Repository
totals are exactly 376 implemented, nine audit-required, three
missing-platform-feasible, and 30 proven-platform-impossible. The nonterminal
set is also exact: three feasible #1351 isolation outcomes, six #1373
jailer/production-host outcomes, two #1378 network outcomes, and one Wave 8
interaction outcome. Aggregate completion does not infer completion of any of
those handoffs.

That 376/9/3/30 paragraph is the immutable #1799 phase recorded by this Wave 7
authority. The phase-aware certifier also accepts exactly one successor:

```console
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --wave8-final
```

The successor promotes only
`semantic.cross-capability:state-errors-metrics-security-and-snapshots` under
the checked [Wave 8 contract](wave8-certification-contract.md), producing
377/8/3/30. The remaining six #1373, two #1378, and three #1351 external
outcomes stay nonterminal. Any other identity or count transition fails the
Wave 7 gate.

The public-tool ledger derives 46 implemented leaves, five terminal Linux-only
jailer exclusions, and four #1373 jailer handoffs. The virtio-MMIO ledger
requires common identity/features/queue/notification/interrupt/status/reset/
activation/configuration/restore/observability behavior, file and vhost-user
block variants, pmem, network, vsock, entropy, balloon, and virtio-mem, with
focused, formal, and signed evidence. PCI-only evidence cannot substitute for
MMIO evidence. Release completion retains Linux 6.18 as a #1373 Linux-host
statement; it does not claim that macOS supplies that kernel. No aggregate row
claims Firecracker binary/Linux/KVM identity, portable performance thresholds,
tracked machine reports, credentials or network ownership, or final Wave 8
interactions.

## Exact #1491-owned ledger

| Identity | Owner | Terminal Wave 7 result |
| --- | --- | --- |
| `api-operation:GET /` | #1784 | `implemented-and-verified` |
| `api-operation:GET /version` | #1784 | `implemented-and-verified` |
| `api-operation:GET /vm/config` | #1784 | `implemented-and-verified` |
| `api-operation:PUT /actions` | #1784 | `implemented-and-verified` |
| `api-path:/` | #1784 | `implemented-and-verified` |
| `api-path:/actions` | #1784 | `implemented-and-verified` |
| `api-path:/version` | #1784 | `implemented-and-verified` |
| `api-path:/vm/config` | #1784 | `implemented-and-verified` |
| `api-property:CpuConfig.cpuid_modifiers` | #1784 | `proven-platform-impossible` |
| `api-property:CpuConfig.msr_modifiers` | #1784 | `proven-platform-impossible` |
| `api-property:CpuidLeafModifier.flags` | #1784 | `proven-platform-impossible` |
| `api-property:CpuidLeafModifier.leaf` | #1784 | `proven-platform-impossible` |
| `api-property:CpuidLeafModifier.modifiers` | #1784 | `proven-platform-impossible` |
| `api-property:CpuidLeafModifier.subleaf` | #1784 | `proven-platform-impossible` |
| `api-property:CpuidRegisterModifier.bitmap` | #1784 | `proven-platform-impossible` |
| `api-property:CpuidRegisterModifier.register` | #1784 | `proven-platform-impossible` |
| `api-property:Error.fault_message` | #1784 | `implemented-and-verified` |
| `api-property:FirecrackerVersion.firecracker_version` | #1784 | `implemented-and-verified` |
| `api-property:InstanceActionInfo.action_type` | #1784 | `implemented-and-verified` |
| `api-property:InstanceInfo.app_name` | #1784 | `implemented-and-verified` |
| `api-property:InstanceInfo.id` | #1784 | `implemented-and-verified` |
| `api-property:InstanceInfo.state` | #1784 | `implemented-and-verified` |
| `api-property:InstanceInfo.vmm_version` | #1784 | `implemented-and-verified` |
| `api-property:MsrModifier.addr` | #1784 | `proven-platform-impossible` |
| `api-property:MsrModifier.bitmap` | #1784 | `proven-platform-impossible` |
| `api-schema:CpuidLeafModifier` | #1784 | `proven-platform-impossible` |
| `api-schema:CpuidRegisterModifier` | #1784 | `proven-platform-impossible` |
| `api-schema:Error` | #1784 | `implemented-and-verified` |
| `api-schema:FirecrackerVersion` | #1784 | `implemented-and-verified` |
| `api-schema:FullVmConfiguration` | #1784 | `implemented-and-verified` |
| `api-schema:InstanceActionInfo` | #1784 | `implemented-and-verified` |
| `api-schema:InstanceInfo` | #1784 | `implemented-and-verified` |
| `api-schema:MsrModifier` | #1784 | `proven-platform-impossible` |
| `corpus:actions-api` | #1784 | `implemented-and-verified` |
| `semantic.specification:api-availability-stability-and-failure-information` | #1784 | `implemented-and-verified` |
| `api-operation:PUT /logger` | #1786 | `implemented-and-verified` |
| `api-path:/logger` | #1786 | `implemented-and-verified` |
| `api-property:FullVmConfiguration.logger` | #1786 | `implemented-and-verified` |
| `api-property:Logger.level` | #1786 | `implemented-and-verified` |
| `api-property:Logger.log_path` | #1786 | `implemented-and-verified` |
| `api-property:Logger.module` | #1786 | `implemented-and-verified` |
| `api-property:Logger.show_level` | #1786 | `implemented-and-verified` |
| `api-property:Logger.show_log_origin` | #1786 | `implemented-and-verified` |
| `api-schema:Logger` | #1786 | `implemented-and-verified` |
| `corpus:logger` | #1786 | `implemented-and-verified` |
| `semantic.observability:logger-delivery-filtering-loss-and-redaction` | #1786 | `implemented-and-verified` |
| `api-operation:PUT /metrics` | #1787 | `implemented-and-verified` |
| `api-path:/metrics` | #1787 | `implemented-and-verified` |
| `api-property:FullVmConfiguration.metrics` | #1787 | `implemented-and-verified` |
| `api-property:Metrics.metrics_path` | #1787 | `implemented-and-verified` |
| `api-property:RateLimiter.bandwidth` | #1787 | `implemented-and-verified` |
| `api-property:RateLimiter.ops` | #1787 | `implemented-and-verified` |
| `api-property:TokenBucket.one_time_burst` | #1787 | `implemented-and-verified` |
| `api-property:TokenBucket.refill_time` | #1787 | `implemented-and-verified` |
| `api-property:TokenBucket.size` | #1787 | `implemented-and-verified` |
| `api-schema:Metrics` | #1787 | `implemented-and-verified` |
| `api-schema:RateLimiter` | #1787 | `implemented-and-verified` |
| `api-schema:TokenBucket` | #1787 | `implemented-and-verified` |
| `corpus:metrics` | #1790 | `implemented-and-verified` |
| `semantic.observability:metrics-schema-producers-flush-and-lifecycle` | #1790 | `implemented-and-verified` |
| `corpus:tracing` | #1791 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/template/dump/config` | #1792 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/template/dump/output` | #1792 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/template/dump/template` | #1792 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/template/verify/config` | #1792 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/template/verify/template` | #1792 | `implemented-and-verified` |
| `tool-operation:cpu-template-helper/template/dump` | #1792 | `implemented-and-verified` |
| `tool-operation:cpu-template-helper/template/verify` | #1792 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/template/strip/paths` | #1793 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/template/strip/suffix` | #1793 | `implemented-and-verified` |
| `tool-operation:cpu-template-helper/template/strip` | #1793 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/fingerprint/compare/curr` | #1794 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/fingerprint/compare/filters` | #1794 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/fingerprint/compare/prev` | #1794 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/fingerprint/dump/config` | #1794 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/fingerprint/dump/output` | #1794 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/fingerprint/dump/template` | #1794 | `implemented-and-verified` |
| `tool-operation:cpu-template-helper/fingerprint/compare` | #1794 | `implemented-and-verified` |
| `tool-operation:cpu-template-helper/fingerprint/dump` | #1794 | `implemented-and-verified` |
| `corpus:cpu-template-helper` | #1795 | `implemented-and-verified` |
| `corpus:cpu-templates` | #1795 | `implemented-and-verified` |
| `semantic.cpu:configuration-templates-and-feature-state` | #1795 | `implemented-and-verified` |
| `corpus:getting-started` | #1796 | `implemented-and-verified` |
| `corpus:rootfs-and-kernel` | #1796 | `implemented-and-verified` |
| `corpus:formal-verification` | #1797 | `implemented-and-verified` |
| `corpus:network-performance` | #1798 | `implemented-and-verified` |
| `corpus:specification` | #1798 | `implemented-and-verified` |
| `semantic.specification:performance-resource-and-telemetry-outcomes` | #1798 | `implemented-and-verified` |
| `corpus:design` | #1799 | `implemented-and-verified` |
| `corpus:device-api` | #1799 | `implemented-and-verified` |
| `corpus:release-changelog` | #1799 | `implemented-and-verified` |
| `semantic.tools:packaging-help-errors-and-applicable-operations` | #1799 | `implemented-and-verified` |
| `semantic.transport:virtio-mmio-activation` | #1799 | `implemented-and-verified` |

## Exact retained handoffs

| Identity | Retained owner | Current disposition |
| --- | --- | --- |
| `corpus:jailer` | #1373 | `audit-required` |
| `corpus:production-host` | #1373 | `audit-required` |
| `tool-argument:jailer/chroot-base-dir` | #1373 | `audit-required` |
| `tool-argument:jailer/gid` | #1373 | `audit-required` |
| `tool-argument:jailer/uid` | #1373 | `audit-required` |
| `tool-operation:jailer/run` | #1373 | `audit-required` |
| `corpus:network-setup` | #1378 | `audit-required` |
| `semantic.network:virtio-net-vmnet-policy-and-connectivity` | #1378 | `audit-required` |
| `semantic.cross-capability:state-errors-metrics-security-and-snapshots` | Wave 8 | `audit-required` |

## Disposition accounting

#1784 moved 22 rows to `implemented-and-verified` and 13 rows to
`proven-platform-impossible`. #1807 subsequently moves the nine concrete
logger operation/path/schema/property rows to `implemented-and-verified`, and
#1810 certifies and promotes both aggregate logger rows after the exact
producer, focused, process, signed, contained, and isolation gates pass. The
#1787 metrics schema/API certification moves its exact twelve rows to
`implemented-and-verified`; #1790 now promotes its exact two aggregate rows
after the schema, process, device, lifecycle, and signed gates pass. #1791
promotes the single tracing corpus row after its feature, AST, privacy,
delivery, process, device, tool, and release-marker gates pass. #1792 promotes
its exact seven CPU-template dump/verify operation and argument rows after the
portable actual-process and signed real-HVF gates pass. #1793 promotes its
exact three portable strip rows after transformation, publication, process,
and audit gates pass. #1866 promotes the three fingerprint-dump arguments and
operation after the closed platform document, public macOS fact, signed
effective-state, publication, and scoped-audit gates pass; compare and #1795
aggregate rows remain unchanged. #1867 then promotes the three compare
arguments and operation after strict persisted-input, platform/filter,
canonical-diagnostic, guest-strip, portable-process, and scoped-audit gates
pass. #1795 finally promotes its exact three aggregate rows after the canonical
producer ledger, exact runtime-foundation dispositions, signed five-command
composition, lifecycle/guest-boot, native-v1 snapshot-boundary, documentation,
and fail-closed transition gates pass. #1872 then certifies #1796's exact two
getting-started and rootfs/kernel corpus rows through the pinned [macOS API and
no-API guest workflow](../../../docs/macos-guest-workflow.md), signed HVF
execution, and fail-closed terminal audit. #1797 promotes the
formal-verification corpus after the exact five bounded Kani harnesses, pinned
toolchain, source-manifest bijection, Linux proof runner, documented [proof
scope](../../../docs/formal-verification.md), and [versioned
contract](formal-verification-contract.md) all pass their fail-closed gates.
#1798 then promotes its exact three reference and measured-outcome rows after
the strict signed [specification benchmark](../../../docs/specification-benchmarks.md),
real FIFO loss/replay, canonical comparison, optional-fixture, documentation,
and terminal-audit gates pass. #1799 finally promotes its five aggregate rows
after the checked source-complete authority derives every design semantic,
device relation and API identity, release entry, public-tool leaf, and
virtio-MMIO device profile while retaining all external handoffs. The #1799
inventory was therefore exactly 376 implemented, nine audit-required, three
missing-platform-feasible, and 30 proven-platform-impossible. The current Wave
8 successor is exactly 377/eight/three/30. These are exact consequences of the
corresponding row sets, not quotas; authoritative current totals remain
derived from `capabilities.json` and are rechecked by `validate --wave8-final`.
