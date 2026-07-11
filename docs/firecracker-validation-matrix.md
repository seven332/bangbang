# Firecracker Validation Matrix

This matrix summarizes bangbang's current Firecracker-facing compatibility
coverage. Detailed endpoint behavior, field policy, platform limits, and
compatibility rationale remain in
[Firecracker Compatibility Scope](firecracker-compatibility.md).

## Status Vocabulary

- `implemented`: the public behavior exists for the documented subset.
- `partial`: an initial subset works, but important Firecracker behavior is
  still tracked by the related issue.
- `recognized unsupported`: the API shape is parsed or recognized before
  returning a Firecracker-shaped fault.
- `deferred`: the behavior needs a larger capability or backend design.
- `platform-limited`: the Firecracker feature depends on Linux-specific
  mechanisms or a host facility that does not map directly to macOS/HVF.

## Validation Layers

- `unit`: crate-local Rust tests for parsers, state, error formatting, and
  backend-neutral helpers.
- `api socket`: in-process API server tests over a real Unix socket.
- `process e2e`: unsigned executable tests in `crates/bangbang/tests/`.
- `signed process`: `scripts/run-signed-process-tests.sh`.
- `signed HVF`: `scripts/run-integration-tests.sh` targets that create HVF
  resources or boot guests.
- `docs`: compatibility, security, testing, or review documentation.

## Matrix

