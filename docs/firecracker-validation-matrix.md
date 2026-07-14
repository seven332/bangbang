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
| Process CLI and API socket | implemented supported subset; Linux hardening platform-limited | unit, api socket, process e2e, signed process, signed App Sandbox process | #536, #545, #1008, #1010, #1048, #1058, #1060, #1070, #1092, #1260, #1302 | Firecracker-shaped startup args including `--boot-timer`, native snapshot envelope version and inspection commands, exit codes for argument parsing, bad startup configuration, best-effort non-clobbering fd-table preallocation, the implemented fatal host signal subset, and non-terminating `SIGPIPE` handling, API socket binding, config-file startup, no-api mode, body-based HTTP payload limits including `413 Payload Too Large` responses plus separate request-head parser bounds, empty mutating request fault messages including unknown bodyless PUT/PATCH parity, cleanup, and selected process-local MMDS/observability isolation are covered. The App Sandbox gate proves a container API socket plus denied default/outside paths. Linux jailer/seccomp behavior remains platform-limited. |
| Instance/version/config reads | implemented | unit, api socket, process e2e | #536 | `GET /`, `GET /version`, `GET /vm/config`, and `GET /machine-config` expose accumulated supported state for the current subset. Unsupported config sections are omitted until modeled. |
| Machine and boot configuration | partial | unit, api socket, process e2e, signed HVF, signed executable | #538, #1284, #1285, #1293, #1298 | Pre-boot machine config, empty/no-op CPU config, value-redacted classification of non-empty KVM capability, KVM vCPU-init feature, arm register modifier, mixed, and deprecated static CPU-template requests, boot source, kernel/initrd loading, FDT generation including arm64 `/chosen/linux,pci-probe-only` and 64-byte `/chosen/rng-seed`, direct-rootfs boot paths, ordered owner-thread vCPU topology, owning concurrent boot-session coordination with active-only batch cancellation, all-MPIDR FDT input, indexed PSCI CPU_ON/CPU_OFF/CPU_SUSPEND/level-0 affinity transitions, per-vCPU timer PPIs, internal signed Linux CPU1 execution, and public host-limited SMP startup are covered. Public `InstanceStart` admits `1..=min(32, host_max)` and keeps capacity/construction failures before session retention or `Running`. Signed executable proof configures two vCPUs through the API, observes independent pinned CPU0/CPU1 progress, offlines CPU1 through guest sysfs, brings the same owner back online, and observes resumed CPU1 progress without fixed sleeps. Signed HVF bare-guest proof retains CPU1 across two virtual-timer suspend cycles while CPU0 observes ON affinity. Custom/static CPU templates remain intentionally unapplied on arm64 HVF; any writable subset needs a separate Apple feature-view and snapshot contract. Multi-vCPU native-v1 snapshots, FDT idle-state discovery, non-timer suspend wake, dynamic CPU topology, and cross-host portability remain deferred. |
| Drives and virtio-block | implemented supported MMIO subset; optional PCI and vhost-user deferred | unit, api socket, process e2e, signed HVF, signed executable | #539, #916, #962, #992, #994, #996, #998, #1020, #1068, #1268, #1304 | Initial drives, Firecracker-shaped in-place replacement ordering, root/data attachment, backing validation and redaction, guest GET_ID/read/write, runtime backing refresh, aggregate and per-drive metrics, and the accepted one-read-only-root native-v1 profile are covered. `Unsafe` suppresses the flush feature; `Writeback` advertises it, and signed executable guest fsync validates the backing flush path. Optional bandwidth/ops limiters support per-bucket runtime updates, pending-descriptor retry timing, and session-owned HVF wakeups without claiming Linux timerfd/eventfd implementation identity. Firecracker v1.16.0's developer-preview runtime PUT/DELETE requires `--enable-pci`, PCI transport, guest rescan after attach, and guest removal before DELETE; bangbang uses MMIO and keeps `--enable-pci` plus runtime PUT/DELETE as tested nonmutating rejections. Linux io_uring `Async`, external vhost-user-block execution, broader optional-device snapshots, and vhost-user-block metrics remain unsupported or deferred. |
| Network and MMDS | implemented supported virtio-MMIO/MMDS-only subset; direct vmnet conditional; optional PCI deferred | unit, api socket, process e2e, signed HVF, signed executable, docs | #540, #962, #982, #1066, #1090, #1146, #1148, #1150, #1154, #1306, #1307, #1308, #1309, #1310, #1311, #1312, #1313 | Initial network config, guest-advertised MTU with signed Linux proof, independent RX/TX bandwidth and ops rate limiting, transactional runtime bucket replacement/clear updates for running and paused interfaces, backend-neutral retry timing for pending limiter work across queue/device/runtime/HVF dispatch results, and per-session HVF retry scheduling with earliest-deadline replacement, owner-thread dispatch, terminal cancellation, and signed RX-limited MMDS progress without a second guest queue notification are implemented. Process-local MMDS, Firecracker-shaped MMDS store presence versus initialized data behavior across startup, internal guest-visible MMDS packet handling, aggregate plus per-interface `net` metrics for implemented virtio-net RX/TX queue activity and failures, top-level `mmds` metrics for implemented guest MMDS packet detour and response queue activity, and signed executable guest MMDS v1, API-enabled and no-api metadata-file startup MMDS v1, plus API-enabled and no-api metadata-file startup MMDS v2 token-flow fetches are covered. A signed one-VM/two-interface MMDS-only case selects both Linux devices by configured MAC, binds each request to its matching interface, records distinct fixed guest markers, and reports activity under both API interface metric keys without direct vmnet resources; focused tests keep split-request buffers, response queues, interrupt lines, limiter state, and metrics associated with the owning interface. A signed two-process MMDS-only case gives each guest distinct V2 data, token authority, API/interface/file resources, and metrics keys, pauses one guest behind a process-local release gate, terminates its peer, then requires the survivor to re-fetch its retained value and complete through the same token-authenticated packet path. File-byte and key inclusion/exclusion assertions prove peer metrics flush/teardown cannot cross-write, while focused tests directly reject tokens across independent MMDS states and existing per-session queue/scheduler tests retain internal ownership evidence. The concurrent failure path redacts metadata, tokens, guest bytes, private paths, and raw worker diagnostics and does not require direct vmnet entitlement. Post-start PUT and DELETE remain stable nonmutating rejections on MMIO; Firecracker v1.16.0's Developer Preview attach/remove path requires optional PCI plus guest rescan/removal. The direct-vmnet foundation has mode selection plus injected lifecycle, read/write, and cleanup tests, but it does not consume Apple's returned MAC/MTU/maximum-packet values, register packet-available callbacks, enforce Apple's per-guest resource policy, provide host firewalling, or carry a real entitled external-connectivity proof. Broader MMDS TCP behavior, limiter-specific metrics, network snapshots, and PCI hotplug remain deferred. |
| Virtio-vsock | implemented supported live virtio-MMIO/Unix-socket subset | unit, api socket, process e2e, signed HVF, signed executable, docs | #541, #984, #1322, #1323, #1324 | Repeatable pre-boot `PUT /vsock` and stable post-start rejection, guest-visible MMIO/FDT attachment, process-local Unix-listener ownership/inode-safe cleanup, 256 retained connections per initiation direction, bounded handshakes and RW queues, dynamic 64-KiB credit windows with wrapping counters, partial/full shutdown, two-second request/shutdown cleanup, reset/error handling, `EVENT_IDX`, no-op event notifications, and Firecracker-shaped aggregate metrics for the implemented queue/packet/byte/cleanup/failure surface are covered. Signed executable cases incrementally verify ≥1 MiB in each direction for guest- and host-initiated streams, both peers' write-half-close/EOF sequence, terminal cleanup, path/payload-redacted diagnostics, and independent two-stream exchanges. Indirect descriptors are a supported bangbang extension. PATCH, DELETE, runtime hotplug, broader CID routing, general performance/artifact parity, PCI/vhost/KVM, broader muxer metrics, and full event payloads remain outside the live subset. Native-v1 snapshot UDS override, event-queue `TRANSPORT_RESET`, and post-restore RX gating are the stable #543 exclusions; the live subset is not classified as snapshot-compatible. |
| Observability: logger, metrics, serial | implemented supported process-local subset; production telemetry and global durability profile-limited | unit, api socket, process e2e, signed process, signed HVF, signed executable, docs | #542, #918, #982, #984, #986, #988, #990, #992, #1008, #1010, #1024, #1056, #1074, #1088, #1090, #1276, #1340, #1341, #1342, #1343 | Logger configuration/filtering, unrestricted request/action records without bodies, the bounded ten-per-five-second boot-timer callsite with recovery warning, best-effort delivery, and missed/rate-limited counters are covered. Metrics use successful-write interval deltas for every implemented API/logger/signal/UART/device count, byte, failure, error, limiter field, and block `sum_us`; startup timing, boot status, latest action latency, and block min/max/sample count are stores. Lower/new generations, keyed disappearance/reappearance, sparse absent families, ambiguous at-least-once replay, and bangbang's `metrics_flush_count: 1` extension are tested. Configuration is silent; one retained-session initial attempt, 60-second Running/Paused attempts, fallible explicit action, and one best-effort normal-terminal attempt have focused/process proof, while existing API/config-file signed scenarios now observe the additional post-exit line. Nullable nonblocking serial files/FIFOs, bounded internal default capture, TX token-bucket drops, UART deltas, redaction, cleanup, representative device producers, guest output, signals, and multi-process isolation are covered. There is no public serial RX/stdin/default stdout, fake zero-filled absent-device schema, process-global panic/fatal writer, or rotation/syslog/journald/tracing/remote telemetry. Bangbang-native v1 captures default serial MMIO/register state but restores a fresh output pipeline and excludes public path, buffered/in-flight bytes, limiter state, and counters. |
| VM lifecycle and run-loop control | partial | unit, api socket, process e2e, signed HVF, signed executable, docs | #537, #1293, #1298, #1284, #1158, #1160, #1162, #1164, #1166, #1168, #1170, #1172, #1174, #1176, #1178, #1180, #1182, #1184, #1186, #1188, #1190, #1192, #1194, #1196, #1198, #1200, #1202, #1204, #1206, #1208, #1210, #1212, #1214, #1216, #1218, #1220, #1222, #1224, #1226, #1228, #1230, #1232, #1234, #1236, #1238, #1240, #1242, #1244, #1246, #1248, #1250, #1252, #1255, #1258, #1261, #1276 | Host-limited public multi-vCPU `InstanceStart`, Running transition, retained boot worker status, runtime `PATCH /vm` pause/resume for the current process-owned boot worker, native-v1 load commit as `Paused` followed by optional ordinary resume, guest PSCI `SYSTEM_OFF`/`SYSTEM_RESET` process exits, PSCI `CPU_OFF` with same-owner `CPU_ON` re-entry, and non-success terminal process failures are covered. #1293 adds exact non-returning CPU_OFF token consumption, last-online denial, scheduler-before-power commit ordering, narrow `SCTLR_EL1` warm-entry reset, and signed Linux sysfs CPU1 offline/online proof through both internal and public startup paths. #1298 adds exact retained CPU_SUSPEND transactions, timer-PPI-before-success ordering, online affinity preservation, lifecycle cancellation/rearm, and signed two-cycle CPU1 context-retention proof. The current pause acknowledgement drains every active online-vCPU run before preventing another guest run-loop window; signed dual-process coverage proves independent CPU0/CPU1 progress stops and resumes while an isolated peer continues, but paused commands, selected guest-memory/control-plane mutations, auxiliary retry state, and host buffering can still change outside an exclusive lease. #1160 adds a scoped supervisor admission barrier: earlier FIFO commands finish, later ordinary commands and resume reject during its scope, and shutdown invalidates it out of band. #1162 adds acknowledged block and entropy limiter retry quiescence inside that scope, including in-flight publication drain and deferred wakeup preservation. #1164 adds an internal runner command that captures immutable X0-X30, PC, and CPSR values on the owning thread with explicit conflict admission. #1170 adds a separate raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 command and shares one failure-atomic core-register admission domain with general-register capture. #1182 adds raw SCTLR_EL1, TTBR0_EL1, TTBR1_EL1, TCR_EL1, MAIR_EL1, AMAIR_EL1, and CONTEXTIDR_EL1 capture in the same domain. #1184 adds raw AFSR0_EL1, AFSR1_EL1, ESR_EL1, FAR_EL1, PAR_EL1, and VBAR_EL1 capture in that domain. #1186 adds raw ACTLR_EL1 and CPACR_EL1 capture there, with a macOS 15 ACTLR boundary. #1172 adds baseline Q0-Q31, FPCR, and FPSR capture through the same admission, preserves every 128-bit Q value, and proves boundary values in signed HVF. #1174 adds CPU-level IRQ/FIQ get/set and failure-atomic capture under generalized interrupt-operation admission, distinct from GIC state. #1176 adds raw TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture as a fourth command in the shared core-register admission domain. #1178 adds stopped-runner capture of Hypervisor.framework's stable, versioned opaque GIC device blob except CPU system registers, sharing generalized interrupt admission. #1180 adds a separate failure-atomic owner-thread command for all ten EL1 ICC CPU-interface registers exposed by the current SDK in that same interrupt domain. #1166 adds a separate owner-thread command for an immutable raw HVF virtual-timer mask/offset pair and serializes it with individual timer operations; #1168 extends the same value, capture order, and admission domain with raw control/CVAL access. #1188 adds raw CNTKCTL_EL1, CNTP_CTL_EL0, and CNTP_CVAL_EL0 capture under generalized timer admission, with macOS 15 and GIC-before-vCPU prerequisites. #1212 extends that capture with raw CNTP_TVAL_EL0 without treating the signed relative view as stable or simultaneous with CVAL. #1190 adds redacted five-key APIA/APIB/APDA/APDB/APGA capture from all ten SDK halves in the shared core-register domain. #1192 adds guest-visible MIDR/MPIDR and baseline PFR/DFR/ISAR/MMFR compatibility metadata in the same domain. #1194 adds observation-only raw MDCCINT_EL1/MDSCR_EL1 debug-control capture in the same domain without changing debug or trap behavior. #1196 adds observation-only raw CSSELR_EL1 cache-selection capture there without changing or interpreting cache state. #1198 adds DFR0-counted observation-only capture of every implemented raw DBGBVR/DBGBCR hardware-breakpoint pair in the same core-register domain without writes, enablement, trap changes, or guest execution. #1200 adds the corresponding DFR0-counted raw DBGWVR/DBGWCR hardware-watchpoint capture under the same admission and observation-only constraints. #1202 adds observation-only capture of Hypervisor.framework's debug-exception and debug-register-access trap-policy booleans in that domain without changing host policy or conflating it with guest EL1 debug state. #1204 adds a separate macOS 15.2+ ZFR0/SMFR0 SVE/SME identification-metadata capture there without changing the baseline identification command or enabling SVE/SME. #1206 adds a runtime-resolved macOS 15.2+ getter-only capture of mutable `PSTATE.SM`/`PSTATE.ZA` in the same domain without calling the setter or reading SME data. #1208 adds redacted getter-only capture of raw macOS 15.2+ SMCR_EL1, SMPRI_EL1, and TPIDR2_EL0 in that shared domain without writes or SME data reads. #1210 adds redacted getter-only capture of raw macOS 15.2+ SCXTNUM_EL0 and SCXTNUM_EL1 in the same domain without writes or guest execution. #1214 adds a runtime-resolved, configuration-wide maximum guest-usable SME SVL query before VM creation, outside VM/vCPU ownership and runner admission. #1216 adds a retained default-vCPU configuration query for raw CTR_EL0/CLIDR_EL1/DCZID_EL0 metadata under the same no-handle boundary. #1218 adds an independent retained default-vCPU query for the complete eight-entry data/unified and instruction CCSIDR arrays. #1220 adds a conditional macOS 15.2+ getter-only Z0-Z31 capture that preflights `PSTATE.SM`, uses maximum SVL only as an allocation width, and redacts all bytes. #1222 adds a separate conditional getter-only P0-P15 capture that derives each predicate width as maximum SVL divided by eight and redacts all bytes. #1224 adds a conditional getter-only ZA capture that requires `PSTATE.ZA` but not `PSTATE.SM`, checked-squares maximum SVL, and redacts bytes and dimensions. #1226 adds a separate conditional fixed 64-byte SME2 ZT0 capture under the same ZA-only preflight, without querying maximum SVL. #1228 adds ordered nontransactional restore of the complete typed general-register capture, with exact partial-write failure context. #1230 adds the paired restore for the complete typed SP_EL0/SP_EL1/ELR_EL1/SPSR_EL1 capture, with exact partial-write failure context. #1232 adds the paired restore for the complete typed AFSR0_EL1/AFSR1_EL1/ESR_EL1/FAR_EL1/PAR_EL1/VBAR_EL1 capture. #1234 adds the paired restore for the complete typed ACTLR_EL1/CPACR_EL1 capture. #1236 adds the paired restore for the complete typed TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 capture. #1238 adds the paired restore for the complete typed SCTLR_EL1/TTBR0_EL1/TTBR1_EL1/TCR_EL1/MAIR_EL1/AMAIR_EL1/CONTEXTIDR_EL1 capture. #1240 adds the paired restore for the complete typed Q0-Q31/FPCR/FPSR capture. #1242 adds the paired restore for the complete redacted APIA/APIB/APDA/APDB/APGA key state and forms a thirty-operation shared core-register admission domain. #1244 adds the paired restore for the complete redacted SCXTNUM_EL0/SCXTNUM_EL1 value and forms a thirty-one-operation shared core-register admission domain. #1246 adds the paired one-write restore for the complete CSSELR_EL1 selector and forms a thirty-two-operation shared core-register admission domain. #1248 adds paired IRQ-then-FIQ restore under generalized interrupt-operation admission without changing that core-register count. #1250 adds paired debug-exception-then-debug-register-access trap-policy restore and forms a thirty-three-operation shared core-register admission domain. #1252 adds paired MDCCINT-then-MDSCR debug-control restore and forms a thirty-four-operation shared core-register admission domain. #1255 adds independently loaded pre-first-run restore of the complete opaque GIC device blob under generalized interrupt admission. #1258 adds pre-first-run restore of nine mutable EL1 ICC registers plus derived-RPR validation in the same interrupt domain. Public native-v1 create/load use the production aggregate capture/restore commands; ordinary pause/resume and standalone lease diagnostics do not invoke the individual low-level operations. FDT idle-state discovery, non-timer suspend wake, dynamic CPU topology, full snapshot-ready quiescence across the remaining auxiliary/host owners, complete HVF state capture/restore, and fine-grained guest error exit-code parity remain deferred; `SYSTEM_RESET` remains a terminal process outcome. |
| Snapshots and restore | partial; public native-v1 baseline | unit, api socket, process e2e, signed HVF, signed executable, docs | #543, #1048, #1072, #1086, #1158, #1160, #1162, #1164, #1166, #1168, #1170, #1172, #1174, #1176, #1178, #1180, #1182, #1184, #1186, #1188, #1190, #1192, #1194, #1196, #1198, #1200, #1202, #1204, #1206, #1208, #1210, #1212, #1214, #1216, #1218, #1220, #1222, #1224, #1226, #1228, #1230, #1232, #1234, #1236, #1238, #1240, #1242, #1244, #1246, #1248, #1250, #1252, #1254, #1255, #1258, #1260, #1261, #1263, #1264, #1268, #1270, #1272, #1274, #1276 | Public `PUT /snapshot/create` supports native-v1 `Full` only from a paused one-vCPU VM with exactly one regular read-only root drive, default serial, and no optional devices/MMDS. It invokes aggregate capture plus the no-clobber memory-first/state-last kind-2 publisher and leaves the source paused. Public `PUT /snapshot/load` accepts a committed pair through `File` or the deprecated sole `mem_file_path` alias only in a pristine fresh process except logger/metrics, validates before construction, commits a real session as `Paused`, and optionally uses ordinary resume. Snapshot-specific typed execution faults, latency/deprecation metrics, retryable/terminal dispositions, path/value redaction, staging cleanup, orphan/committed-uncertain outcomes, exact local compatibility, external root identity, normalized timers, aggregate architecture/GIC/ICC/pending restore, and post-restore VMGenID replacement are covered. Signed executable coverage synchronizes on a tiny guest's UART metric, publicly creates, terminates the source, restores the immutable pair into two fresh processes, exercises explicit and automatic resume, and allows guest PSCI shutdown only after VMGenID changes. The format is bangbang-native, not Firecracker-compatible or authenticated. `Diff`, UFFD, dirty tracking, clock adjustment, overrides, writable/additional drives, optional devices, active optional architecture/debug state, EL2 GIC state, VMClock mutable restore, and cross-host portability remain unsupported. |
| Memory hotplug | implemented supported virtio-MMIO subset | unit, api socket, process e2e, signed HVF, signed executable, docs | #544, #942, #952, #1022, #1026, #1028, #1030, #1032, #1034, #1040, #1042, #1044, #1046, #1050, #1333, #1334 | Pre-boot `PUT /hotplug/memory`, public requested/plugged status, runtime requested-size PATCH, config-generation signaling, and the one-queue virtio-mem MMIO/FDT device are implemented. Valid `STATE`, `PLUG`, `UNPLUG`, and `UNPLUG_ALL` requests operate in configured block units over complete guest ranges. Exact block-owned guest/HVF mappings can be split or combined; backend mutation precedes ACK publication, device state commits only after guest-visible completion, and partial or late failures roll applied ranges back in reverse order. Focused tests cover adjacent sequential plugs, partial multi-block unplug, one request crossing the conceptual slot boundary, and rollback failures without claiming Firecracker's KVM slot identity. Signed executable coverage proves Linux binds `virtio_mem` and public requested/plugged size completes `0 -> 128 MiB -> 0`. Runtime device deletion, broader public guest-memory accounting, and optional-device snapshot state remain deferred. |
| RTC | implemented Firecracker aarch64 no-interrupt subset | unit, signed HVF, signed executable, docs | #544, #944, #1052, #1074 | A PL031 RTC is registered as MMIO during HVF startup and emitted with Firecracker's `arm,pl031` / `arm,primecell` FDT shape and no interrupt property. The backend-neutral handler implements the current-time, load, match, control, mask, no-interrupt status/clear, and PrimeCell identity register surface with fixed-width validation and Firecracker-shaped error metrics. Signed executable direct-rootfs coverage proves `/dev/rtc0` and PL031 discovery. Alarm interrupts are an explicit boundary of the same upstream no-interrupt aarch64 subset, not a missing parity item. |
| Time and identity devices | implemented startup plus native-v1 VMGenID replacement; VMClock/PVTime profile-limited | unit, signed HVF, signed executable, docs | #543, #544, #946, #1076, #1078, #1080, #1082, #1084, #1261, #1272, #1276 | Startup emits Firecracker-shaped DeviceTree VMGenID and VMClock nodes, initializes a nonzero generation and minimal VMClock ABI, and allocates deterministic SPI lines. Native-v1 load replaces all 16 VMGenID bytes after aggregate interrupt restore, commits retained metadata, and injects an edge-rising SPI; signed public cross-process coverage proves guest-observed replacement and continued execution. Firecracker v1.16.0's ACPI HID correction is not applicable to this aarch64 DeviceTree device. Mutable VMClock restore/generation signaling remains outside native-v1, and ARM PVTime remains platform/architecture-limited because KVM's per-vCPU shared-page ABI has no current HVF equivalent. |
| Remaining Firecracker devices | implemented supported subsets; transport and profile limits explicit | unit, api socket, process e2e, signed HVF, signed executable, docs | #544, #797, #800, #802, #804, #806, #808, #810, #812, #814, #815, #818, #869, #873, #875, #877, #888, #890, #892, #894, #896, #898, #900, #902, #904, #905, #908, #910, #912, #914, #920, #922, #926, #928, #930, #932, #934, #936, #938, #940, #960, #962, #964, #968, #970, #972, #988, #990, #1000, #1002, #1016, #1018, #1024, #1329, #1328, #1330, #1331, #1335, #1336, #1337, #1338 | Balloon implements validated inflate, hinting, statistics, and free-page reporting queues; accepted ranges are wholly mapped, split by owner, aligned inward, zeroed then freed on Darwin, and processed best effort before acknowledgement with requested/advised/skipped/failure metrics. Signed coverage proves driver binding, reporting bit 5, nonzero actual pages, hinting control, and nonzero reporting activity without a synchronous footprint claim. Pmem implements pre-boot backing validation/mapping, virtio-MMIO/FDT attachment, targeted lazy flush of only the notified device, no flush for empty or malformed-only events, one-op plus exact-backing-length rate limiting, pending session-owned retry, atomic live PATCH, peer isolation, metrics, and signed initial-limiter/PATCH/read/flush proof. Entropy implements the 64-KiB request cap, 64-byte aarch64 seed, optional limiter/retry, metrics, and signed `/dev/hwrng` proof. Paired reusable-page accounting, optional-device snapshots, pmem root/direct mapping/dirty tracking, ARM PVTime, mutable VMClock restore, and developer-preview PCI runtime attach/delete remain explicit limits. |
| macOS isolation and platform limits | platform-limited direct jailer parity; App Sandbox feasibility validated | docs, process e2e, signed App Sandbox HVF and process | #545, #924, #1102, #1302 | Security docs cover socket, host-path, entitlement, vmnet host policy, multi-user/operator, and multi-process boundaries. Integration-only app bundles run the complete signed HVF lifecycle suite and prove container-socket service/cleanup plus redacted denial of the default socket and an outside config. The ordinary CLI is not sandboxed. Production bundle distribution, security-scoped resource grants, launcher/broker design, vmnet provisioning, and network policy are separate product/deployment decisions rather than claimed Firecracker jailer parity. |
| Native-v1 baseline device state | public baseline component | unit, signed HVF ownership, signed executable, docs | #543, #1268, #1276 | Exact bounded `BANGDEV\0` state persists one read-only root transport, queue/cursor/interrupt state, frozen limiter/retry time, UART registers, and VMGenID/VMClock topology. Load reopens the root regular file read-only/no-follow with exact descriptor identity and installs drop-safe resources without boot writes. Public signed coverage exercises this profile across fresh processes; optional devices and mutable VMClock restore remain deferred. |
| Native-v1 composite capture | public baseline component | unit, supervisor ownership, signed HVF, signed executable, docs | #543, #1270, #1276 | Kind-2 `BANGCMT\0` binds memory to the exact five-component `BANGHVF\0` baseline. Aggregate runner admission plus supervisor admission/block/entropy quiescence spans encoding and cancellable memory streaming. Public Full create invokes this path; recoverable failures leave the source paused/retryable. Optional profiles, dirty tracking, and portability beyond exact local compatibility remain deferred. |
| Native-v1 paused restore | public baseline component | unit, process lifecycle, signed HVF, signed executable, docs | #543, #1272, #1276 | Public File load validates the committed pair/platform/cache/root before fresh VM construction, installs the baseline runtime without boot writes, performs exact never-run architecture/GIC/ICC/timer/pending restore, replaces VMGenID, and commits a real `Paused` session before optional ordinary resume. Retryable versus terminal cleanup evidence is preserved. Optional profiles, overrides, and Firecracker artifact compatibility remain unsupported. |
| Native-v1 composite publication | public baseline component | unit, process lifecycle, signed HVF, signed executable, docs | #543, #1274, #1276 | The pathless move-only staging writer, closure proof, output/binding match, barriers, and exclusive memory-first/state-last renames back public Full create. Existing finals are not replaced; producer failures clean private staging, late memory finals remain typed orphans, and state-directory sync uncertainty remains committed success. |
| Native-v1 public endpoint activation | implemented narrow subset | runtime, API socket, process lifecycle, signed executable, docs | #543, #1276 | Public Full create and File load route through the production transactions. Load commits `Paused` before either returning or applying `resume_vm`; deprecated `mem_file_path`, metrics, latency, redaction, collision, retryable/terminal errors, explicit resume, automatic resume, VMGenID replacement, and cross-process continuation are covered. |
| Validation matrix maintenance | implemented | docs | #546 | Future capability PRs should update this matrix when support status or validation layers change. Full upstream Firecracker test-suite mapping remains deferred. |

