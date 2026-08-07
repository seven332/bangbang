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

Both gates pin all eleven #1794–#1795 fingerprint, corpus, and aggregate rows
to their exact evidence-free `audit-required` handoff. They do not infer a
host fingerprint, cross-host portability, guest execution, or completion of
those separately owned scopes. The detailed command, format, publication, and
security contracts are in
[CPU-template helper dump and verify](cpu-template-helper-contract.md) and
[CPU-template strip](cpu-template-strip-contract.md).

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

## Exact #1491-owned ledger

| Identity | Owner | Result after #1784 |
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
| `tool-argument:cpu-template-helper/fingerprint/compare/curr` | #1794 | `audit-required` |
| `tool-argument:cpu-template-helper/fingerprint/compare/filters` | #1794 | `audit-required` |
| `tool-argument:cpu-template-helper/fingerprint/compare/prev` | #1794 | `audit-required` |
| `tool-argument:cpu-template-helper/fingerprint/dump/config` | #1794 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/fingerprint/dump/output` | #1794 | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/fingerprint/dump/template` | #1794 | `implemented-and-verified` |
| `tool-operation:cpu-template-helper/fingerprint/compare` | #1794 | `audit-required` |
| `tool-operation:cpu-template-helper/fingerprint/dump` | #1794 | `implemented-and-verified` |
| `corpus:cpu-template-helper` | #1795 | `audit-required` |
| `corpus:cpu-templates` | #1795 | `audit-required` |
| `semantic.cpu:configuration-templates-and-feature-state` | #1795 | `audit-required` |
| `corpus:getting-started` | #1796 | `audit-required` |
| `corpus:rootfs-and-kernel` | #1796 | `audit-required` |
| `corpus:formal-verification` | #1797 | `audit-required` |
| `corpus:network-performance` | #1798 | `audit-required` |
| `corpus:specification` | #1798 | `audit-required` |
| `semantic.specification:performance-resource-and-telemetry-outcomes` | #1798 | `audit-required` |
| `corpus:design` | #1799 | `audit-required` |
| `corpus:device-api` | #1799 | `audit-required` |
| `corpus:release-changelog` | #1799 | `audit-required` |
| `semantic.tools:packaging-help-errors-and-applicable-operations` | #1799 | `audit-required` |
| `semantic.transport:virtio-mmio-activation` | #1799 | `audit-required` |

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
aggregate rows remain unchanged. The current inventory is therefore exactly
358 implemented, 27 audit-required, three
missing-platform-feasible, and 30 proven-platform-impossible. If every other
#1491-owned row later becomes implemented while the nine handoffs remain, the
prospective Wave 7 endpoint is 376/9/3/30. These are exact consequences of the
current row set, not quotas; the authoritative totals remain derived from
`capabilities.json`.