| Area | Current status | Primary validation | Related issue | Notes |
| --- | --- | --- | --- | --- |
| Process CLI and API socket | partial | unit, api socket, process e2e, signed process | #536, #545, #1008, #1010, #1048, #1058, #1060, #1070, #1092 | Firecracker-shaped startup args including `--boot-timer`, recognized unsupported snapshot inspection commands, exit codes for argument parsing, bad startup configuration, best-effort non-clobbering fd-table preallocation, the implemented fatal host signal subset, and non-terminating `SIGPIPE` handling, API socket binding, config-file startup, no-api mode, body-based HTTP payload limits including `413 Payload Too Large` responses plus separate request-head parser bounds, empty mutating request fault messages including unknown bodyless PUT/PATCH parity, cleanup, and selected process-local MMDS/startup metrics isolation are covered for the current subset. Linux jailer/seccomp behavior is platform-limited. |
| Instance/version/config reads | implemented | unit, api socket, process e2e | #536 | `GET /`, `GET /version`, `GET /vm/config`, and `GET /machine-config` expose accumulated supported state for the current subset. Unsupported config sections are omitted until modeled. |
| Machine and boot configuration | partial | unit, api socket, process e2e, signed HVF | #538 | Pre-boot machine config, empty/no-op CPU config, boot source, kernel/initrd loading, FDT generation including arm64 `/chosen/linux,pci-probe-only` and 64-byte `/chosen/rng-seed`, direct-rootfs boot paths, and current multi-vCPU startup rejection are covered. Non-empty custom CPU templates, multi-vCPU execution, and broader CPU feature behavior remain deferred. |
| Drives and virtio-block | partial | unit, api socket, process e2e, signed HVF | #539, #916, #962, #992, #994, #996, #998, #1020, #1068 | Initial drives, cache policy handling, optional bandwidth/ops rate limiters, Firecracker-shaped in-place replacement ordering for existing pre-boot drive IDs, runtime per-bucket limiter updates for existing active drives, backend-neutral block limiter retry timing, HVF block limiter retry wakeups, block read/write, root-drive boot args, basic guest-visible block behavior, signed executable writeback block flush coverage, runtime backing refresh for existing drives, recognized unsupported Firecracker-shaped vhost-user-block `socket` configs without storing or leaking socket paths, and aggregate plus per-drive block queue/read/write/flush/update/failure/read/write latency/throttling metrics are covered. Hotplug, removal, real vhost-user-block device support, full Firecracker timerfd/eventfd parity, and vhost-user-block metrics remain deferred. |
| Network and MMDS | partial | unit, api socket, process e2e, signed HVF | #540, #962, #982, #1066, #1090, #1146, #1148, #1150, #1154 | Initial network config, no-op shared rate-limiter shapes, runtime no-op PATCH for existing interfaces, vmnet mode selection, process-local MMDS, Firecracker-shaped MMDS store presence versus initialized data behavior across startup, internal guest-visible MMDS packet handling, aggregate plus per-interface `net` metrics for implemented virtio-net RX/TX queue activity and failures, top-level `mmds` metrics for implemented guest MMDS packet detour and response queue activity, and signed executable guest MMDS v1, API-enabled and no-api metadata-file startup MMDS v1, plus API-enabled and no-api metadata-file MMDS v2 token-flow fetches are covered. Broader public packet movement, configured rate limiting, rate-limiter metrics, and real runtime network mutation remain deferred. |
| Virtio-vsock | partial | unit, api socket, process e2e, signed HVF | #541, #984 | Initial config, startup listener ownership, connection setup, bounded buffering, reset/shutdown cleanup, minimal partial shutdown state including same-window receive shutdown ordering, Firecracker-shaped aggregate `vsock` metrics for implemented RX/TX queue, packet, payload-byte, connection cleanup, and classifiable queue/event failure activity, executable startup paths, signed executable guest-initiated and host-initiated EOF cleanup, signed executable guest-initiated and host-initiated multi-payload connection exchanges, and narrow signed executable guest-initiated and host-initiated multi-stream exchanges are covered. Full throughput-oriented streaming semantics, Firecracker's full graceful-shutdown timeout/kill-queue behavior, credit accounting, full muxer/connection metrics parity, and broader socket lifecycle parity remain deferred. |
| Observability: logger, metrics, serial | partial | unit, api socket, process e2e, signed process, signed HVF | #542, #918, #982, #984, #986, #988, #990, #992, #1008, #1010, #1024, #1056, #1074, #1088, #1090 | Minimal logger output with level, origin, and module-prefix filtering, Firecracker-shaped API request method/path lines without request bodies, Firecracker-shaped boot timer logger events, metrics, startup timing, immediate startup metrics flushing, explicit and 60-second periodic metrics flushes, selected GET, PUT, PATCH including parser failures for endpoints with Firecracker-shaped request metric fields, `/actions` API request counters, selected deprecated HTTP API usage counter, `latencies_us.pause_vm` and `latencies_us.resume_vm` metrics for successful runtime `PATCH /vm` transitions, Firecracker-shaped snapshot action latency metrics for recognized snapshot requests that reach snapshot-specific unsupported faults, minimal `logger.missed_metrics_count` and `logger.missed_log_count`, `signals.sigpipe` metrics for handled non-terminating `SIGPIPE`, aggregate and per-drive `block` metrics for implemented queue activity, read/write latency aggregates, backing update counters, failures, and block limiter throttling, aggregate and per-device `pmem` metrics for implemented virtio-pmem queue activity and failures, aggregate and per-interface `net` metrics for implemented packet counts, byte counts, queue activity, and failures, top-level `mmds` metrics for implemented guest MMDS packet detour and response queue activity, aggregate `vsock` metrics for implemented packet counts, byte counts, queue activity, connection cleanup counters, and classifiable failures, aggregate `entropy` metrics for implemented virtio-rng request, byte, host-randomness failure, event-failure, throttling, and limiter-event activity, aggregate `rtc` metrics for implemented PL031 invalid read/write and error counters, serial output paths, serial output token-bucket limiting, Firecracker-shaped `uart` metrics for implemented TX writes, missed writes, output errors, and rate-limiter drops, and initrd plus direct-rootfs serial output paths are covered. Full Firecracker counters beyond the currently implemented subset and full log routing remain deferred. |
| VM lifecycle and run-loop control | partial | unit, api socket, process e2e, signed HVF, docs | #537, #1158, #1160, #1162, #1164, #1166, #1168, #1170, #1172, #1174, #1176, #1178, #1180, #1182, #1184, #1186, #1188, #1190, #1192, #1194, #1196, #1198, #1200, #1202, #1204, #1206, #1208, #1210, #1212, #1214, #1216, #1218, #1220, #1222, #1224, #1226, #1228, #1230, #1232, #1234, #1236, #1238, #1240, #1242, #1244, #1246, #1248, #1250, #1252, #1255, #1258 | `InstanceStart`, Running transition, retained boot worker status, runtime `PATCH /vm` pause/resume for the current process-owned boot worker, guest PSCI `SYSTEM_OFF`/`SYSTEM_RESET` process exits, and non-success terminal process failures are covered. The current pause acknowledgement prevents another guest run-loop window, but paused commands, selected guest-memory/control-plane mutations, auxiliary retry state, and host buffering can still change outside an exclusive lease. #1160 adds a scoped supervisor admission barrier: earlier FIFO commands finish, later ordinary commands and resume reject during its scope, and shutdown invalidates it out of band. #1162 adds acknowledged block and entropy limiter retry quiescence inside that scope, including in-flight publication drain and deferred wakeup preservation. #1164 adds an internal runner command that captures immutable X0-X30, PC, and CPSR values on the owning thread with explicit conflict admission. #1170 adds a separate raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 command and shares one failure-atomic core-register admission domain with general-register capture. #1182 adds raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 capture in the same domain. #1184 adds raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 capture in that domain. #1186 adds raw ACTLR_EL1 and CPACR_EL1 capture there, with a macOS 15 ACTLR boundary. #1172 adds baseline Q0-Q31, FPCR, and FPSR capture through the same admission, preserves every 128-bit Q value, and proves boundary values in signed HVF. #1174 adds CPU-level IRQ/FIQ get/set and failure-atomic capture under generalized interrupt-operation admission, distinct from GIC state. #1176 adds raw TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture as a fourth command in the shared core-register admission domain. #1178 adds stopped-runner capture of Hypervisor.framework's stable, versioned opaque GIC device blob except CPU system registers, sharing generalized interrupt admission. #1180 adds a separate failure-atomic owner-thread command for all ten EL1 ICC CPU-interface registers exposed by the current SDK in that same interrupt domain. #1166 adds a separate owner-thread command for an immutable raw HVF virtual-timer mask/offset pair and serializes it with individual timer operations; #1168 extends the same value, capture order, and admission domain with raw control/CVAL access. #1188 adds raw CNTKCTL_EL1, CNTP_CTL_EL0, and CNTP_CVAL_EL0 capture under generalized timer admission, with macOS 15 and GIC-before-vCPU prerequisites. #1212 extends that capture with raw CNTP_TVAL_EL0 without treating the signed relative view as stable or simultaneous with CVAL. #1190 adds redacted five-key APIA/APIB/APDA/APDB/APGA capture from all ten SDK halves in the shared core-register domain. #1192 adds guest-visible MIDR/MPIDR and baseline PFR/DFR/ISAR/MMFR compatibility metadata in the same domain. #1194 adds observation-only raw MDCCINT_EL1/MDSCR_EL1 debug-control capture in the same domain without changing debug or trap behavior. #1196 adds observation-only raw CSSELR_EL1 cache-selection capture there without changing or interpreting cache state. #1198 adds DFR0-counted observation-only capture of every implemented raw DBGBVR/DBGBCR hardware-breakpoint pair in the same core-register domain without writes, enablement, trap changes, or guest execution. #1200 adds the corresponding DFR0-counted raw DBGWVR/DBGWCR hardware-watchpoint capture under the same admission and observation-only constraints. #1202 adds observation-only capture of Hypervisor.framework's debug-exception and debug-register-access trap-policy booleans in that domain without changing host policy or conflating it with guest EL1 debug state. #1204 adds a separate macOS 15.2+ ZFR0/SMFR0 SVE/SME identification-metadata capture there without changing the baseline identification command or enabling SVE/SME. #1206 adds a runtime-resolved macOS 15.2+ getter-only capture of mutable `PSTATE.SM`/`PSTATE.ZA` in the same domain without calling the setter or reading SME data. #1208 adds redacted getter-only capture of raw macOS 15.2+ SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 in that shared domain without writes or SME data reads. #1210 adds redacted getter-only capture of raw macOS 15.2+ SCXTNUM_EL0 and SCXTNUM_EL1 in the same domain without writes or guest execution. #1214 adds a runtime-resolved, configuration-wide maximum guest-usable SME SVL query before VM creation, outside VM/vCPU ownership and runner admission. #1216 adds a retained default-vCPU configuration query for raw CTR_EL0/CLIDR_EL1/DCZID_EL0 metadata under the same no-handle boundary. #1218 adds an independent retained default-vCPU query for the complete eight-entry data/unified and instruction CCSIDR arrays. #1220 adds a conditional macOS 15.2+ getter-only Z0-Z31 capture that preflights `PSTATE.SM`, uses maximum SVL only as an allocation width, and redacts all bytes. #1222 adds a separate conditional getter-only P0-P15 capture that derives each predicate width as maximum SVL divided by eight and redacts all bytes. #1224 adds a conditional getter-only ZA capture that requires `PSTATE.ZA` but not `PSTATE.SM`, checked-squares maximum SVL, and redacts bytes and dimensions. #1226 adds a separate conditional fixed 64-byte SME2 ZT0 capture under the same ZA-only preflight, without querying maximum SVL. #1228 adds ordered nontransactional restore of the complete typed general-register capture, with exact partial-write failure context. #1230 adds the paired restore for the complete typed SP_EL0/SP_EL1/ELR_EL1/SPSR_EL1 capture, with exact partial-write failure context. #1232 adds the paired restore for the complete typed AFSR0_EL1/AFSR1_EL1/ESR_EL1/FAR_EL1/PAR_EL1/VBAR_EL1 capture. #1234 adds the paired restore for the complete typed ACTLR_EL1/CPACR_EL1 capture. #1236 adds the paired restore for the complete typed TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture. #1238 adds the paired restore for the complete typed SCTLR_EL1/TTBR0_EL1/TTBR1_EL1/TCR_EL1/MAIR_EL1/AMAIR_EL1/CONTEXTIDR_EL1 capture. #1240 adds the paired restore for the complete typed Q0-Q31/FPCR/FPSR capture. #1242 adds the paired restore for the complete redacted APIA/APIB/APDA/APDB/APGA key state and forms a thirty-operation shared core-register admission domain. #1244 adds the paired restore for the complete redacted SCXTNUM_EL0/SCXTNUM_EL1 value and forms a thirty-one-operation shared core-register admission domain. #1246 adds the paired one-write restore for the complete CSSELR_EL1 selector and forms a thirty-two-operation shared core-register admission domain. #1248 adds paired IRQ-then-FIQ restore under generalized interrupt-operation admission without changing that core-register count. #1250 adds paired debug-exception-then-debug-register-access trap-policy restore and forms a thirty-three-operation shared core-register admission domain. #1252 adds paired MDCCINT-then-MDSCR debug-control restore and forms a thirty-four-operation shared core-register admission domain. #1255 adds independently loaded pre-first-run restore of the complete opaque GIC device blob under generalized interrupt admission. #1258 adds pre-first-run restore of nine mutable EL1 ICC registers plus derived-RPR validation in the same interrupt domain. The supervisor lease invokes none of these captures or restore operations and no configuration queries. Reboot-in-place, multi-vCPU pause coordination, full snapshot-ready quiescence across the remaining auxiliary/host owners, complete HVF state capture/restore, and fine-grained guest error exit-code parity remain deferred. |
| Snapshots and restore | recognized unsupported | unit, api socket, process e2e, signed HVF, docs | #543, #1048, #1072, #1086, #1158, #1160, #1162, #1164, #1166, #1168, #1170, #1172, #1174, #1176, #1178, #1180, #1182, #1184, #1186, #1188, #1190, #1192, #1194, #1196, #1198, #1200, #1202, #1204, #1206, #1208, #1210, #1212, #1214, #1216, #1218, #1220, #1222, #1224, #1226, #1228, #1230, #1232, #1234, #1236, #1238, #1240, #1242, #1244, #1246, #1248, #1250, #1252, #1254, #1255, #1258 | Snapshot create/load request shapes are parsed and normalized into complete debug-redacted API/runtime values; malformed bodies fail at the parser, while valid bodies carry every path/backend/flag/override through VMM state/action policy without opening files. The native-v1 gate admits only Full create, File load, no dirty/clock/override options, and the one-vCPU/read-only-root/default-serial/no-optional-device create profile. Snapshot create reaches the snapshot-specific unsupported fault only in the paused state; not-started and running create requests fail through state policy. Rejected paused modes/profiles skip the supervisor barrier; an admitted create crosses scoped admission, waits for acknowledged block and entropy limiter retry quiescence, releases those guards before the lease, and returns the same snapshot-specific fault and latency classification. Preboot load requires successful-action history plus current non-logger/metrics configuration to be pristine, detecting explicit-default/no-op actions and residual MMDS presence while allowing logger/metrics. Public faults, latency/deprecation metrics, lifecycle state, and no-file behavior remain unchanged. The HVF crate can capture X0-X30, PC, and CPSR through one runner-owner command and restore that complete typed value through a separate ordered, nontransactional owner-thread operation; raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values through a second capture command and restore that complete typed value through a separate ordered, nontransactional owner-thread operation; raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 values through another core-register capture and restore that complete typed value through a separate ordered, nontransactional owner-thread operation; raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 values through another core-register capture and restore that complete typed value through a separate ordered, nontransactional owner-thread operation; raw ACTLR_EL1 and CPACR_EL1 values through another core-register capture and restore that complete typed value through a separate ordered, nontransactional owner-thread operation; baseline Q0-Q31, FPCR, and FPSR values through a third capture and restore that complete typed value through a separate ordered, nontransactional owner-thread operation; five 128-bit pointer-authentication keys through a redacted core-register capture and a separate ordered, nontransactional owner-thread restore; guest-visible MIDR/MPIDR and baseline PFR/DFR/ISAR/MMFR compatibility metadata through another command; optional macOS 15.2+ ZFR0/SMFR0 SVE/SME identification metadata through a separate command; the configuration-wide maximum guest-usable SME SVL through a no-handle query outside VM/vCPU ownership and runner admission; raw default-vCPU CTR_EL0/CLIDR_EL1/DCZID_EL0 cache features through a retained no-handle configuration query; the complete raw data/unified and instruction CCSIDR arrays through an independent retained no-handle configuration query; mutable macOS 15.2+ `PSTATE.SM`/`PSTATE.ZA` through one runtime-resolved getter-only command; raw macOS 15.2+ SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 through a separate debug-redacted getter-only command; raw macOS 15.2+ SCXTNUM_EL0 and SCXTNUM_EL1 through another debug-redacted capture and a separate ordered, nontransactional owner-thread restore; conditional macOS 15.2+ maximum-width Z0-Z31 through a runtime-resolved, PSTATE-preflighted, debug-redacted getter-only command; conditional maximum-derived P0-P15 through a separate runtime-resolved, PSTATE-preflighted, debug-redacted getter-only command; conditional maximum-SVL-square ZA through a runtime-resolved, ZA-preflighted, debug-redacted getter-only command that does not require streaming mode; conditional fixed 64-byte SME2 ZT0 through a separate runtime-resolved, ZA-preflighted, debug-redacted getter-only command that neither requires streaming mode nor queries maximum SVL; raw MDCCINT_EL1 and MDSCR_EL1 debug controls through a distinct capture and paired ordered nontransactional owner-thread restore; raw CSSELR_EL1 cache selection through another core-register capture and a separate one-write nontransactional owner-thread restore; every DFR0-reported raw DBGBVR/DBGBCR hardware-breakpoint pair through a count-aware observation-only command; every DFR0-reported raw DBGWVR/DBGWCR hardware-watchpoint pair through a separate count-aware observation-only command; Hypervisor.framework's debug-exception and debug-register-access trap-policy booleans through a distinct capture and paired ordered nontransactional owner-thread restore; raw physical-timer CNTKCTL, control, CVAL, and TVAL values through another command; and raw virtual-timer mask, offset, control, and CVAL values through another command. CPU-level IRQ/FIQ pending values are captured through a separate interrupt command, and the complete typed value can be reapplied IRQ then FIQ through a separate nontransactional owner-thread restore. Raw TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 values are captured through a fourth core-register command and restored through a separate ordered, nontransactional owner-thread operation. Another stopped-runner command captures Hypervisor.framework's stable, versioned opaque GIC device blob except GIC CPU system registers and a paired never-run owner command reapplies that complete value; a separate owner-thread command captures all ten exposed EL1 ICC registers as one per-vCPU value and a paired pre-first-run command restores its nine mutable registers while validating derived RPR. Snapshot create invokes, persists, and restores none of these subsets; the isolated general-, core-system-, exception-register, execution-control, cache-selection, debug-control, debug-trap-policy, thread-context, translation, system-context, baseline SIMD/FP, and pointer-authentication restore operations plus the separate pending-interrupt, opaque-GIC, and EL1-ICC restore operations are exposed only as internal runner and boot-session plumbing. The baseline and optional SVE/SME identification values are read-only compatibility metadata rather than mutable restore state and have no mask or destination policy. The separate SME PSTATE value is mutable guest execution state; maximum SVL, Z0-Z31, P0-P15, ZA, and ZT0 are captured separately, but it has no setter, feature validation, transition ordering, persistence, or restore policy. The separate SME Z-register value redacts every byte and uses maximum SVL only as an allocation width; it has no effective-SVL interpretation, protected persistence, schema, or restore policy. The separate SME P-register value likewise redacts every byte and derives each predicate width as maximum SVL divided by eight; it has no effective-SVL or inactive-lane interpretation, protected persistence, schema, or restore policy. The separate SME ZA-register value redacts bytes and dimensions and uses the checked square of maximum SVL; it has no effective-SVL or layout interpretation, protected persistence, schema, or restore policy. The separate SME ZT0-register value redacts its fixed 64 bytes and has no SME2 feature/destination or lane policy, protected persistence, schema, or restore policy. The separate SME system-register value redacts its raw registers; maximum SVL is queried separately, but it has no feature/writable-bit validation, persistence, or safe restore ordering with PSTATE and conditional SME data. The separate system-context value redacts both software context numbers and its capture-order apply reports no values, but it has no interpretation, feature/destination validation, protected persistence, rollback, or safe wider restore ordering with TPIDR and CONTEXTIDR state. Breakpoint and watchpoint comparators are captured separately but have no control-bit, destination-count, persistence, or restore policy. The guest debug-control capture and apply and separately captured host debug-trap capture and apply remain separate and lack joint feature, writable-bit, destination-policy, persistence, and wider restore semantics. The cache-selection value is not topology, and its capture-order apply provides no selector or destination validation, ISB/dependent CCSIDR visibility, maintenance, protected persistence, rollback, schema, or portable restore policy. Default-vCPU CTR/CLIDR/DCZID features and CCSIDR geometry remain independent, non-atomic queries. The pending-interrupt apply is nontransactional and HVF clears both levels after every run, so it defines neither GIC/device composition, delivery/EOI, automatic reassertion, nor durable snapshot restore. The raw core system-register, exception, execution-control, cache-selection, breakpoint, watchpoint, debug-control, debug-trap policy, translation, thread-context, SME PSTATE, SME Z-register, SME P-register, SME ZA-register, SME ZT0-register, SME system-register, system-context, baseline SIMD/FP, pointer-authentication key, pending-interrupt, physical-timer, opaque GIC, and EL1 ICC values have no bangbang snapshot-restore validation or snapshot-schema meaning; the pointer-authentication value has no feature/destination validation, zeroization, protected persistence, or safe SCTLR enable ordering; the raw virtual-timer offset is host-time-relative, physical CVAL is an absolute comparator, physical TVAL is a changing signed relative view returned raw, their reads are sequential, control ISTATUS is time-sensitive, and none of these subsets has a complete snapshot restore policy yet. Snapshot load reaches the snapshot-specific unsupported fault only before startup. `--snapshot-version` and `--describe-snapshot <PATH>` remain recognized early commands without a supported data format. Remaining auxiliary/host quiescence, guest-memory persistence, complete HVF vCPU/VM state capture and restore, dirty tracking, snapshot format, data-format inspection, EL2 GIC CPU-interface and emulated-device state persistence, and real create/load remain deferred; see [Snapshot Feasibility](snapshot-feasibility.md). |
| Memory hotplug | partial config/status and startup shell foundation | unit, api socket, process e2e, signed HVF, docs | #544, #942, #952, #1022, #1026, #1028, #1030, #1032, #1034, #1040, #1042, #1044, #1046, #1050 | `PUT /hotplug/memory` stores validated Firecracker-shaped pre-boot block/slot/total config and exposes it through `GET /vm/config`; `GET /hotplug/memory` returns pre-start status with zero plugged size and, after startup, active runtime plugged size with the current requested size when configured; malformed bodies and unknown fields fail before mutation; lifecycle faults and supported API request metrics still cover routed requests plus parser failures; the backend-neutral virtio-mem identity, one-queue MMIO handler, feature bits, read-only 56-byte config-space layout, activation-time queue metadata checks, request descriptor parsing, response descriptor completion, used-ring publication, plugged-block state tracking, config-space plugged-size updates, failure-aware mutation execution/rollback boundaries, active status reads, and queue-interrupt plumbing through boot runtime/HVF are modeled; `InstanceStart` can attach a guest-visible virtio-mem MMIO/FDT shell with zero usable, plugged, and requested bytes; and runtime `PATCH /hotplug/memory` validates and commits requested-size updates while refreshing active virtio-mem config space and signaling a config interrupt. `STATE` requests return plugged, unplugged, or mixed responses from runtime state for valid usable ranges; accepted `PLUG`, `UNPLUG`, and `UNPLUG_ALL` requests use the active HVF dynamic guest-memory executor to map or unmap the backend-owned guest memory before ACK publication, and late publication failures roll back the applied backend mutation when possible; invalid ranges, duplicate mutations, unsupported request types, descriptor parse failures, mutation execution failures, response write failures, status query failures, used-ring failures, and rollback failures remain fail-closed. Signed executable e2e coverage proves a direct-rootfs guest binds `virtio_mem` and observes a runtime requested-size update through the public API path. Broader public guest-memory accounting and public hot-unplug semantics remain deferred to dedicated designs. |
| RTC | partial | unit, signed HVF, docs | #544, #944, #1052, #1074 | A minimal aarch64 PL031 RTC is registered as MMIO during HVF startup and emitted in the FDT using Firecracker's no-interrupt `arm,pl031` / `arm,primecell` shape. The backend-neutral handler supports 32-bit current-time, load, match, control, interrupt-mask, no-interrupt status, clear, and PrimeCell ID register accesses. Runtime metrics emit a non-empty Firecracker-shaped `rtc` object for implemented invalid read/write and error counters. Signed executable direct-rootfs coverage proves Linux exposes `/dev/rtc0` and PL031 RTC evidence. RTC alarm interrupts are intentionally unsupported for this PL031 shape because no interrupt line is exposed. |
| Time and identity devices | partial startup VMGenID and VMClock | unit, signed HVF, docs | #543, #544, #946, #1076, #1078, #1080, #1082, #1084 | PVTime/steal-time, VMGenID/SysGenID, VMClock, and their snapshot/restore interactions are classified separately from the implemented PL031 RTC. PVTime is platform-limited until bangbang has an HVF-specific steal-time capability design. The backend-neutral arm64 FDT builder validates and emits Firecracker-shaped VMGenID and VMClock nodes. Startup places an initial VMGenID buffer and a page-aligned 4 KiB VMClock backing page in the reserved arm64 system-memory area, writes a non-zero VMGenID from host randomness, initializes VMClock's minimal Firecracker ABI fields, allocates deterministic SPI lines through the HVF startup interrupt allocator, and exposes both nodes to guests. Signed HVF direct-rootfs coverage proves Linux observes the `/vmgenid` device-tree node with the `microsoft,vmgenid` compatible string and 16-byte `reg` property tuple. Signed executable direct-rootfs coverage proves Linux observes the startup `amazon,vmclock` `ptp@...` device-tree node with a 16-byte `reg` property tuple and 4 KiB region size through the public `bangbang` startup path. Restore-time generation changes, interrupt signaling after restore, persistence, and broader snapshot lifecycle behavior remain deferred. |
| Remaining Firecracker devices | partial | unit, api socket, process e2e, signed HVF, docs | #544, #797, #800, #802, #804, #806, #808, #810, #812, #814, #815, #818, #869, #873, #875, #877, #888, #890, #892, #894, #896, #898, #900, #902, #904, #905, #908, #910, #912, #914, #920, #922, #926, #928, #930, #932, #934, #936, #938, #940, #960, #962, #964, #968, #970, #972, #988, #990, #1000, #1002, #1016, #1018, #1024 | Balloon now has pre-boot config storage from API/config-file paths, target validation against guest memory, runtime target updates with config-generation and config-interrupt signaling, runtime nonzero statistics interval updates without toggling statistics enabled state, process-level periodic statistics scheduling while the VM is running, virtio-balloon identity/features/queues/config-space, startup MMIO/FDT attachment, inflate/deflate queue dispatch with mapped-PFN validation and inflated-page accounting, required `GET /balloon/statistics` target/actual fields, free-page hinting command/status handling, active-run hinting range recording, bounded statistics queue report parsing/storage/exposure as optional `GET /balloon/statistics` fields, backend-neutral completion of a pending statistics descriptor with queue-interrupt intent when runtime policy triggers a statistics update, rejection of `free_page_reporting: true` API/config-file requests without mutation or readiness publication, and minimal metrics for implemented queue activity and failures. Signed executable e2e coverage proves the public `/balloon` API path boots a direct-rootfs guest that binds `virtio_balloon` and exercises minimal hinting command-state APIs; full Firecracker balloon device counters, free-page reporting, and host reclaim remain deferred. Valid pre-boot pmem requests without configured rate limiters or requested root-device semantics, including empty and all-null limiter objects, store Firecracker-shaped configuration from API and config-file startup paths, appear in `GET /vm/config`, make `InstanceStart` open and validate non-zero regular host backing files while retaining handles, mmap those files to 2 MiB-aligned host ranges during startup preparation, assign deterministic non-overlapping 2 MiB-aligned guest physical ranges after the aarch64 MMIO64 gap while skipping current guest RAM, copy prepared mappings into HVF-compatible anonymous shadows, register those shadows with HVF after DRAM using read-only or read/write non-executable permissions, write writable shadows back to the backing file for guest queue-driven flush requests and after clean unmap while skipping writeback after failed unmap cleanup, attach one virtio-mmio/FDT node per prepared pmem device with a backend-neutral virtio-pmem identity, queue metadata, feature-bit, alignment constant, config-space `start`/`size` values, flush queue completion handling, aggregate plus per-device `pmem` metrics for implemented queue activity and failures, and accept runtime `PATCH /pmem/{id}` no-op rate-limiter updates for existing pmem devices when the limiter is omitted, `null`, empty, or all-null. Signed executable e2e coverage now proves the public `/pmem/{id}` API path can boot a direct-rootfs guest that reads the host marker, accepts the runtime no-op PATCH path, rejects configured runtime pmem limiters without echoing limiter values, rejects pre-boot root-device requests without mutating stored pmem configuration, and flushes a guest marker back to the pmem backing file; root-device semantics, real rate limiting and rate-limiter metrics, dirty-range tracking, direct file-backed HVF mapping, and hot-unplug remain deferred. Valid pre-boot entropy requests with missing, null, empty-object, all-null, or valid configured bandwidth/ops rate limiters store public configuration, appear in `GET /vm/config`, feed `InstanceStart` so the existing HVF virtio-rng MMIO/FDT device can use the session-owned host OS randomness source, have signed executable e2e coverage that proves a direct-rootfs guest reads non-empty data from `/dev/hwrng`, enforce configured entropy rate limiters in backend-neutral queue dispatch with pending-descriptor retry timing, HVF entropy retry wakeups, and no sleeps, and report Firecracker-shaped aggregate `entropy` metrics for implemented request, byte, host-randomness failure, event-failure, throttling, and limiter-event activity. Entropy remains outside current API request metrics output because Firecracker does not define entropy request counters. Full Firecracker timerfd/eventfd limiter wakeup parity and other remaining device implementations still need separate designs. |
| macOS isolation and platform limits | partial | docs, process e2e | #545, #924, #1102 | Security docs cover current socket, host-path, entitlement, vmnet host policy, multi-process boundaries, and a concise isolation compatibility checklist. macOS sandboxing, launcher/resource broker, production network isolation, and stricter host-path policy need follow-up work. |
| Validation matrix maintenance | implemented | docs | #546 | Future capability PRs should update this matrix when support status or validation layers change. Full upstream Firecracker test-suite mapping remains deferred. |

