# Firecracker Validation Matrix

This page is the compact, current-state index for bangbang's Firecracker-facing
surface. Use
[Firecracker Compatibility Scope](firecracker-compatibility.md) for exact API,
CLI, field, and runtime behavior; use the
[v1.16.0 capability inventory](../compat/firecracker/v1.16.0/README.md) for
machine-checked identities, dispositions, and evidence.

Delivery history belongs in Git and GitHub, not in this matrix.

## Status Vocabulary

- **implemented**: the documented bangbang behavior exists and has matching
  validation.
- **implemented subset**: the documented subset exists, while explicitly
  excluded Firecracker behavior does not.
- **compatibility reader**: an older bangbang-native format remains accepted,
  but is no longer emitted by the public writer.
- **platform limit**: the exact Firecracker behavior depends on Linux/KVM
  facilities that cannot be provided through the supported macOS/HVF surface.

These labels summarize behavior for readers. They are not capability-inventory
dispositions. The checked inventory uses `audit-required`,
`missing-platform-feasible`, `implemented-and-verified`, and
`proven-platform-impossible`; only
[`capabilities.json`](../compat/firecracker/v1.16.0/capabilities.json) is
authoritative for those values and their current totals.

## Validation Layers

The project uses crate-local unit tests, real API-socket and process tests,
signed process and HVF tests, signed App Sandbox tests, production-bundle tests,
pinned-source comparison, and checked documentation evidence. The commands and
selection rules live in [Testing Guide](testing.md#running-tests).

## Current Matrix

| Area | Current boundary | Evidence and detail |
| --- | --- | --- |
| Process CLI and API socket | **Implemented subset.** The checked startup contract covers argument parsing and precedence, readiness, file-descriptor behavior, signals, socket lifecycle, configuration startup, PCI selection, snapshot version/description output, and stable platform faults for Linux seccomp options. | [Process startup CLI](firecracker-compatibility.md#process-startup-cli), [process contract](../compat/firecracker/v1.16.0/process-contract.md) |
| API reads and configuration | **Implemented subset.** Instance/version/configuration reads and the documented machine, boot, drive, network, vsock, balloon, entropy, logger, metrics, MMDS, snapshot, and lifecycle request subsets use Firecracker-shaped endpoints and faults. Unsupported fields and operations fail explicitly. | [Endpoint compatibility matrix](firecracker-compatibility.md#endpoint-compatibility-matrix), [initial field handling policy](firecracker-compatibility.md#initial-field-handling-policy) |
| VM lifecycle and vCPUs | **Implemented subset.** Host-limited multi-vCPU start, process-owned pause/resume, topology-wide transition acknowledgement, PSCI power sessions, and the native snapshot transaction are implemented. Dynamic CPU topology and the documented non-timer/platform gaps remain outside the supported boundary. | [API state and response policy](firecracker-compatibility.md#api-state-and-response-policy), [snapshot ownership](snapshot-feasibility.md#current-ownership-and-pause-boundary), [machine lifecycle audit](../compat/firecracker/v1.16.0/machine-lifecycle-audit.md) |
| Machine memory and dirty tracking | **Implemented subset.** Anonymous and File/COW RAM, descriptor-backed boot RAM, shared dirty epochs, native-v2 Diff selection, and virtio-mem mixed-memory restore are supported. Linux hugetlbfs `2M` and native-v2 Uffd are not aliases for macOS page sizes or HVF mappings. | [Guest memory address space](firecracker-compatibility.md#guest-memory-address-space), [machine-memory contract](../compat/firecracker/v1.16.0/machine-memory-contract.md), [memory-hotplug contract](../compat/firecracker/v1.16.0/memory-hotplug-contract.md) |
| Virtio transports and devices | **Implemented subset.** Coherent all-MMIO or all-PCI startup supports the documented block, pmem, serial, entropy, balloon, virtio-mem, network/MMDS, and vsock graphs. Runtime block, pmem, and network lifecycle is supported where documented; automatic guest hotplug notification is not claimed. | [PCI startup](firecracker-compatibility.md#pci-segment-and-all-virtio-startup), [device hotplug contract](../compat/firecracker/v1.16.0/device-hotplug-contract.md), [remaining-device contract](../compat/firecracker/v1.16.0/remaining-device-contract.md) |
| Storage | **Implemented subset.** Direct and contained regular-file block/pmem ownership, portable Sync/Async block behavior, vhost-user block lifecycle, profile-3 native-v2 capture/restore, failure-atomic replacement, and cleanup are covered. Vhost-user snapshot state remains rejected. | [Aggregate storage closure](firecracker-compatibility.md#aggregate-storage-closure), [storage contract](../compat/firecracker/v1.16.0/storage-contract.md) |
| Network and MMDS | **Implemented subset.** MMIO/PCI interfaces, portable queue/limiter behavior, MMDS V1/V2, live direct-vmnet ownership, capture-ready state, and exact native-v2 2.11-or-newer reconstruction are supported. External vmnet connectivity and broad performance parity remain explicit nonclaims. | [Aggregate network and MMDS closure](firecracker-compatibility.md#aggregate-network-and-mmds-closure), [network/MMDS contract](../compat/firecracker/v1.16.0/network-mmds-contract.md) |
| Vsock | **Implemented subset.** The live MMIO/PCI Unix-socket subset and exact native-v2 2.12 CID/selector/cursor/queue/reset/placement state are supported with captured or explicit clone-local authority. Live-peer, socket, grant, and connection migration are excluded. | [Internal vsock configuration](firecracker-compatibility.md#internal-vsock-configuration), [vsock contract](../compat/firecracker/v1.16.0/vsock-contract.md), [snapshot 2.12 boundary](snapshot-feasibility.md#native-v2-212-vsock-activation-and-certification) |
| Observability and serial | **Implemented subset; logger, opt-in developer tracing, and the complete metrics corpus/lifecycle aggregate are certified.** Tracing is absent from default builds and admits exactly eight literal API/VMM/device/tool scopes with fixed nesting, record, privacy, and bounded/nonblocking delivery rules. Metrics retain the exact API/schema, 69 process records (64 implemented/one source-neutral/four platform-zero), 231 device records (212 implemented/two source-neutral/17 platform-zero), ten shared device profiles, and a checked ten-scenario aggregate matrix. The aggregate gate covers initial, real 60-second, explicit, terminal, backpressure, partial/retry, configured cardinality, snapshot-destination freshness, hotplug/reuse, and process isolation. Its composed oracle proves coherent process/device commit, previous-success retry, concurrent-cut ownership, lost-output accounting, and one-shot final behavior while explicitly allowing an ambiguously visible prefix followed by at-least-once replay. Signed direct, App Sandbox, snapshot, real-periodic production, and actual contained-worker/supervisor tests retain the product evidence. Serial output/input policy and exact native-v2 serial state remain supported subsets. Log rotation, crash durability, exactly-once output, default-enabled or durable tracing, remote telemetry, and a public serial streaming API are not claimed. | [Observability contract](firecracker-compatibility.md#firecracker-v1160-observability-contract), [logger contract](../compat/firecracker/v1.16.0/logger-contract.md), [tracing contract](../compat/firecracker/v1.16.0/tracing-contract.md), [metrics contract](../compat/firecracker/v1.16.0/metrics-contract.md), [serial contract](../compat/firecracker/v1.16.0/serial-contract.md) |
| Current snapshots | **Implemented and Wave 6 certified.** Public Full/File emits bangbang-native `2.12.0` state plus a complete image; Diff/File emits `2.13.0` state plus a tracked-dirty or all-current-page layer. Frozen native-v1 File/Uffd and exact native-v2 2.3–2.13 readers, strict load schema, all optional-device products, tools, time/identity, failure policy, and same-host cross-process portability are bound by one exact 70-row ledger. Artifacts are not Firecracker snapshot bytes, native-v2 rejects Uffd, and no distinct-host success pair is claimed. | [Snapshot current status](snapshot-feasibility.md#current-status), [Wave 6 contract](../compat/firecracker/v1.16.0/snapshot-wave6-contract.md) |
| Snapshot tools | **Implemented tool subset.** Deprecated `rebase-snap` and `snapshot-editor edit-memory rebase` share one macOS native-v2 staged no-clobber transaction. `snapshot-editor info-vmstate` reports exact versions plus deterministic redacted vCPU/VM JSON, and `edit-vmstate remove-regs` admits only a reviewed 67-ID aarch64 registry into a distinct canonical state. Signed Full 2.12 and Diff 2.13 MMIO/PCI product evidence restores the edited state through App Sandbox. State merging, Firecracker bytes, arbitrary KVM vectors, live-peer migration, and untested cross-host success remain excluded. | [Snapshot Rebase Tools](firecracker-compatibility.md#snapshot-rebase-tools), [Snapshot State Inspection and Reviewed Editing](firecracker-compatibility.md#snapshot-state-inspection-and-reviewed-editing), [Diff/rebase contract](../compat/firecracker/v1.16.0/snapshot-diff-rebase-contract.md), [snapshot-editor contract](../compat/firecracker/v1.16.0/snapshot-editor-contract.md), [Wave 6 contract](../compat/firecracker/v1.16.0/snapshot-wave6-contract.md) |
| Frozen native-v1 and demand paging | **Compatibility reader.** Frozen native-v1 File and the bounded macOS UFFD-equivalent reader remain supported under their exact fixed-memory, dirty-tracking, authority, protocol, and consumer gates. Current native-v2 rejects Uffd, and no Linux UFFD wire compatibility is claimed. | [Public UFFD-equivalent feasibility](snapshot-feasibility.md#public-uffd-equivalent-feasibility), [snapshot-paging contract](../compat/firecracker/v1.16.0/snapshot-paging-contract.md), [pager protocol](snapshot-pager-protocol.md) |
| Time and identity | **Implemented subset.** PL031, VMGenID, VMClock, ARM PVTime accounting, and their documented native snapshot clone semantics are supported. | [RTC-adjacent devices](firecracker-compatibility.md#rtc-adjacent-time-and-identity-devices), [time/identity contract](../compat/firecracker/v1.16.0/time-identity-contract.md) |
| CPU templates | **Implemented subset.** A finite reviewed arm64 custom register profile, transactional selection, exact startup/readback behavior, native-v2 evidence, signed `cpu-template-helper template dump/verify` over a real topology-common HVF checkpoint, portable canonical `template strip`, and signed platform-tagged `fingerprint dump` are supported. Dump artifacts are private and absent-only; verification and strip success are silent. Strip provides strict inputs, native-width common-bit transformation, per-path atomic absent/exact-replacement publication, rollback, and explicit uncertainty without claiming a global or crash-atomic batch. Fingerprints use closed macOS/Linux provenance, reviewed public macOS facts, strict reparse, and the same effective guest document; they are diagnostic rather than compatibility authority. KVM/static-template execution, fingerprint compare/filters, corpus-wide template parity, and broader cross-host portability remain separate or excluded as documented. | [Arm64 CPU-template subset](firecracker-compatibility.md#arm64-cpu-template-subset), [CPU-template dump and verify helper](firecracker-compatibility.md#cpu-template-dump-and-verify-helper), [CPU-template strip](firecracker-compatibility.md#cpu-template-strip), [CPU-template fingerprint dump](firecracker-compatibility.md#cpu-template-fingerprint-dump), [CPU-template contract](../compat/firecracker/v1.16.0/cpu-template-contract.md), [helper contract](../compat/firecracker/v1.16.0/cpu-template-helper-contract.md), [strip contract](../compat/firecracker/v1.16.0/cpu-template-strip-contract.md), [fingerprint contract](../compat/firecracker/v1.16.0/cpu-template-fingerprint-contract.md) |
| Offline seccompiler | **Implemented tool subset.** The pinned v1.16 command surface and policy semantics are implemented and checked against a Linux oracle. The tool does not install or enforce seccomp in the VMM. | [Offline seccompiler compatibility](firecracker-compatibility.md#offline-seccompiler-compatibility) |
| macOS production isolation | **Implemented subset with platform limits.** The fixed launcher/worker topology, App Sandbox and HVF entitlement boundary, authenticated startup policy, typed resource grants, exact contained facets, cleanup, and signed validation are supported. Linux seccomp, cgroup, network-namespace, and PID-namespace mechanisms are terminal platform exclusions, not native aliases. | [Security model](security.md), [isolation contract](../compat/firecracker/v1.16.0/isolation-contract.md) |
| Capability inventory | **Checked delivery state.** Every generated identity has one human-owned overlay; terminal claims require resolvable implementation and validation evidence. | [Inventory guide](../compat/firecracker/v1.16.0/README.md), [testing](testing.md#firecracker-capability-inventory) |

### Issue #1389 PATCH /vm Validation Note

Valid same-state `Paused` and `Resumed` requests return success, require a
retained process session, skip another backend command and generation, preserve
state, and still record successful API-request latency. Runtime, API-socket,
process, and signed single-/dual-process tests cover this contract.
Snapshot-ready quiescence is a separate lifecycle concern.

## Update Rule

When a change alters a Firecracker-facing boundary, update the detailed owner
document and affected capability records first, then update this row if its
reader-facing summary changed. Do not add delivery chronology or duplicate
test-command blocks here.