## Historical Prerequisite Landing Notes

The chronological notes below preserve the boundary at each prerequisite's
landing; they do not describe current support status. The #1270 composite row
supersedes their older statements that cache queries were necessarily
non-atomic, captured subsets lacked schema or
orchestration, and composite capture remained deferred. #1276 supersedes every
statement below that public endpoint activation or public snapshot load
remained deferred; those phrases record each slice's landing state rather than
current behavior. The matrix above is authoritative for current support.
Technical destination and optional-state limitations apply only where its
current rows retain them.

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

#1261 extends the lifecycle, snapshot, and time/identity rows with an internal
native arm64 timer and VMGenID restore policy. One timer-domain owner command
normalizes virtual count and full-width physical comparator distance around a
single host-counter sample; a paired sticky-never-run command preflights every
destination field and the counter, strips ISTATUS, ignores TVAL, and applies ten
ordered nontransactional writes. Typed value-free errors report the failed
read/sample/write and completed write prefix; a complete retry recomputes
host-relative fields from a fresh sample. A pure native-v1 classifier rejects
CPACR-enabled SVE/SME, active PSTATE.SM/ZA, and enabled implemented breakpoint
or watchpoint controls without values.

The same slice adds backend-neutral VMGenID replacement that commits retained
metadata only after the complete distinct nonzero 16-byte guest write, plus
borrowed and owned HVF session methods that preflight runner/GIC capability and
assert the edge-rising SPI last. Signal failure is an explicit post-commit
partial stage. Unit coverage exercises wrapping arithmetic, control policy,
every preflight/write failure, fresh retry, admission/lifecycle cleanup,
optional-state precedence/redaction, random/zero/duplicate/write/signal
VMGenID stages, and exact memory/metadata ordering. Signed HVF coverage restores
timer state across destroyed source and fresh destination VMs, verifies shared
elapsed-counter invariants for disabled and armed/masked controls, and proves
both session forms update guest VMGenID bytes and metadata before successful
real SPI injection. The composite restore lease/schema, supervisor/public load
wiring, VMClock restore, guest-observed VMGenID handling, timer EOI policy,
active optional-state restore, and userspace secret rotation remain deferred.