#1206 extends the lifecycle and snapshot rows with a sixteenth shared-core
capture: one runtime-resolved macOS 15.2+ getter observes mutable `PSTATE.SM`
and `PSTATE.ZA` without calling the setter. Unit coverage validates the C ABI,
all Boolean combinations, raw error propagation, fresh retry, bidirectional
admission, and cleanup; signed HVF coverage validates same-vCPU idle observation
or the exact documented unavailable result. Snapshot create invokes, persists,
and restores none of it. Maximum SVL, Z0-Z31, P0-P15, ZA, and ZT0 are captured separately;
feature and transition validation, schema, persistence, and
restore remain deferred.

#1208 extends the same rows with a seventeenth shared-core capture: raw macOS
15.2+ `SMCR_EL1`, `SMPRI_EL1`, and `TPIDR2_EL0` reads publish only after all
three succeed, and `Debug` redacts every value. Unit coverage validates exact
SDK ids, order, boundary values, every failure point and fresh retry,
bidirectional admission, abandonment, unwind, panic, and cleanup; signed HVF
coverage validates two idle same-vCPU captures without raw logging, writes,
maximum-SVL queries, SME data reads, or guest execution. Snapshot create
invokes, persists, and restores none of it; feature and writable-bit validation,
schema, persistence, and safe restore ordering remain deferred.

#1210 extends the same rows with an eighteenth shared-core capture: raw macOS
15.2+ `SCXTNUM_EL0` and `SCXTNUM_EL1` reads publish only after both succeed,
and `Debug` redacts both software context numbers. Unit coverage validates exact
SDK ids, EL0-then-EL1 order, boundary values, both failure points and fresh
retry, bidirectional admission, abandonment, unwind, panic, and cleanup; signed
HVF coverage validates two idle same-vCPU captures without raw logging, writes,
guest execution, reset assumptions, or compatibility inference. Snapshot create
invokes, persists, and restores none of it; interpretation, feature/destination
validation, schema, persistence, and safe restore ordering remain deferred.

#1214 extends the lifecycle and snapshot rows with a configuration-wide,
runtime-resolved macOS 15.2+ maximum guest-usable SME SVL query. The typed
`usize` value remains outside backend instance state, VM/vCPU ownership, runner
admission, boot sessions, and snapshot orchestration. Unit coverage validates
missing and present symbols, full-width `size_t` preservation, exact return and
operation behavior, the public accessor, and the non-target boundary. Signed
HVF coverage queries twice before VM creation without logging the value and
accepts only two successful equal observations or two exact `HV_UNSUPPORTED`
results. Snapshot create invokes, persists, and restores none of it; effective
SVL selection, feature/destination policy, ZT0 lane policy and ZA layout, schema, persistence,
and restore remain deferred.