#1296 extends the lifecycle validation foundation without changing public PSCI
support. One owner-thread retained virtual-timer wait derives an exact Mach
deadline from raw offset/control/CVAL state, rechecks an enabled guest-unmasked
timer, and sets its selected PPI before completion. Identity-bound condvar
cancellation composes with active-run batch exits: a canceled wait consumes its
own acknowledgement, while a timer-won race preserves the raw next-run exit
needed for coordinator cancellation debt. Unit coverage exercises wrapping and
timebase arithmetic, every owner/PPI failure, operation admission, mixed-batch
races, and shutdown. Signed HVF coverage proves due/future timers under both
HVF exit-mask states plus disabled/guest-IMASK cancel and shutdown without fixed
sleeps. At the #1296 boundary, PSCI `CPU_SUSPEND`, coordinator suspended
membership, SGI/SPI/direct IRQ/FIQ wake, and guest-visible discovery remained
deferred to #1295.

#1298 activates the narrow guest-facing layer above that foundation. Both
`CPU_SUSPEND` widths reserve an exact retained transaction without changing
`ON` affinity, and suspended members reuse ordinary coordinator generations
for interruptible timer waits. A due enabled, guest-unmasked virtual timer
publishes its PPI before deferred `SUCCESS`; wakeup/pause cancellation rearms
without X0 completion, while stop/shutdown/terminal drains synthesize no wake.
Unit coverage spans decoding, power conflicts, exact runner tokens, mixed/all
suspended scheduling, cancellation debt, and session teardown. The signed
two-vCPU bare guest proves CPU0 can observe CPU1 as `ON` while CPU1 makes no
post-call progress, then proves two real timer wakes preserve non-result
context and return success without fixed sleeps. FDT idle-state discovery,
SGI/SPI/direct IRQ/FIQ wake and powerdown resume remain deferred.

#1300 completes the dependency-ordered PSCI discovery layer after the power
calls are real. `PSCI_VERSION` reports 1.0, and one metadata table defines the
exact `PSCI_FEATURES` matrix plus immediate/coordinated availability; both
CPU_SUSPEND IDs return zero feature bits for original power-state format and
platform-coordinated mode. The retained
Firecracker v1.15.1 `arm,psci-0.2`/HVC FDT binding discovers that runtime
revision just as its KVM baseline does. `SMCCC_VERSION` reports 1.1 with the
mandatory minimum `SMCCC_ARCH_FEATURES` VERSION/self results; optional
architecture workarounds, SoC ID, KVM PV/vendor calls, and TRNG remain
unsupported. Unit coverage exhausts supported and excluded IDs, runner reads
and writes, unknown calls, and nonzero HVC immediates. A signed one-vCPU bare
guest stores 36 feature-query results plus both version and architecture
discovery results before terminating through SYSTEM_OFF without fixed sleeps.

## Update Rule

When a PR changes Firecracker-facing behavior, update this matrix if it changes
support status, adds or removes a validation layer, or moves work between
implemented, partial, deferred, recognized unsupported, or platform-limited
states.