#1216 extends the lifecycle and snapshot rows with raw macOS 11+
`CTR_EL0`/`CLIDR_EL1`/`DCZID_EL0` feature metadata from a fresh retained default
vCPU configuration. The query takes no backend instance, VM/vCPU handle, or
runner admission and does not change the configuration used for vCPU creation.
Unit coverage validates exact ids, arbitrary values, deterministic order, null
creation, every getter failure, operation errors, target behavior, accessors,
and success/error/unwind release. Signed HVF coverage compares two pre-VM
queries without logging raw values or performing selector, CCSIDR, maintenance,
or guest operations. Snapshot create invokes, persists, and restores none of
it; CCSIDR geometry is queried separately, while interpretation, masks,
destination policy, schema, persistence, and restore remain deferred.

#1218 extends the same rows with two complete eight-entry raw data/unified and
instruction CCSIDR arrays from an independent fresh retained default vCPU
configuration. The query also takes no backend instance, VM/vCPU handle, or
runner admission and does not change live vCPU creation. Unit coverage
validates exact cache types, all sixteen arbitrary values, deterministic order,
null creation, both getter failures, exact operation errors, target behavior,
accessors, and success/error/unwind release. Signed HVF coverage compares two
pre-VM queries without logging raw values or performing selector, live CCSIDR,
ISB, maintenance, or guest operations. Snapshot create invokes, persists, and
restores none of it; the feature and geometry queries are not atomic, and
interpretation, masks, destination policy, schema, persistence, and restore
remain deferred.

#1220 extends the lifecycle and snapshot rows with a nineteenth shared-core
command that conditionally captures all streaming Z0-Z31 bytes on macOS 15.2+.
It preflights `PSTATE.SM`, queries maximum SVL only as the exact allocation
width, fallibly allocates one contiguous buffer, and publishes no value until
all 32 runtime-resolved getter calls succeed; `Debug` redacts every byte. Unit
coverage validates ABI and ids, inactive/size/allocation boundaries, exact
order and bytes, every getter failure and retry, bounded accessors, thirty-four-way
admission, abandonment, channel, panic, and cleanup. Signed HVF coverage accepts
only documented unavailability/inactivity or two complete idle-vCPU captures
without logging raw bytes or width, changing SME state, or executing the guest.
Both session forms expose capture, but snapshot create invokes, persists, and
restores none of it; P0-P15, ZA, and ZT0 are captured separately, while effective SVL,
feature/destination policy,
protected persistence, schema, restore ordering, orchestration, and multi-vCPU
association remain deferred.

#1222 extends the same rows with a twentieth shared-core command that
conditionally captures all streaming P0-P15 predicate bytes on macOS 15.2+.
It preflights `PSTATE.SM`, queries maximum SVL, requires a non-zero value
divisible by eight, fallibly allocates one contiguous buffer, and publishes no
value until all 16 runtime-resolved getter calls succeed; `Debug` redacts every
byte. Unit coverage validates ABI and ids, inactive/size/divisibility/allocation
boundaries, exact order and bytes, every getter failure and retry, bounded
accessors, thirty-four-way admission, abandonment, channel, panic, and cleanup.
Signed HVF coverage accepts only documented unavailability/inactivity or two
complete idle-vCPU captures without logging raw bytes or widths, changing SME
state, or executing the guest. Both session forms expose capture, but snapshot
create invokes, persists, and restores none of it; Z0-Z31 and ZA are captured
separately alongside ZT0, while effective SVL, feature/destination policy, inactive-
lane interpretation, protected persistence, schema, restore ordering,
orchestration, and multi-vCPU association remain deferred.

#1224 extends the same rows with a twenty-first shared-core command that
conditionally captures the complete SME ZA matrix on macOS 15.2+. It preflights
`PSTATE.ZA` without requiring `PSTATE.SM`, queries a non-zero maximum SVL,
checked-squares it, fallibly allocates the exact buffer, and publishes no value
until the single runtime-resolved getter succeeds; `Debug` redacts bytes and
dimensions. Unit coverage validates the exact ABI, both streaming-mode values
under active/inactive ZA, zero/overflow/allocation boundaries, exact bytes,
backend failure and retry, raw accessors, thirty-four-way admission,
abandonment, channel, panic, and cleanup. Signed HVF coverage accepts only
documented unavailability/inactivity or two complete idle-vCPU captures without
logging bytes or dimensions, changing SME state, or executing the guest. Both
session forms expose capture, but snapshot create invokes, persists, and
restores none of it; Z/P/ZT0 are captured separately, while effective SVL,
feature/destination policy, layout interpretation, protected persistence,
schema, restore ordering, orchestration, and multi-vCPU association remain
deferred.

#1226 extends the same rows with a twenty-second shared-core command that
conditionally captures the fixed 64-byte SME2 ZT0 register on macOS 15.2+. It
preflights `PSTATE.ZA` without requiring `PSTATE.SM`, performs no maximum-SVL
query, and publishes no value until one runtime-resolved getter succeeds through
a private 16-byte-aligned SDK-compatible output object; `Debug` redacts every
byte. Unit coverage validates the exact SDK ABI, 64-byte size and 16-byte
alignment, both streaming-mode values under active/inactive ZA, exact bytes,
backend failure and retry, fixed-size access, thirty-four-way admission,
abandonment, channel, panic, and cleanup. Signed HVF coverage accepts only
documented unavailability/inactivity or two complete idle-vCPU captures without
logging bytes, changing SME state, querying maximum SVL, or executing the guest.
Both session forms expose capture, but snapshot create invokes, persists, and
restores none of it; Z/P/ZA are captured separately, while setters/transitions,
SME2 feature/destination policy, lane interpretation, protected persistence,
schema, restore ordering, orchestration, and multi-vCPU association remain
deferred.

#1228 extends the same rows with the first owner-thread restore operation for a
captured architectural subset. It borrows the complete typed X0-X30/PC/CPSR
value, clones it into the runner command, and writes all 33 registers in
architectural order. The shared core-register admission domain is generalized
from capture to operation and now covers twenty-three mutually exclusive
operations. Hypervisor.framework provides no batch transaction: a typed error
reports the failed register, completed-write count, and backend source, so the
caller retains the complete value for a full retry or must discard the vCPU
before execution. Unit coverage exercises every failure position and retry,
exact ordering, owner-thread dispatch, thirty-four-way conflicts,
abandonment, channels, queued destruction, unwind, panic, shutdown, and both
boot-session forms. Signed HVF coverage restores and recaptures one complete
same-vCPU idle value twice without guest execution or raw-value logging.
Rollback, schema/deserialization, input and destination validation, wider-state
ordering, snapshot orchestration, and public snapshot load remain deferred.

#1230 extends the same rows with the paired owner-thread restore operation for
the complete typed SP_EL0/SP_EL1/ELR_EL1/SPSR_EL1 capture. It writes the four
raw values in capture order through the existing runner owner and expands the
shared core-register admission domain to twenty-four mutually exclusive
operations. Hypervisor.framework still provides no transaction: a reusable
typed system-register error reports the exact failed register, completed-write
count, and backend source, while retaining the caller's complete value for a
full retry or vCPU discard before execution. Unit coverage exercises every
failure position and full retry, exact ordering, owner-thread dispatch,
thirty-four-way conflicts, abandonment, channels, queued destruction, unwind,
panic, shutdown, and both boot-session forms. Signed HVF coverage restores and
recaptures one complete same-vCPU idle value twice without guest execution or
raw-value logging. Rollback, schema/deserialization, input and destination
validation, wider-state ordering, snapshot orchestration, and public snapshot
load remain deferred.

#1232 extends the same rows with the paired owner-thread restore operation for
the complete typed AFSR0_EL1/AFSR1_EL1/ESR_EL1/FAR_EL1/PAR_EL1/VBAR_EL1
capture. It writes the six raw values in capture order through the existing
runner owner and expands the shared core-register admission domain to
twenty-five mutually exclusive operations. The reusable typed system-register
error reports the exact failed register, completed-write count, and backend
source while retaining the caller's complete value for a full retry or vCPU
discard before execution. Unit coverage exercises every failure position and
full retry, exact ordering, owner-thread dispatch, thirty-four-way conflicts,
abandonment, channels, queued destruction, unwind, panic, shutdown, and both
boot-session forms. Signed HVF coverage restores the actual same-vCPU
guest-written capture twice, preserves implementation-defined AFSR readback,
and performs no post-restore guest execution or raw-value logging. Vector-table
memory, coherent exception validation, destination policy, rollback, schema,
wider ordering, snapshot orchestration, and public snapshot load remain
deferred.

#1234 extends the same rows with the paired owner-thread restore operation for
the complete typed ACTLR_EL1/CPACR_EL1 capture. It writes both raw values in
capture order through the existing runner owner and expands the shared core-
register admission domain to twenty-six mutually exclusive operations. The
reusable typed system-register error reports the exact failed register,
completed-write count, and backend source while retaining the complete value
for a full retry or vCPU discard before execution. Unit coverage exercises both
failure positions and full retry, exact ordering, owner-thread dispatch,
thirty-four-way conflicts, abandonment, channels, queued destruction, unwind,
panic, shutdown, and both boot-session forms. Signed HVF coverage restores the
same-vCPU guest-written EnTSO/FPEN capture twice without post-restore guest
execution or raw-value logging. The macOS 15 ACTLR boundary, optional CPACR
feature and destination validation, writable-bit policy, guest ISB transitions,
wider feature-state ordering, rollback, schema, snapshot orchestration, and
public snapshot load remain deferred.

#1236 extends the same rows with the paired owner-thread restore operation for
the complete typed TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture. It writes all three
raw values in capture order through the existing runner owner and expands the
shared core-register admission domain to twenty-seven mutually exclusive
operations. The reusable typed system-register error reports the exact failed
register, completed-write count, and backend source while retaining the
complete value for a full retry or vCPU discard before execution. Unit coverage
exercises all three failure positions and full retry, exact ordering,
owner-thread dispatch, thirty-four-way conflicts, abandonment, channels,
queued destruction, unwind, panic, shutdown, and both boot-session forms.
Signed HVF coverage restores the same-vCPU guest-written capture twice without
post-restore guest execution or raw-value logging. Pointer/address validation,
TPIDR2/SCXTNUM/CONTEXTIDR coordination, rollback, schema, wider context ordering,
snapshot orchestration, and public snapshot load remain deferred.

#1238 extends the same rows with the paired owner-thread restore operation for
the complete typed SCTLR_EL1/TTBR0_EL1/TTBR1_EL1/TCR_EL1/MAIR_EL1/AMAIR_EL1/
CONTEXTIDR_EL1 capture. It writes all seven raw values in capture order through
the existing runner owner and expands the shared core-register admission domain
to twenty-eight mutually exclusive operations. The reusable typed system-
register error reports the exact failed register, completed-write count, and
backend source while retaining the complete value for a full retry or vCPU
discard before execution. Unit coverage exercises all seven failure positions
and full retry, exact ordering, owner-thread dispatch, thirty-four-way
conflicts, abandonment, channels, queued destruction, unwind, panic, shutdown,
and both boot-session forms. Signed HVF coverage leaves the MMU disabled and
restores the actual same-vCPU guest-written capture twice, including
implementation-defined AMAIR readback, without post-restore guest execution or
raw-value logging. Translation-table memory, feature and destination validation,
barriers, TLB/cache maintenance, safe MMU transition ordering, rollback,
schema, wider state ordering, snapshot orchestration, and public snapshot load
remain deferred.

#1240 extends the same rows with the paired owner-thread restore operation for
the complete typed Q0-Q31/FPCR/FPSR capture. It adds one macOS arm64 C shim so
Clang can invoke the SDK's by-value SIMD vector setter while stable Rust passes
an ordinary 16-byte pointer, then writes all 34 fields in capture order through
the existing runner owner. The shared core-register admission domain expands to
twenty-nine mutually exclusive operations. A dedicated typed error distinguishes
the SIMD/FP and scalar register spaces and reports the completed-write prefix
and backend source while retaining the complete value for a full retry or vCPU
discard before execution. Unit coverage exercises all 34 failure positions and
full retry, exact ordering, owner-thread dispatch, thirty-four-way conflicts,
abandonment, channels, queued destruction, unwind, panic, shutdown, and both
boot-session forms. Signed HVF coverage restores the actual same-vCPU
non-streaming guest-written capture twice without post-restore guest execution
or raw-value logging. SVE/SME Q/Z alias ordering, feature and destination
validation, FPCR/FPSR writable-bit policy, protected persistence/zeroization,
rollback, schema, wider state ordering, snapshot orchestration, and public
snapshot load remain deferred.

#1242 extends the same rows with the paired owner-thread restore operation for
the complete redacted APIA/APIB/APDA/APDB/APGA pointer-authentication key
capture. It splits each `u128` into its low/high halves, writes all ten system
registers in capture order through the existing runner owner, and expands the
shared core-register admission domain to thirty mutually exclusive operations.
The reusable value-free system-register error reports the exact failed
register, completed-write count, and backend source while retaining the
caller's complete value for a full retry or vCPU discard before execution. Unit
coverage exercises all ten failure positions and full retry, exact pairing and
ordering, owner-thread dispatch, thirty-way conflicts, abandonment, channels,
queued destruction, unwind, panic, shutdown, redacted `Debug`, and both boot-
session forms. Signed HVF coverage restores and recaptures the visibly fake
same-vCPU guest-written keys twice without PAC execution, post-restore guest
execution, or raw-value logging. Feature/algorithm and destination validation,
zeroization, protected persistence, safe SCTLR enable ordering, rollback,
schema, wider state ordering, snapshot orchestration, and public snapshot load
remain deferred.

#1244 extends the same rows with the paired owner-thread restore operation for
the complete redacted SCXTNUM_EL0/SCXTNUM_EL1 system-context capture. It writes
both system registers in capture order through the existing runner owner and
expands the shared core-register admission domain to thirty-one mutually
exclusive operations. The reusable value-free system-register error reports
the exact failed register, completed-write count, and backend source while
retaining the caller's complete value for a full retry or vCPU discard before
execution. Unit coverage exercises both failure positions and full retry,
exact ordering, owner-thread dispatch, thirty-one-way conflicts, abandonment,
channels, queued destruction, unwind, panic, shutdown, redacted `Debug`, and
both boot-session forms. Signed HVF coverage restores and recaptures the first
same-vCPU idle capture twice without guest execution, reset-value assumptions,
compatibility inference, or raw-value logging. Interpretation, feature and
destination validation, protected persistence, TPIDR/CONTEXTIDR coordination,
rollback, schema, wider state ordering, snapshot orchestration, and public
snapshot load remain deferred.

#1246 extends the same rows with the paired owner-thread restore operation for
the complete typed CSSELR_EL1 cache-selection capture. It writes the selector
once through the existing runner owner and expands the shared core-register
admission domain to thirty-two mutually exclusive operations. The reusable
value-free system-register error reports the exact failed register, zero
completed writes, and backend source while retaining the caller's complete
value for a full retry or vCPU discard before execution. Unit coverage
exercises the one failure and full retry, exact owner-thread dispatch,
thirty-four-way conflicts, abandonment, channels, queued destruction, unwind,
panic, shutdown, and both boot-session forms. Signed HVF coverage restores and
recaptures the first same-vCPU idle selector twice without logging it, querying
CCSIDR, issuing ISB, performing cache maintenance, running the guest, or making
reset/topology/destination assumptions. Selector interpretation/validation, an
atomic cache feature/geometry manifest, ISB/dependent CCSIDR visibility,
maintenance, protected persistence, rollback, schema, snapshot orchestration,
and public snapshot load remain deferred.

#1248 extends the same rows with a paired owner-thread restore operation for
the complete typed CPU-level IRQ/FIQ pending capture. It writes IRQ then FIQ
under one command-owned generalized interrupt-operation admission guard without
changing the thirty-two-operation core-register count. A dedicated value-free
error reports the exact failed interrupt type, completed-write count, and
backend source while retaining the caller's complete value for a full retry or
vCPU discard before execution. Unit coverage exercises both failure positions
and full retry, exact ordering and values, every forward/reverse conflict,
abandonment, channels, queued destruction, unwind, panic, shutdown, and both
boot-session forms. Signed HVF coverage restores and recaptures a known
IRQ-only same-vCPU value twice after a FIQ-only mutation, then explicitly clears
both levels without a guest run. HVF clear-after-run behavior, GIC/device
composition, routing, delivery/EOI, automatic pre-run reassertion, persistence,
rollback, schema, multi-vCPU coordination, snapshot orchestration, and public
snapshot load remain deferred.

#1250 extends the same rows with paired owner-thread restore for the complete
typed Hypervisor.framework debug-exception/debug-register-access trap-policy
capture. It writes exception policy then register-access policy under one
command-owned core-operation guard and expands that domain to thirty-three
mutually exclusive operations. A dedicated value-free error reports the exact
failed host-policy operation, completed-write count, and backend source while
retaining the caller's complete value for full retry or vCPU discard before
execution. Unit coverage exercises both write failures and retry, exact Boolean
propagation and order, every forward/reverse conflict, abandonment, channels,
queued destruction, unwind, panic, shutdown, and both boot-session forms.
Signed HVF coverage restores and recaptures the original idle same-vCPU pair
twice without assuming or logging either Boolean, manufacturing a policy
change, altering guest debug controls/comparators, running guest instructions,
or executing the vCPU. Joint debug feature/security and destination policy,
wider guest/host debug ordering, persistence, rollback, schema, multi-vCPU
coordination, snapshot orchestration, and public snapshot load remain deferred.

#1252 extends the same rows with paired owner-thread restore for the complete
typed raw MDCCINT_EL1/MDSCR_EL1 debug-control capture. It writes MDCCINT then
MDSCR under one command-owned core-operation guard and expands that domain to
thirty-four mutually exclusive operations. The reusable value-free system-
register error reports the exact failed register, completed-write count, and
backend source while retaining the caller's complete value for full retry or
vCPU discard before execution. Unit coverage exercises both write failures and
retry, exact values and order, every forward/reverse conflict, abandonment,
channels, queued destruction, unwind, panic, shutdown, and both boot-session
forms. Signed HVF coverage restores and recaptures the original idle same-vCPU
pair twice without assuming or logging either register, manufacturing a control
change, altering comparator or host trap state, activating debug behavior, or
executing the vCPU. Feature/writable/status-bit and destination validation,
comparator and host-trap coordination, protected persistence, rollback, schema,
multi-vCPU coordination, snapshot orchestration, and public snapshot load
remain deferred.

#1255 extends the lifecycle and snapshot rows with paired pre-first-run restore
of #1178's complete opaque Hypervisor.framework GIC device blob. A setter-only
dynamic capability remains independent from capture, forwards the exact
non-empty pointer and `usize`/`size_t`, and preserves the original HVF result
without exposing bytes. The owner command clones the redacted value, shares
generalized interrupt admission, and atomically rejects any runner whose sticky
run lifetime has started. Unit coverage exercises empty/no-call, exact pointer
and size, backend provenance, completed and failed-run rejection, every
forward/reverse conflict, abandonment, channels, queued destruction, panic,
shutdown, and both boot-session forms. Signed HVF coverage captures and
reapplies the original same-VM blob before any run, then destroys the VM without
parsing, comparing, mutating, or logging bytes or executing the guest. EL1 ICC
restore remains separate, while host-update preflight, transactional recovery,
protected persistence, the cross-step no-run lease, schema, multi-vCPU
coordination, snapshot orchestration, and public snapshot load remain deferred.

#1258 extends the lifecycle and snapshot rows with paired pre-first-run restore
of #1180's complete ten-register EL1 ICC value. Independent getter and setter
capabilities load before mutation; nine architecturally mutable registers are
written in capture order, while derived read-only RPR is read and validated at
its original position. The typed value-free failure distinguishes write from
derived validation and reports the exact register, completed write prefix, and
backend source. Unit coverage exercises the ten-position sequence, every mutable
write failure, RPR read failure and mismatch, complete retry, the sticky never-
run gate, every interrupt-operation conflict, abandonment, channels, queued
destruction, unwind, panic, shutdown, and both boot-session delegates. Signed
HVF coverage applies the same-VM opaque blob first, restores the idle ICC value,
and proves two exact recaptures without guest execution or value logging.
Destination validation, host-update preflight, transactional recovery, protected
persistence, cross-step no-run leasing, composite orchestration, schema,
multi-vCPU coordination, and public snapshot load remain deferred.

## Update Rule

When a PR changes Firecracker-facing behavior, update this matrix if it changes
support status, adds or removes a validation layer, or moves work between
implemented, partial, deferred, recognized unsupported, or platform-limited
states.
