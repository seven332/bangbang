# Testing Guide

This document defines how to add and run tests in bangbang. Prefer tests that
exercise project behavior through the narrowest public boundary that still
proves the change.

## Test Layers

Use unit tests for small, deterministic logic. Place them next to the code they
exercise under each crate's `src/` tree with Rust's built-in `#[test]`
framework. Unit tests are the right fit for parsers, error formatting, state
transitions, range checks, request validation, and backend-neutral helpers.
The `clippy.toml` test exceptions allow `expect`, `unwrap`, `panic`, and
indexing in `#[test]` bodies, but they do not cover ordinary helper functions in
integration-test crates. If an integration test needs those test-only patterns
in helpers, add a file-scoped allow at the top of that test file:

```rust
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]
```

Keep these allows scoped to test files, and do not use them in production code.

Use normal Rust integration tests when behavior crosses a crate or process
boundary but does not require Hypervisor.framework entitlements. Put these under
the owning crate's `tests/` directory. A PR may start by adding a new
integration test to pin the intended behavior before changing implementation,
especially for CLI, API, filesystem, or cross-crate workflows. The final PR
must leave the new test passing in the documented command set.

Use process-level executable tests when the behavior depends on the real
`bangbang` binary, process arguments, Unix-socket publication, signal handling,
HTTP-over-socket API mutation, or process-owned cleanup but does not enter HVF.
These tests live under `crates/bangbang/tests/` and run in the normal unsigned
workspace test command. They should start `env!("CARGO_BIN_EXE_bangbang")`, use
unique temporary resources, wait on explicit process or socket readiness
signals, and shut the child down with normal signals when testing owned cleanup.

Keep tests that require a signed executable or real HVF execution in separate
Cargo test targets from unsigned tests. Do not hide signing or HVF requirements
behind `#[ignore]` in a normal test target. Mark the dedicated target with
`test = false` in that crate's `Cargo.toml` so `--all-targets` does not run it
accidentally, then run it explicitly from the signed integration runner.

Use HVF crate integration tests for behavior that creates HVF VMs, vCPUs, GIC
state, mapped guest memory, signed test binaries, or guest boot execution
through the `bangbang-hvf` crate. These tests live in `crates/hvf/tests/` and
must run through
`scripts/run-integration-tests.sh` so the binaries are signed with the
`com.apple.security.hypervisor` entitlement. Do not add real HVF tests to the
unsigned workspace test path.

HVF GIC MSI changes require two complementary signed gates. The focused
`hvf_lifecycle` test must create an opt-in GIC, prove the Linux-incompatible
terminal INTID 1019 is not allocated, send the range-provenance token through
the real `hv_gic_send_msi`, and observe that exact INTID through guest
`ICC_IAR1_EL1` with a bounded cancellation fallback. The focused `guest_boot`
test must parse the pre-run FDT, require one hardware-described
`arm,gic-v2m-frame` child and no GICv3 MBI/ITS properties, boot the pinned
Firecracker kernel, and match Linux's exact GICv2m SPI range. Unit tests must
cover opt-in/default separation, dynamic-symbol loading, configuration order
and cleanup, geometry/overlap/range validation, the 1019 guard, allocator
exhaustion/provenance/generation, atomic device-vector allocation and rollback,
exact message routing, sender serialization/errors/redaction, quiesce/drain,
teardown revocation and deterministic reuse, and FDT publication without raw
values in formatted failures.

PCI foundation changes additionally require atomic MMIO registration/release,
slot and BAR lease, type-0 configuration, ECAM, address-plan, FDT, and startup
unit coverage. Failure cases must prove rollback for a prefix overlap or
handler collision, reject wrong-owner/allocator/dispatcher and stale leases,
and keep the no-PCI FDT bytes unchanged. The signed `guest_boot` case
`boots_firecracker_kernel_and_enumerates_the_internal_pci_segment` must run
through `scripts/run-integration-tests.sh --test guest_boot`, inspect the exact
generic-ECAM node and GICv2m parent before execution, then require pinned Linux
to enumerate both `0000:00:00.0 [8086:0d57]` and the identity-only
`0000:00:01.0 [0042:0000]` before the normal boot marker. This is discovery
evidence only.

Modern virtio-pci changes additionally require neutral-core/MMIO regression,
capability-chain and BAR-layout, common/device/ISR/notification access, queue
validation, MSI-X table/PBA masking and pending, exact tuple-registry, ordered
publication/rollback, and stale-handle teardown unit coverage. The signed
`guest_boot` case
`boots_firecracker_kernel_with_modern_virtio_pci_rng_and_distinct_msix_vectors`
must run through the same wrapper. It boots the pinned Firecracker kernel,
requires `[1af4:1044]` and the standard `virtio_rng` driver, reads a bounded
deterministic payload from `/dev/hwrng`, and compares marker-bounded
`/proc/interrupts` snapshots proving independent queue and configuration MSI-X
delivery. Before VM destruction it must unpublish the endpoint and prove stale
rejection plus exact slot, BAR, and GICv2m vector reuse. Both focused signed
cases remain internal conformance evidence and are not unsigned-test
substitutes for the product all-virtio gate.

The hidden PCI data-device conformance mode additionally requires the signed
`guest_boot` cases
`boots_direct_rootfs_and_fsyncs_block_devices_over_modern_virtio_pci`,
`boots_direct_rootfs_and_flushes_pmem_over_modern_virtio_pci`, and
`boots_signed_pci_guest_with_complete_virtio_network_semantics` through
the same wrapper. They check stable BDF/vendor/device identities in guest sysfs,
perform real block write/`fsync`, pmem read/write/flush, and MMDS TCP traffic,
and require programmed, unmasked, distinct queue/configuration MSI-X vectors.
The retained runtime inventory must contain no block, pmem, or network MMIO
registration/FDT node in this mode, and explicit reverse teardown must finish
before VM destruction. Existing MMIO signed cases remain required. These hidden
cases do not by themselves certify the public selector, runtime attach/delete,
guest rescan/removal, hotplug, or PCI snapshot state.

Virtio-net semantic changes additionally require both
`boots_signed_mmio_guest_with_complete_virtio_network_semantics` and
`boots_signed_pci_guest_with_complete_virtio_network_semantics`. Each signed
case must inspect the actual guest-acknowledged feature bitmap, require every
published checksum/TSO/UFO/merged-buffer/ring bit, submit one bounded TCP
request that the host observes as multiple normalized packets, and validate a
49152-byte response in guest userspace. The response must cross transactional
merged RX while an ops limiter produces both a throttle and a later retry event;
the small request in the same run preserves the older MMDS baseline. This is
deterministic internal packet-path evidence, not positive external vmnet
connectivity.

Aggregate network/MMDS reconciliation additionally requires
`network_mmds_closure_policy_is_stable`. The checked
[network and MMDS ledger](../compat/firecracker/v1.16.0/network-mmds-contract.md)
pins exactly 35 identities, 33 terminal outcomes, two named downstream
handoffs, the direct MMIO/PCI transport cases above, signed process isolation
and hotplug, signed capture and exact-2.11 restore, contained production
ownership, and the credential-free vmnet preflight block. These layers compose the
aggregate claim; the repository still does not treat a blocked credential gate
as positive #1378 connectivity evidence.

Public `--enable-pci` changes additionally require
`macos_arm64::signed_executable_runs_all_startup_virtio_devices_over_product_pci`
through
`scripts/run-integration-tests.sh --test executable_hvf_e2e -- <name> --exact`.
The test must launch the signed product binary with the exact flag, configure
balloon, root/data block, MMDS-only network, pmem, vsock, entropy, and
virtio-mem, and require Linux to enumerate their deterministic BDF/device IDs
with no virtio-MMIO FDT nodes. Positive evidence must include root/data reads,
guest block write/`fsync`, MMDS traffic, pmem read/write/flush, at least 1 MiB
of bidirectional vsock I/O, entropy output, balloon inflate/reporting, and the
virtio-mem grow/shrink lifecycle. The same session must prove existing live
block backing/limiter, network limiter, and pmem limiter PATCH paths still
operate through PCI handles. Default signed MMIO cases, exact/attached/
duplicate parser tests, supported-host pre-readiness process startup,
unsupported-target compilation, complete capacity/rollback unit tests, and
native-v1 PCI rejection are mandatory companions. This startup gate does not
by itself certify runtime attach/delete, guest rescan/removal, PCI snapshot
persistence, or external vmnet connectivity; separate signed hotplug gates own
the block, pmem, and network runtime claims.

Runtime block hotplug changes additionally require both
`macos_arm64::signed_executable_hotplugs_and_reuses_runtime_block_over_product_pci`
and
`normal_bundle_hotplugs_runtime_block_from_exact_unused_grants` through the
signed wrapper. Each test starts with a permanent PCI control drive and no
runtime target, performs a Running-state PUT, waits for Linux PCI rescan plus
guest read/write/fsync and sysfs removal, then pauses the VM, DELETEs the first
endpoint, PUTs a second backing through the released capacity, resumes, and
repeats guest I/O/removal before a final DELETE and clean stop. The contained
case must use two exact initially unused manifest grants, replace their source
pathnames after launcher preparation, inject a failed access claim, reuse that
same authority successfully, and prove guest writes reached only the
launcher-opened inodes. Unit companions must cover projection commit order,
default-MMIO rejection before backing use, bounded metrics generations,
publication cleanup, terminal incomplete-publication handling, prepared teardown
rollback, work/message drain, terminal commit handling, paused FIFO admission,
and slot/BAR/vector/dispatcher reuse.
The guest/operator rescan and sysfs-removal handshake is part of this gate; it
is not an automatic notification claim.

macOS block-special drive changes additionally require all four signed owner
gates through `scripts/run-integration-tests.sh`:

- `macos_arm64::signed_executable_replaces_macos_block_special_backings_over_product_mmio`;
- `macos_arm64::signed_executable_hotplugs_macos_block_special_media_over_product_pci`;
- `normal_bundle_replaces_contained_macos_block_special_media_over_mmio`; and
- `normal_bundle_hotplugs_contained_macos_block_special_media_over_pci`.

Together they must cover direct and normal-production ownership, MMIO startup
and PCI runtime attach, complementary Sync/Async plus Unsafe/Writeback order,
read-only/read-write enforcement, live limiter retry without another guest
notification, 4/6/8-MiB capacity/config refresh, exact current
backing-derived GET_ID, real guest read/write/flush/readback persistence,
regular-to-block, block-to-regular, and block-to-block replacement, failed
directory/access candidates without mutation, manual guest removal, DELETE,
same-ID and PCI-slot reuse, and native-v1 capture rejection before artifact
publication. The contained cases must capture twice through fresh launcher
inspection, prove source-path replacements are irrelevant, retain exactly App
Sandbox plus Hypervisor worker entitlements, and drop every worker, broker,
descriptor, and session owner before cleanup.

These tests may use only `tests/support/macos_virtual_block.rs`. That fixture
creates one repository-owned unmounted image with `hdiutil -plist`, accepts only
the single returned node, and freshly verifies the canonical image-to-node
mapping, fstat identity, access, logical block size, block count, and exact
capacity before every detach, including forced cleanup. It must never enumerate
or select existing `/dev` media and must remove only its own image. Real disk
ioctls, App Sandbox behavior, signing, persistence after read-only reattach, and
detach ownership cannot run in the unsigned workspace suite. Unit companions
must still cover block geometry/identity/access rejection, grant and fixed
block-control codec/session/sequence poisoning, direct versus broker control,
capture mismatch, live-update rollback, and the synchronous rate-limited queue
pending state that permits an empty-notification timer retry.

Runtime pmem hotplug changes additionally require both
`macos_arm64::signed_executable_hotplugs_flushes_and_reuses_runtime_pmem_over_product_pci`
and
`normal_bundle_hotplugs_flushes_and_reuses_runtime_pmem_from_exact_unused_grants`
through the signed wrapper. Each test starts without pmem, performs two
Running/Paused PUT/DELETE rounds around manual PCI rescan and sysfs removal,
and requires guest reads plus queue-driven flushes to reach the exact first and
second host backings. The guest records the first PCI BDF and pmem namespace
resource and accepts the second round only when both are reused. The contained
case must inject a failed access claim without consuming the exact pmem grant,
then consume two distinct initially unused grants and prove pathname
replacement cannot redirect either direct mapping. Unit companions must cover
transactional configuration projection, first-fit range exclusion and reuse,
generation-safe metrics ownership, dynamic HVF map/take/restore, failed
map/unmap isolation, endpoint rollback, recoverable versus terminal owner
failures, paused FIFO admission, and default-MMIO rejection before backing use.

Direct pmem or pmem-root changes additionally require the signed
`hvf_lifecycle` cases
`guest_write_to_writable_pmem_is_visible_before_any_pmem_flush` and
`guest_write_to_read_only_pmem_faults_without_mutating_backing`, plus
`direct_pmem_mapping_has_bounded_process_memory_growth`, the
`guest_boot` cases `boots_read_only_ext4_root_directly_from_mmio_pmem` and
`boots_writable_ext4_root_directly_from_modern_pci_pmem`, the public-process
case `macos_arm64::signed_executable_boots_read_only_and_writable_pmem_roots`,
and the normal-bundle case
`normal_bundle_boots_read_only_pmem_root_from_exact_granted_descriptor`.
Together they must prove one authoritative mapping before flush, exact guest
protection, no second full-size virtual mapping with a generous resident-size
bound, `/dev/pmem<i>` plus `ro`/`rw`, MMIO/PCI enumeration, runtime-root
rejection, and exact contained descriptor identity after pathname replacement.
The wrapper must run on Apple Silicon without `--allow-unsupported`.

Aggregate storage closure changes additionally require both
`macos_arm64::signed_executable_certifies_aggregate_storage_semantics_over_product_pci`
and
`normal_bundle_certifies_aggregate_storage_semantics_through_contained_grants`
through `scripts/run-integration-tests.sh`. Each launches one signed
product-PCI guest with a read-only Sync root, writable Sync control drive,
writable portable-Async data drive, writable vhost-user drive, writable pmem
device, and virtio-mem. The generated direct-rootfs init selects this profile
with `bangbang.storage-certification=1` and discovers storage from unique
on-media markers; startup tests must not assume fixed BDFs after the permanent
root/control functions.

Both profiles must prove initial and continuing Sync/Async/vhost/pmem I/O,
Writeback and queue-driven persistence, disjoint concurrent block/pmem/vhost
updates, paused Async replacement, `0 -> 128 MiB -> 0` memory growth, exact
final config projection, and clean termination. Dynamic block and pmem
attach/remove/reinsert phases stay serialized around Linux PCI rescan and sysfs
removal: placing both removed namespaces in the kernel's tombstone state at
once can block deterministic rescans and is not part of the public concurrent
mutation claim. The second insertion must reuse the first block slot and pmem
slot/resource without sleeps. The direct branch must then kill the vhost
backend and observe terminal frontend/process cleanup; the contained branch
must prove exact grants/children, pathname-replacement resistance, redaction,
unchanged entitlements, and orderly frontend/session/helper cleanup.

Unsigned companions must pin no-side-effect capacity ordering. In particular,
`runtime_pmem_owner_preflight_precedes_grant_claim_mapping_and_config_commit`
must exhaust an owner-side resource before runtime PUT and prove the error is a
capacity error, not a grant error; insert count, public config, active claims,
broker requests, opens, and mappings remain zero, and the exact grant stays
reusable. Shared endpoint, pmem inventory, PCI function, BAR, MSI-X,
dispatcher, and metrics checks remain focused lower-layer companions. The
checked capability-audit test must continue enforcing an exact 40-row storage
ledger with all 40 records terminal.

Runtime network hotplug changes additionally require both
`macos_arm64::signed_executable_hotplugs_mmds_network_and_reuses_product_pci_slot`
and `normal_bundle_hotplugs_mmds_network_without_vmnet_authority` through the
signed wrapper. Each starts with one MMDS-selected PCI network and a permanent
control drive, lets Linux remove the startup function, then performs two
host-DELETE/runtime-PUT rounds. The guest must rescan, find modern virtio-net by
the configured MAC, require the original BDF, bring the link up, complete a
real MMDS request, and remove the function through sysfs before each host
DELETE. The second PUT occurs while Paused. The normal production bundle must
retain its exact networkless signature and no vmnet authority, reject one
non-MMDS bridged runtime request without live-config mutation, and still finish
both MMDS-only rounds. Unit companions must cover duplicate ID/MAC and capacity,
generation-safe metrics reuse, independent provider classes, actual-live-vmnet
authority counting, explicit vmnet stop/drop, packet-I/O and endpoint
take/restore, publication/removal injection, terminal cleanup, snapshot and
shutdown admission, paused FIFO ordering, default-MMIO rejection, redaction,
exact lifecycle-session propagation through start/restore/capture, rejection of
same-policy authority from a different session before both vmnet and MMDS-only
paths, and exact PCI lease reuse. The signed networkless matrix must try host,
shared, and bridged positive policies and prove the fixed rejection occurs with
no worker-session directory creation. Apple-approved vmnet credentials and real
external connectivity remain separate #1351/#1378 gates.

Aggregate runtime PCI hotplug changes additionally require
`runtime_mixed_device_mutations_preserve_type_scoped_identity_and_live_configuration`
and
`boot_run_loop_supervisor_serializes_concurrent_mixed_runtime_mutations` in the
`bangbang` binary tests, plus
`runtime_pci_endpoint_capacity_is_shared_across_mixed_device_types` and
`mixed_full_pci_inventory_fits_reserved_runtime_vector_headroom` in the HVF
library tests. Together they pin equal cross-type IDs, same-type and
duplicate-MAC rejection before session mutation, mixed insertion/removal and
live-config truth, exactly-once owner-thread execution from concurrent command
handles, the shared 31-endpoint boundary, fail-closed overflow, and vector
headroom at that boundary. This #1423 aggregate gate must be run with all three
class-specific signed block, pmem, and network gates above; it does not claim a
single mixed signed guest scenario, automatic guest notification, PCI snapshot
persistence, or external vmnet connectivity.

Ordered HVF vCPU-topology changes require a signed `hvf_lifecycle` baseline
that creates one VM and GIC before two permanent owner-thread runners, proves
their exact ordered MPIDRs are `[0, 1]`, cancels both before their first bounded
run, shuts them down in full, and destroys the VM. Unit tests must inject count,
host-capacity, allocation, owner-start, affinity write/readback, channel,
cancel, and shutdown failures and assert reverse cleanup plus primary error
precedence without entering unsigned HVF.

Concurrent topology-runner changes additionally require deterministic fake
tests for submit-before-collect, out-of-order identified completions, shared
MMIO identity, online/offline membership, stale generations, partial submission
unwind, one active-only batch call, cancellation debt, exact control barriers,
reason coalescing, terminal precedence, and indexed owner operations. The signed
gate must configure two different guest entries in one mapped memory, have each
vCPU write its own flag and wait for its peer, poll both flags with a deadline
and no fixed sleep, then collect two `Canceled` acknowledgements from one stop
barrier. Repeat complete owner and VM teardown to catch stale cancellation or
resource leaks.

HVF dirty-write protection changes require focused tests for page alignment,
overflow and mapped ownership; retained original permissions; complete
preflight; reverse activation rollback and terminal incomplete rollback;
tracked dynamic add/remove success and rollback; exact initial/reprotected
syndromes and unowned-MMIO discrimination; same-page first-writer
serialization; bounded peer stale exits; page-unprotect, epoch-reset, and stop
retry; and owner-before-cleanup ordering.
Runner tests must prove the dirty branch runs before MMIO without taking its
lock, does not read or advance PC, performs no hidden second run, and preserves
ordinary MMIO PC advancement. The signed `hvf_lifecycle` gate must use at least
two vCPUs writing shared and distinct protected pages through two reset epochs,
include a current-device write in the shared bitmap, explicitly redispatch each
dirty outcome, verify final guest values and both exact sets, bound event
progress without sleeps, batch-cancel, join every owner, restore permissions,
and destroy the VM. Accepted signed syndromes are EC `0x24`, WnR set, S1PTW
clear, and translation DFSC `0x05`, `0x06`, or `0x07`, or level-three
permission DFSC `0x0f`, at a tracker-owned currently protected IPA. CM may be
clear for ordinary stores or set for observed Linux cache-maintenance writes;
the ownership and protection checks remain mandatory. Every other encoding
must fail closed and reopen feasibility; tests must not broaden this set.

Run the focused signed proof with:

```sh
scripts/run-integration-tests.sh --test hvf_lifecycle -- tracks_concurrent_guest_writes_with_exact_retry_and_bounded_cancellation --exact
```

Retained virtual-timer owner waits require pure tests for the Arm unsigned
`CVAL <= virtual count` condition, wrapping offset subtraction, Mach timebase
conversion, injected failures for every owner read and PPI write,
and deterministic timer-versus-cancel arbitration. If timer completion wins a
control race, the next raw HVF exit must remain queued so coordinator
cancellation debt can drain; if cancellation wins, the retained completion
must consume that debt without setting a PPI. The signed `hvf_lifecycle` gate
must program real due and future virtual timers under both HVF exit-mask
states, verify guest-disabled and guest-IMASK waits cancel, and prove shutdown
drains an indefinite wait. Use Mach deadlines, admission observations, and
completion acknowledgements rather than fixed sleeps. This foundation does not
by itself advertise or validate PSCI `CPU_SUSPEND`.

Guest-facing PSCI `CPU_SUSPEND` validation must additionally cover both calling
conventions and all three ignored arguments, exact pending runner/power tokens,
unchanged `ON` affinity, no X0 write before wake, and PPI publication before
deferred `SUCCESS`. Suspended members must share normal run generations with
runnable peers. Wakeup and pause cancellation must retain and rearm the exact
transaction; timer-won cancellation debt must be consumed before later guest
execution; stop, shutdown, and terminal drains must not synthesize success.
The signed `hvf_lifecycle` proof uses a two-vCPU bare guest: CPU0 provides an
AFFINITY_INFO checkpoint while CPU1 has made no post-call progress, then CPU1
must complete two real virtual-timer suspend cycles with preserved non-result
register sentinels. Use guest publications, observed run-loop steps, and a
bounded watchdog rather than fixed sleeps. Do not claim FDT idle discovery,
SGI/SPI/direct IRQ/FIQ wake, discovery revision changes, or powerdown resume
from this gate.

PSCI 1.0 discovery validation is a separate gate after CPU_OFF/re-entry and
CPU_SUSPEND retention pass. Table tests must cover every advertised PSCI ID,
both CPU_SUSPEND feature values, optional PSCI 1.0 and PSCI 1.1+ exclusions,
SMCCC_VERSION, mandatory SMCCC_ARCH_FEATURES VERSION/self queries, optional
architecture IDs, unknown calls, 32-bit zero extension, and direct versus
coordinated availability. Runner tests must prove exact X1 reads and X0 writes
without deferred PSCI work while preserving nonzero-HVC rejection. The signed
one-vCPU guest stores the complete supported/unsupported query table and both
revision results in guest memory before SYSTEM_OFF; drive it with observed
steps and a bounded watchdog, never fixed sleeps. Retain the Firecracker
`arm,psci-0.2` FDT binding and do not infer host mitigation, KVM PV/vendor,
TRNG, PSCI 1.1+, or optional power-service support from this gate.

The ARM PVTime gate starts with the exact standard firmware and memory contract.
Runtime tests must prove the exact 64-byte
little-endian revision-0/attributes-0 structure, zero padding, one aligned and
nonoverlapping record per vCPU, deterministic topology ordering, bounded arena
exhaustion, committed-prefix rollback, and no FDT advertisement. HVF unit tests
must cover checked Mach-to-nanosecond conversion, missing-symbol and error
redaction, retained aligned atomic publication and dirty marking, owner-thread
admission and cleanup, per-vCPU IPA and counter isolation, exact
64-bit `PV_TIME_FEATURES` self/`PV_TIME_ST` queries and `PV_TIME_ST` results,
unknown-feature denial, both 32-bit aliases, publish-before-run ordering,
saturating runnable wall-minus-execution deltas, canceled/virtual-timer discard,
clock regression and sampling stages, topology-ordered pause-gated capture, and
fresh-baseline continuation. Production enables the per-owner policy only after
the runtime measurement symbol, every publisher, and the exact topology are
configured; missing support remains undiscoverable and partial setup aborts.
Signed `hvf_lifecycle` proof queries the real public macOS 11 execution-time
primitive on the permanent owner thread. Signed `guest_boot` then uses a hidden
real-delay admission probe and the production calculator to require Linux's
`stolen time PV` discovery message, nonzero monotonic aggregate `/proc/stat`
steal ticks under contention, unchanged ticks after disabling the probe, and
unchanged captures across a completed pause interval:

```sh
scripts/run-integration-tests.sh --test hvf_lifecycle -- measures_real_hvf_vcpu_execution_time_on_owner_thread --exact
scripts/run-integration-tests.sh --test guest_boot -- certifies_linux_pvtime_contention_idle_and_paused_accounting --exact
```

These live gates do not claim KVM's ARM device attribute or cross-host clock
portability. Native-v2 #1529 separately covers portable PVTime encoding,
focused restore orchestration, and repeated immutable clone behavior; it does
not turn the KVM attribute into an HVF mechanism or activate the public v2
lifecycle.

Validation for internal PSCI secondary-power changes must cover both CPU_ON calling
conventions, exact X1-X3 reads and 32-bit truncation, MPIDR reserved-bit
validation, all `OFF`/`ON_PENDING`/`ON` transitions and affinity results,
already-on/on-pending/invalid-target/invalid-entry/internal-failure responses,
stale transaction rejection, target setup success and rollback, retryable
caller X0 completion, response abandonment, and unchanged public CPU_ON
rejection. Target owner-thread tests must preserve context in X0, clear X1-X3,
apply the Linux boot PSTATE, write PC last, stop at every injected failure, and
require a complete retry while the target remains fail-closed. Session tests
must also prove target-only admission precedes caller `SUCCESS`, a pending
caller is not resubmitted, barrier acknowledgements retain ordinary work, and
timer PPIs use the completing index. CPU_OFF coverage must prove the last
committed online CPU receives `DENIED`, a successful call consumes only its
exact pending runner token without writing X0, scheduler removal precedes the
power-state `OFF` commit, abort restores `ON`, and a later CPU_ON reuses the same
owner. Re-entry tests must prove `SCTLR_EL1` is cleared before the existing
X0-X3, PSTATE, and PC-last publication and must not claim a complete cold reset.

The signed `guest_boot` gate builds a deterministic `/smp-init`, boots with two
internal vCPUs, verifies FDT CPU nodes and PSCI enable methods for MPIDRs
`[0, 1]`, pins PID 1 to CPU1 with raw `sched_setaffinity`, confirms `getcpu == 1`,
and only then writes `BANGBANG_SECONDARY_CPU_OK`. Use deadline/marker
synchronization and the signed wrapper; do not add a fixed sleep. Public process
startup is covered separately by the signed executable target. Its generated
`/smp-progress-init` verifies CPU0 and CPU1 affinity, gates progress until both
are ready, then emits distinct non-ASCII one-byte tokens from each pinned role
with a brief guest nanosleep between tokens to keep the observation fixture bounded.
The public test pauses one two-vCPU process, uses both token streams from an
isolated peer as an event-driven observation window, requires the paused serial
bytes to stay exact, and requires both streams to resume. It also repeats
`Paused` while paused and `Resumed` while running, requiring `204`, stable
public state, no extra backend generation, and continued peer isolation.
Focused controller/process tests additionally prove these no-ops still require
the retained session and successful HTTP requests record their own latency.
Native-v1 multi-vCPU acceptance remains a separate negative gate.

The generated `/smp-hotplug-init` mounts sysfs, takes CPU1 offline through
Linux's CPU hotplug interface, proves the migrated worker is quiescent with a
phase/shared-counter handshake, brings CPU1 online, reapplies CPU1 affinity,
and proves progress resumes before emitting `BBHOTDONE`. The signed guest and
public executable tests must observe `BBHOTREADY`, `BBHOTOFF`, and `BBHOTDONE`
with deadlines and deterministic yields rather than fixed sleeps.

For virtio-pmem changes, unit tests should cover MMIO registration, FDT
metadata, config-space `start`/`size`, deterministic multi-device layout,
queue parsing/completion, direct mapping lease lifetime, exact-prefix
`MS_SYNC`, and cleanup/error paths. Targeted flush tests must prove empty and
malformed-only events do not synchronize,
one valid request caches one selected-device result, peer backings are not
traversed, one operation plus exact backing length is charged before flush,
throttled cursors retry, and live limiter replacement is failure-atomic. Signed
HVF coverage validates the exact host address/GPA/size/protection, proves a
writable guest store is visible through an independent backing handle before
flush, and proves a read-only guest store faults without mutating the backing.
Signed guest and executable coverage should retain read-only MMIO and writable
PCI pmem-root boot, initial limiter, live PATCH, guest read/write, and
selected-backing flush proof. The production-bundle target must also retain the
exact launcher-opened descriptor after pathname replacement.

For virtio-mem changes, focused tests should cover block-aligned validation,
adjacent sequential plugs, partial multi-block unplug, split/combined exact
mapping ownership, a request crossing the conceptual slot boundary, guest
completion before state commit, and reverse rollback including injected
rollback failure. Signed executable coverage should retain Linux driver binding
and public requested/plugged status across `0 -> 128 MiB -> 0`; it must not
substitute a requested-size-only observation for guest-completed plug/unplug.

## What To Cover

For CLI and API changes, cover successful requests, unknown options or fields,
empty values, duplicate values, malformed inputs, exit codes, HTTP status codes,
and Firecracker-shaped response bodies.

For machine configuration changes, keep syntax and semantic evidence separate.
Parser tests cover required/default/null fields, strict unknown/duplicate
fields, integer representation, enum names, and `Empty PATCH request.`.
Runtime/API/process tables cover default GET, PUT replacement, PATCH
preservation/clear, vCPU 0/1/32/33, memory 0/1/1,046,528/1,046,529, combined
aarch64 SMT-vCPU-memory precedence, odd/even `2M`, balloon compatibility,
value-redacted typed faults, and GET/state atomicity after failure. Maximum
memory is a configuration-only process test; do not allocate a 1022-GiB test
VM. Host admission uses deterministic injected topology tests plus practical
signed two-vCPU HVF/executable evidence. Exact `2M` uses stable rejection tests
and the authoritative platform evidence in the checked machine-memory contract;
do not relabel alignment or a 16-KiB IPA granule as signed success.

For host filesystem paths, cover missing paths, directories, unsupported file
types, redacted error messages, cleanup ownership, and failure atomicity. A
failed operation should not partially mutate accepted configuration, guest
memory, or host resources.
For `seccompiler-bin`, also cover help/version, missing/duplicate/unknown and
attached short options, the default and explicit output paths, both target
architectures, basic and split modes, deterministic replacement, bitcode decode
through Firecracker's map shape, little-endian raw split decode, and independent
classic-BPF execution. Input cases include empty, missing, oversized,
non-UTF-8, directory, symlink, FIFO, socket, schema, and syscall failures.
Output cases include absent/existing/mixed regular files, symlinks,
directories, FIFOs, sockets, a replacement arriving after preflight, every
split publication boundary, and distinct rollback/durability/cleanup outcomes.
Fault injection stays binary-private; do not add an environment variable or
hidden production CLI switch. A pinned upstream Linux oracle is maintainer
evidence, not a checked build or CI dependency.
For deferred-open paths such as serial output, also cover that parsing stores
configuration without opening the path, and that startup wiring opens or writes
through the selected sink with redacted errors.
For boot-source payload failures, cover both request/API fault formatting and
config-file startup failure paths. Use a test starter that invokes runtime boot
resource assembly when the behavior does not need real HVF execution; keep real
signed executable/HVF coverage in dedicated integration targets.

For guest memory, address, and range logic, cover exact-fit success, one-past
failure, overflow failure, overlapping ranges, and no-partial-mutation behavior.
Native snapshot memory tests additionally pin both binary headers and CRC
golden bytes; preserve discontiguous, adjacent, and dynamically inserted region
boundaries; cross every fixed I/O chunk boundary; reject malformed counts,
lengths, offsets, alignment, ordering, overlap, identity, and integrity; and
inject short, interrupted, zero-progress, seek, allocation, and guest-access
failures. Length-preflight tests must prove zero-position restoration before a
rejection, while late truncation/growth tests prove the final trailer/EOF guard
and that partial guest memory never escapes. Cancellation tests check every
fixed write stage and successive 1 MiB chunks, prove that no binding escapes,
and then reuse a fresh writer successfully. Run the focused module with
`cargo test -p bangbang-runtime snapshot_memory --locked`.

Native-v2 structural state tests pin an independent exact 72-byte empty
`2.0.0` fixture and keep the named native-v1 fixture byte-for-byte stable. A
private catalog-aware test codec exercises multiple required features,
semantic components, instances, and an ignorable nonsemantic extension. The
production catalog admits semantic memory kind 1 introduced in minor 1; the
public Full `2.12.0` writer additionally admits machine/global/topology kinds 2–4
and per-vCPU kind 5 introduced in minor 2, singleton time kind 6 introduced in
minor 3, optional singleton device-graph kind 7 introduced in minor 4, and
mandatory singleton serial kind 8 introduced in minor 7, plus optional entropy
kind 9 introduced in minor 8, optional balloon kind 10 introduced in minor 9,
optional virtio-mem kind 11 introduced in minor 10, and optional network/MMDS
kind 12 introduced in minor 11, plus optional vsock kind 13 introduced in minor
12.
Exact `2.4.0` retains device-graph profile 1's singleton root, while `2.5.0`
uses profile 2's bounded ordered block vector and `2.6.0` uses profile 3's
bounded ordered block-and-pmem vector. Exact `2.7.0` requires kind 8 and
optionally composes the unchanged profile-3 graph. Exact `2.8.0` retains those
rules and optionally appends kind 9 with one exact queue, dual buckets,
pending/retry state, and MMIO/PCI placement. Exact `2.9.0` retains those rules
and optionally appends kind 10 with variable active queue state, latest and
pending statistics, DONE-normalized hint history, exact PFN accounting, and
MMIO/PCI placement. Exact `2.10.0` retains those rules and optionally appends
kind 11 with configuration, config space, an inactive or active queue, a canonical
plugged-block bitmap bound to exact kind-1 extents, common virtio state, and
MMIO/PCI placement. Exact `2.11.0` retains those rules and optionally appends
kind 12 with ordered interface configuration, inert selector identity,
requested/realized MAC/MTU, backend class, queue/common-virtio state,
limiter/retry, MMIO/PCI placement, and MMDS protocol configuration while
excluding source owners, packets, connections, data, tokens, metrics, and
clocks. Exact `2.12.0` retains those rules and optionally appends kind 13 with
the guest CID, inert backend selector, host-local port cursor, active
queue/common-virtio continuation, and MMIO/PCI placement while excluding
listeners, connections, callbacks, metrics, and other host authority. Exact
`2.13.0` retains kinds 1–13 unchanged and requires Diff kind 14, matching the
current compatibility ceiling and public Diff writer. Exact `2.3.0` remains the
legacy device-free platform profile. The mutation corpus
covers every fixed header field, both count caps, exact/trailing/oversized
lengths, all three offsets, CRC and every truncation, feature
zero/order/duplicate/unknown cases, and component
key/order/flag/reserved/empty/gap/overlap/wrap/trailing-range cases. It also
checks patch/minor/major policy, introduction-minor catalogs, borrowed views,
redacted diagnostics, allocation failure, native-v1/v2 family dispatch, and
named Firecracker-family incompatibility without invoking an unsupported
resource action. Run the focused surface with
`cargo test -p bangbang-runtime snapshot_format --locked`.

Native-v2 lazy-memory tests retain exact multi-extent binding and complete
`2.1.0` compatibility fixtures while proving that ordinary Full output uses
`2.12.0`; Diff tests separately prove exact `2.13.0` state/layer bindings.
They cover canonical 64-KiB metadata/data offsets and sparse gaps, every
binding/header/topology/length mutation, exact admitted-version retention,
typed state profiles,
read-only/CLOEXEC/regular descriptor policy, final-symlink rejection,
descriptor/path replacement, source truncation at both rechecks, retained-file
and cursor independence, private COW isolation, clean dirty baselines, mixed
private/anonymous/shared owners, source-preserving discard, pre-mmap validation,
and partial mapping rollback. Writer coverage keeps output empty/position-zero,
bounds all copying to 1-MiB chunks, exercises cancellation without returning a
binding, and proves the exact final length. Run it with
`cargo test -p bangbang-runtime snapshot_memory_v2 --locked`.

Native-v2 HVF platform tests round-trip canonical 1-, 2-, and 32-vCPU graphs,
all stable lifecycle dispositions, U32/U64/U128 CPU-application evidence, and
explicit maximum-SVL SME Z/P/ZA/ZT0 state. They also round-trip the fixed
PL031/PVTime/VMGenID/VMClock schema and its four policy tags. Checksum-valid
component rebuilds exercise exact profile/singleton/instance rules; every
platform and time header, flag, policy, reserved field, count, ABI, and length
bound; closed optional ordering, duplicate/unknown tags, disposition, width,
feature dependencies, SIMD aliases, and the 16 MiB composite budget. Locally
valid machine-memory/FDT, timer, optional-identity, redistributor, vCPU-count,
RTC, canonical identity placement/SPI, PVTime layout/backing, and time-ABI
mismatches prove whole-graph validation. CPU-template unit tests separately
prove the receipt appears only after topology-wide application and retains the
logical/common/effective equation with redacted diagnostics. Reconstruction
tests inject every preflight/VM/memory/GIC/topology/CPU/compatibility/global/
per-vCPU/RTC/PVTime/identity/lifecycle/publication stage, assert the exact
shared reverse cleanup sequence and identity commit boundary, and retain all
cleanup failures. CPU replay tests require every destination baseline read
before the first effective-target write and preserve read/apply failure
positions. Run these surfaces with
`cargo test -p bangbang-hvf --lib --all-features --locked`.

The signed `hvf_lifecycle` lazy-memory case writes a 64-MiB image, drops the
source allocation, loads the retained file mapping, and proves bounded resident
and fault growth before guest entry. The guest's first access writes a distant
untouched page, proving the lazy File/COW level-two translation exit is owned by
the dirty tracker; it then resumes, reads back the value, executes two HVC
exits, and leaves the source bytes unchanged. The test also proves a clean
initial dirty epoch, the exact dirtied page, ordinary unmap/destroy, and
post-VM-destroy owner cleanup. Run it only through
`scripts/run-integration-tests.sh`; unsigned workspace tests must not execute
real HVF.

The same signed target contains the native-v2 platform completion gate. A
three-vCPU bare guest reaches a paused runnable/suspended/offline graph, writes
fresh canonical memory images, rejects a bounded memory-stage cancellation,
recovers the inner coordinator without guest dispatch, and pauses/captures
again to prove non-consuming source ownership. It structurally decodes before
construction, loads
already-authorized memory, destroys the source VM, and restores fresh focused
platforms repeatedly from the same immutable graph. Before ordinary progress,
the guest acknowledges VMGenID then VMClock and observes a fresh clone ID,
the saved-counter VMClock transition, destination-current PL031 rather than
source mutable state, and exact PVTime discovery/cumulative time. The proof
also recaptures one restored clone to a fresh image and restores it again.
Before each resume it requires an equivalent lifecycle graph and unchanged
ordinary guest progress. After resume it proves retained virtual-timer PPI
publication and `CPU_SUSPEND64` completion, primary continuation, CPU_ON of the
initially offline third owner, both secondary CPU_OFF transitions, a final
valid recapture, and clean shutdown. Run the focused proof with:

```sh
scripts/run-integration-tests.sh --test hvf_lifecycle -- \
  --exact native_v2_three_vcpu_platform_round_trip_preserves_paused_lifecycle_and_progress
```

The private production-process composition has a separate signed group:

```sh
scripts/run-integration-tests.sh --test native_v2_process
```

The wrapper builds the `bangbang` binary unit-test harness, locates and signs
that exact test executable. One idempotent helper also builds, separately
signs, and strictly verifies the `rebase-snap` and `snapshot-editor` binaries
whenever either `native_v2_process` or `production_bundle` requires them, so a
targeted production selection has no hidden group-order dependency. Only the
exact private Diff seam receives both fixed test-only paths through
`BANGBANG_REBASE_SNAP_PATH` and `BANGBANG_SNAPSHOT_EDITOR_PATH`; production
bundle tests receive the signed editor path for their real product evidence.
Other private seams receive no tool activation. Its minimal two-vCPU guest does not touch serial or optional
devices beyond the required read-only root. The first test starts and pauses the real
process-owned HVF supervisor, publishes one exact 2.12 Full
serial-plus-profile-3 MMIO-root pair without entropy, resumes and repauses the
source, publishes a fresh recapture,
and drops the source. It
restores the first immutable pair into one fresh normal process initially
`Paused`, proves the exact root configuration plus fresh destination
UART/metrics state, explicitly resumes and repauses it, then shuts it down. A
second fresh process restores the same pair with resume intent through
already-opened contained state/memory/root descriptors after both artifact
paths are replaced, uses the ordinary action gate to reach `Running`, pauses,
and shuts down. The replacements remain untouched. Additional ignored seams
exercise complete MMIO and PCI root-owner commit/rollback boundaries,
exact-2.11 network owner transactions, and exact-2.12 vsock owner transactions
over both MMIO and PCI. The exact-2.13 activation seam additionally proves a
tracked direct-MMIO zero-root Diff, an untracked contained-PCI Full→Diff rebase,
strict optional-balloon state, both load forms, and restored-lineage recapture.
This group is part of the default integration set and
must run without
`--allow-unsupported` on supported Apple Silicon. The tool paths are confined
to this signed test harness and do not add production API, config-file, or
hidden command activation.

### Native-v2 2.13 Diff and rebase certification

The final focused evidence is compositional. Each layer owns a distinct
observable boundary rather than repeating the complete matrix in one signed
test:

| Boundary | Exact evidence |
| --- | --- |
| Format, selection, lineage, and final bytes | `current_dynamic_add_and_remove_topologies_write_canonically`, `repeated_complete_application_handles_add_then_remove`, `exact_minor_thirteen_diff_closes_all_sixty_four_mmio_and_pci_products`, and the snapshot-artifact/lineage failure suites |
| Rebase transaction and command behavior | `sparse_cross_directory_and_repeated_rebases_are_exact`, the complete injected race/failure/cleanup matrix, `both_commands_materialize_byte_identical_complete_images`, `sequential_commands_apply_repeated_lineage_exactly`, signal/substitution cases, and the shared 0/1/2/3/130/143 outcome tests |
| Signed real-HVF chain | `signed_native_v2_diff_process_loads_zero_root_and_rebased_products` creates tracked and untracked layers, invokes both separately signed tools, restores a contained PCI result, and recaptures its exact predecessor |
| Ordinary product and App Sandbox boundary | `normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox` creates and loads a tracked zero-root Diff through exact path-scoped outputs plus post-adoption replacement of every descriptor-backed source/input pathname over MMIO and PCI, describes `v2.13.0`, publishes Paused, resumes to real guest `SYSTEM_OFF`, and proves immutability and cleanup |
| Inventory closure | `snapshot_diff_rebase_terminal_policy_is_stable` pins all fifteen terminal rows, exact evidence paths, the checked ledger, the snapshot-editor state-ledger handoff, and the Wave 6 aggregate composition |

Use these focused commands while changing this surface, then run the full
repository matrix and complete signed wrapper:

```sh
cargo test -p bangbang-firecracker-capability-audit --test checked_inventory snapshot_diff_rebase_terminal_policy_is_stable --locked
cargo test -p bangbang-snapshot-tools --all-targets --all-features --locked
cargo test -p bangbang-runtime snapshot_diff_v2_13 --all-features --locked
cargo test -p bangbang-runtime snapshot_rebase --all-features --locked
cargo test -p bangbang-hvf --lib --all-features --locked exact_minor_thirteen_diff
cargo check -p bangbang-snapshot-tools --all-targets --all-features --locked --target aarch64-unknown-linux-musl
scripts/run-integration-tests.sh --test native_v2_process
scripts/run-integration-tests.sh --test production_bundle -- --exact normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox
```

### Wave 6 snapshot certification

`snapshot_wave6_terminal_policy_is_stable` independently reproduces the exact
70-record boundary: 26 API identities, the 27-record snapshot source family,
one snapshot-version process argument, and 16 explicit producer handoffs. It
requires 68 terminal evidence-bearing rows, retains only the two network
aggregates for #1378/#1491, compares the exact set with
`snapshot-wave6-contract.md`, and pins the repository totals at
296/102/3/17.

The one new product observation lives inside the existing
`normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci` lifecycle.
Immediately after each rooted/rootless x MMIO/PCI Paused destination recapture,
`assert_production_snapshot_time_identity_transition` independently validates
the source and recaptured state-memory bindings, requires a fresh nonzero
VMGenID and changed exact 112-byte VMClock fingerprint, and compares every
other canonical profile and time fact without logging confidential values.
The device comparison normalizes only numeric limiter `age_nanos` and retry
`remaining_nanos` values while retaining their presence/type and comparing the
rest of the device graph exactly. The ordinary explicit resume, guest
completion, immutability, grant, cleanup, and session assertions then continue
unchanged.

Use these focused checks while changing the Wave 6 ledger or assertion:

```sh
cargo test -p bangbang-firecracker-capability-audit --test checked_inventory snapshot_wave6_terminal_policy_is_stable --locked
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
cargo run -p bangbang-firecracker-capability-audit --locked -- compare --firecracker ../firecracker
scripts/run-integration-tests.sh --test production_bundle -- normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci --exact
```

### Snapshot-editor state certification

The state commands have a separate twelve-record closure rather than extending
the memory-rebase ledger. Unit and actual-binary layers cover every admitted
native profile, the deterministic redacted JSON schema, exact 67-ID request
admission, canonical profile-preserving transformation, immutable inputs,
owner-only no-clobber output, all path/content/parent races, injected failures,
signals, stream closure, staging cleanup, and durable-versus-uncertain exits.

The signed product layer reuses the existing real snapshot lifecycle instead
of composing unrelated artifacts. For Full 2.12 and Diff 2.13, and for both
MMIO and PCI, the actual supplied editor runs `version`, `vcpu-states`, and
`vm-state` twice, removes DBGBVR0 value ID `0x6030000000138004` exactly once,
and reinspects the distinct output. The test compares the complete JSON after
normalizing only `vcpus[*].debug.reviewed`, pins exact profile, transport,
memory, and Diff relationships, and proves original bytes/inode facts,
owner-only output, and zero staging residue. It then passes only the edited
state plus unchanged memory/layer and drives through retained launcher grants,
replaces every adopted pathname, observes Paused, explicitly resumes, reaches
guest `SYSTEM_OFF`, and rechecks both original and adopted artifacts.

The targeted `production_bundle` group independently builds, ad-hoc signs,
verifies with `codesign --verify --strict`, and supplies the editor; running
`native_v2_process` first is neither required nor sufficient. Use:

```sh
cargo test -p bangbang-firecracker-capability-audit --test checked_inventory snapshot_editor_terminal_policy_is_stable --locked
cargo test -p bangbang-snapshot-tools --all-targets --all-features --locked
cargo test -p bangbang-runtime snapshot_state_edit --all-features --locked
cargo test -p bangbang-hvf --lib --all-features --locked snapshot_document::inspection
cargo test -p bangbang-hvf --lib --all-features --locked snapshot_document::register_removal
cargo check -p bangbang-snapshot-tools --all-targets --all-features --locked --target aarch64-unknown-linux-musl
scripts/run-integration-tests.sh --test production_bundle -- --exact normal_bundle_adopts_native_v2_snapshot_grants_for_create_describe_and_restore
scripts/run-integration-tests.sh --test production_bundle -- --exact normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox
```

The canonical evidence map and deliberate Firecracker-byte/KVM-vector
differences are in the checked
[snapshot-editor state contract](../compat/firecracker/v1.16.0/snapshot-editor-contract.md).
These focused commands do not replace the full repository gate or the complete
no-skip signed wrapper.

### Native-v2 2.5 compatibility transaction failure matrix

Profile-2 certification treats “every stage” as the following finite public
transaction boundaries. It does not multiply pre-HVF failures across every
root/transport cell. Test names below are exact anchors in
[`vmm.rs`](../crates/bangbang/src/vmm.rs),
[`snapshot_restore_resources.rs`](../crates/bangbang/src/snapshot_restore_resources.rs),
[`startup.rs`](../crates/hvf/src/startup.rs), and the signed executable and
production targets. Retryable means the source or pristine destination remains
eligible only after complete reverse cleanup; terminal means the process may
not accept another load/create even when no final artifact escaped.

| Create boundary | Required classification | Exact anchors |
| --- | --- | --- |
| Product and output preflight | Unsupported profile, vhost-user, cancellation, and collision reject before pause, authority claim, or staging; fresh retry remains possible. | `native_v2_multi_block_product_profile_accepts_rootless_mixed_vector`, `storage_preflight_failure_precedes_contained_claim_and_publication`, `vhost_user_snapshot_rejects_before_session_barrier_or_artifact_staging`, `native_v2_pre_cancel_and_collision_do_not_pause_and_fresh_retry_succeeds` |
| Pause admission and auxiliary quiescence | Failure releases snapshot admission and recovers the original source before retry. | `boot_run_loop_supervisor_snapshot_scope_rejects_ordinary_commands_until_release`, `boot_run_loop_snapshot_barrier_recovers_after_auxiliary_quiescence_failure`, `boot_run_loop_supervisor_snapshot_error_releases_admission_once` |
| Block drain, platform capture, and memory streaming | A mutation-free capture/output failure is retryable only after source recovery; source damage or incomplete recovery is terminal and publishes nothing. | `native_v2_root_candidate_recovers_from_memory_write_failure_and_retries`, `native_v2_platform_terminal_policy_separates_source_damage_from_output_failure`, `native_v2_topology_and_recovery_failures_are_terminal_without_publication` |
| Graph composition, state encoding, and artifact staging | Graph/codec/write/sync/collision/cancellation failures clean all staging and recover the source before a fresh retry. | `native_v2_root_candidate_recovers_graph_and_closed_state_failures`, `native_v2_root_candidate_commit_cancellation_recovers_before_retry`, `native_v2_supervisor_recovers_after_cancellation_before_seal` |
| Panic and recovery | A panic first runs the same reverse recovery; incomplete or post-mutation recovery latches terminal and leaves no final pair. | `native_v2_panic_recovers_then_terminates_without_publication` |
| Visible memory-first/state-last commit | Once state is visibly committed the pair is success; later bookkeeping failure may terminate the source but must not retract or corrupt the pair. | `native_v2_visible_commit_survives_terminal_post_publication_failure` |
| End-to-end source recovery and recapture | A successful create returns the source Paused and permits resume or another complete capture. | `public_snapshot_create_publishes_native_v2_pair_without_legacy_barrier`, `native_v2_supervisor_publishes_loadable_pairs_and_recaptures_one_paused_source` |

| Load boundary | Required classification | Exact anchors |
| --- | --- | --- |
| State/memory inspection and profile dispatch | Unknown/corrupt/incomplete/legacy-mismatched input rejects before resource use or VM construction and keeps a pristine process retryable. | `native_v2_process_rejects_other_family_and_incomplete_profile_before_construction`, `public_native_v2_dispatch_retains_exact_minor_three_and_minor_four_profiles` |
| Complete request derivation and authority transaction | Missing, extra, swapped, aliased, wrong-access, wrong-role/kind, wrong-size, changed-geometry, consumed, or canceled claims reject as one vector before construction; reverse abort makes valid authority reusable and diagnostics remain redacted. | `profile_2_derives_one_exact_request_and_config_per_record`, `profile_2_contained_authority_failures_are_preconstruction_retryable_and_reusable`, `profile_2_direct_preflight_rejects_alias_geometry_grants_and_cancellation`, `profile_2_contained_batch_has_no_path_fallback_and_aborts_as_one_vector` |
| Async plan and aggregate process bundle | Partial preparation never publishes an owner; every prepared drive and fresh Async generation is released in reverse order. | `profile_2_process_bundle_failure_aborts_aggregate_completion`, `profile_2_process_bundle_retains_completion_until_explicit_commit` |
| VM and memory construction | Construction failure destroys the incomplete destination, aborts all drive completion, and remains retryable only when cleanup is complete. | `profile_2_destination_construction_failure_aborts_completion_and_runtime` |
| MMIO/PCI device, scheduler, and controller construction | Injected owner/transport/controller failures preserve exact retryable-versus-terminal disposition and retain cleanup failures without leaking identifiers or generations. | `profile_2_controller_failure_destroys_destination_before_completion_abort`, `native_v2_multi_block_mmio_errors_preserve_terminality_and_redact_cleanup`, `native_v2_multi_block_pci_errors_preserve_terminality_and_redact_cleanup` |
| Aggregate completion and Paused publication | Completion failure after irreversible resource commit destroys the destination and is terminal; successful completion publishes exactly one Paused session/controller. | `profile_2_completion_failure_destroys_destination_and_is_terminal`, `public_native_v2_load_commits_one_paused_session`, `native_v2_resource_adoption_failure_latches_all_process_construction_paths` |
| Optional resume | Resume occurs only after Paused publication; failure is terminal and the destination never returns to pristine eligibility. | `public_native_v2_load_resumes_only_after_paused_commit`, `native_v2_requested_resume_failure_latches_after_paused_publication` |
| Public signed continuation and process death | Rooted/rootless MMIO/PCI active I/O, recapture, immutable state/memory, shared writable backings, and exact worker-first/launcher-first cleanup are certified at the real direct and normal bundle boundaries. | `signed_executable_certifies_native_v2_multi_block_epochs_over_mmio_and_pci`, `normal_bundle_certifies_native_v2_multi_block_epochs_over_mmio_and_pci` |

### Native-v2 2.6 profile-3 pmem certification matrix

Profile 3 retains the complete profile-2 failure boundary and adds typed pmem
authority, mapping, and lifecycle terms. These exact anchors form the #1634
negative/fault ledger:

| Requirement | Exact anchors |
| --- | --- |
| Direct/contained complete-set derivation, missing authority, access, length, selector, and cancellation | `profile_3_contained_preflight_rejects_access_length_selector_and_cancellation`, `profile_3_direct_rejects_pmem_geometry_changes_and_cancellation_before_batch` |
| Extra, omitted, swapped, and cross-class aliased typed outputs | `profile_3_storage_batch_rejects_omitted_extra_and_swapped_typed_outputs_with_full_rollback`, `profile_3_direct_preflight_rejects_cross_class_alias_before_conversion`, `receiver_rejects_cross_class_descriptor_aliases_and_closes_the_whole_batch` |
| Wrong role/kind and no path fallback | `profile_3_contained_mode_rejects_wrong_pmem_role_atomically_without_path_fallback`, `snapshot_pmem_restore_returns_only_exact_regular_file_authority` |
| Preparation, construction, controller, completion, cancellation, and exact cleanup | `profile_3_destination_failures_and_drop_preserve_clean_retry`, `profile_3_plan_failure_precedes_backing_access_and_bundle_failure_aborts_completion`, `profile_3_completion_failure_destroys_destination_and_is_terminal`, `profile_3_runtime_cancellation_and_contained_abort_release_all_authority` |
| Signed owner capture/reconstruction | `capture_ready_storage_traverses_signed_mmio_and_pci_owners` |
| Direct rooted pmem-only/rootless mixed MMIO/PCI continuation | `signed_executable_certifies_native_v2_storage_epochs_over_mmio_and_pci` |
| Normal production/App Sandbox exact-grant continuation and death cleanup | `normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci` |

The two signed matrices use writable and read-only pmem backings whose exact
file length is 2 MiB plus one 16-KiB Apple-Silicon host page. The deterministic
guest accesses the aligned private tail through `/dev/pmem0` DAX, proving it is
zero for every fresh mapping while the writable external prefix advances
across destinations. They also require immutable state/memory, unchanged
read-only prefixes, limiter/retry and interrupt continuation, graph-stable
recapture, explicit/automatic resume, exact direct or contained ownership,
redaction, and staging/session/grant cleanup.

### Native-v2 2.7 serial certification matrix

Exact 2.7 retains the profile-3 storage failure boundary and adds one required
serial singleton plus a configured-output authority class:

| Requirement | Exact anchors |
| --- | --- |
| Canonical bytes, exact version/profile, bounded selector/RX, every header/reserved/length mutation, complete UART cross-fields, allocation, and redaction | `canonical_default_and_configured_fixtures_round_trip`, `exact_version_and_endpoint_profile_fail_closed`, `limiter_presence_and_complete_uart_semantics_are_rejected_when_inconsistent`, `every_owned_decode_and_encode_allocation_is_fallible`, `debug_output_redacts_endpoint_and_device_values` |
| Default/configured direct and contained endpoint preparation, terminal/FIFO lifetime, fresh limiter/metrics, missing/extra output, cancellation, and repeated cleanup | `default_fifo_endpoints_preserve_uart_and_start_fresh_limiter_and_metrics`, `default_terminal_is_raw_until_explicit_abort_then_fully_restored`, `configured_output_skips_stdio_and_commits_through_explicit_lifecycle`, `contained_configured_output_is_output_only_and_rebinds_after_abort`, `construction_failure_and_repeated_bundles_restore_shared_stdio_lifetime` |
| Complete storage-plus-serial authority, alias/role/access/reference/cancellation rejection, rollback/reuse, construction/controller/completion/cleanup failure | `exact_2_7_mixed_contained_storage_and_serial_share_one_rollback`, `exact_2_7_serial_preflight_rejects_aliases_authority_crossing_and_cancellation`, `storage_and_serial_owners_remain_one_graph_ordered_abort_lifetime`, `stdio_failure_and_post_endpoint_cancellation_are_retryable_after_cleanup`, `retained_output_clone_surfaces_split_lifetime_cleanup_failure`, `exact_v2_serial_commit_classifies_missing_projection_as_terminal` |
| Private signed HVF reconstruction and recapture | `signed_native_v2_serial_destination_reconciles_and_recaptures_private_owner`, `signed_native_v2_serial_storage_destination_preserves_mmio_and_pci_owners` |
| Direct signed public continuation | `signed_executable_certifies_native_v2_serial_continuation_over_fresh_stdio`, `signed_executable_reopens_configured_serial_snapshot_file_and_fifo_destinations` |
| Normal production/App Sandbox continuation and containment | `normal_bundle_certifies_native_v2_serial_snapshot_continuation_and_containment` |

The shared bare-arm64 image programs nondefault valid UART registers, retains a
full 64-byte prefix, and leaves a distinct 40-byte suffix in the source pipe.
After source termination, the destination must supply a different 40-byte
suffix. Guest checks and destination metrics therefore distinguish serialized
UART bytes from forbidden inherited host-pipe bytes. The matrix covers
serial-only, MMIO-storage, PCI-storage, explicit/automatic resume, paused
recapture, immutable pair reuse, default FIFO/pipe stdio, direct configured
regular-file/FIFO replacement, contained write-only output grants, TX, EOF,
redaction, and teardown. Destination terminal raw-mode/restoration remains a
focused test because the signed harness supplies pipes rather than a PTY.

### Native-v2 2.8 entropy certification matrix

Exact 2.8 retains the complete 2.7 serial and optional profile-3 storage
boundaries and adds one optional entropy singleton. These anchors certify its
codec, public process transaction, fresh destination ownership, and
host/guest-visible continuation:

| Requirement | Exact anchors |
| --- | --- |
| Canonical bytes, exact optionality/version/placement, queue and dual-bucket state, pending/retry invariants, every fixed field and hostile length/count/value, allocation, and redaction | `inactive_mmio_and_active_pci_round_trip_canonically`, `exact_outer_version_is_required`, `header_directory_and_complete_bounds_fail_closed`, `local_presence_retry_and_cursor_mutations_fail_closed`, `local_bucket_presence_and_pending_matrix_fails_closed`, `common_virtio_hostile_fields_fail_closed`, `mmio_and_pci_transport_hostile_fields_fail_closed`, `allocation_failures_return_no_partial_value`, `diagnostics_redact_entropy_values_and_placement` |
| Product graph selection, cross-graph rejection, public create/load staging, resource transaction failures, rollback/reuse, and current-version routing | `internal_exact_minor_eight_entropy_compositions_encode_nested_versions_canonically`, `exact_minor_eight_composition_rejects_transport_and_placement_conflicts`, `exact_minor_eight_decoder_rejects_malformed_profile_and_nested_entropy`, `native_v2_process_preflight_routes_exact_minor_eight_profile_to_hvf_decode`, `native_v2_entropy_candidate_profile_admits_all_products_and_both_transports`, `native_v2_entropy_capture_failures_recover_release_guards_and_remain_reusable`, `public_native_v2_process_publication_is_loadable_and_repeatable` |
| Signed fresh MMIO/PCI source, scheduler, notifier, route, endpoint, limiter, metrics, reconstruction, and recapture | `restores_signed_serial_entropy_mmio_owners_with_exact_retry_semantics`, `restores_signed_serial_entropy_pci_owners_with_exact_retry_semantics`, `restores_signed_storage_serial_entropy_mmio_owner_graph`, `restores_signed_storage_serial_entropy_pci_owner_graph` |
| Direct signed public continuation | `signed_executable_certifies_native_v2_entropy_snapshot_continuation` |
| Normal production/App Sandbox continuation and containment | `normal_bundle_certifies_native_v2_entropy_snapshot_continuation_and_containment` |

The deterministic `/snapshot-entropy-init` guest binds `virtio_rng`, consumes
one exact 64-byte `/dev/hwrng` prefix, publishes readiness, and leaves a second
64-byte read retained behind the one-operation and byte limiters. The source is
then terminated. A fresh destination must decode the exact pending
queue/limiter/retry state and complete that read after refill without another
guest kick. The matrix covers entropy-only and profile-3 storage-plus-entropy
graphs over MMIO and PCI, explicit and automatic resume, paused recapture,
immutable pair reuse, exact fresh entropy source construction, fresh
scheduler/notifier/route/endpoint and metrics owners, source-path replacement,
malformed-state rejection, graceful cancellation, both launcher/worker death
orders, redaction, and staging/session/socket cleanup. It does not claim
persisted host randomness, migration of an OS entropy descriptor, bit-identical
random output, or Firecracker snapshot compatibility.

Native snapshot commit/publication tests pin the fixed 32-byte `BANGCMT\0`
record, preserve kind-1 bytes exactly, and pin kind 2's exact nested binding,
non-empty backend state, and envelope composition. They must reject every
length, schema, kind, flags, kind-specific state-length, nested-binding, outer-
envelope, and trailing-data failure without leaking identities, checksums,
paths, or bytes.
Artifact tests run on macOS and cover same- and cross-directory success,
owner-only staging modes, exact and opened-parent aliases, case-equivalent volume
behavior, pre-existing regular/directory/FIFO/socket/symlink entries, missing and
unwritable parents, every ordered write/sync/publish failure, late final-name
collisions, observed staging replacement, cleanup failure precedence,
memory-only orphans, committed-but-durability-uncertain state, state-first
loading, bounded/nonregular inputs, swapped/truncated/extended/corrupt pairs,
diagnostic redaction, and coordinated multiprocess contention with exactly one
durable winner. Generic-producer coverage additionally proves both staging
entries precede one callback, earlier failures skip it, ordinary drop and
explicit close satisfy the close proof, retained/forgotten/error-owned writers
never publish, panic and typed producer failure clean staging and permit retry,
and short/extra/wrong-identity/wrong-length/wrong-trailer output cannot commit.
Native-family coverage additionally proves that the closed v2 value derives its
binding from the exact state bytes, current-version publication cannot be
bypassed with a compatible-reader value, both family adapters traverse the same
injected transaction stages, and unrelated v2 state/image identities fail
before publication. Direct and already-opened v1/v2 loads preserve descriptor
identity after path replacement; v2 requires read-only/CLOEXEC input, retains
private File/COW regions, isolates repeated-load writes, and leaves source bytes
unchanged. The v2 staging verifier separately covers exact position, length,
header, zero padding, and read-write staging acceptance without weakening the
final loader policy. The lightweight verifiers are not substitutes for loader
CRC/GPA validation.
Failure hooks may prove an observed replacement is refused, but must not claim
atomic source identity against a hostile directory writer. Run the focused surface with
`cargo test -p bangbang-runtime snapshot_artifact --locked`.

Native-v1 device-profile tests pin the fixed `BANGDEV\0` header and an exact
active/inactive schema shape under the 16 KiB cap. Cover transport status and
feature mismatches, queue mapping/non-overlap/cursor wraparound, drained
notifications, interrupt bits, limiter budget/burst/age with injected
`Instant`s, retry eligibility, UART register round trips, canonical
VMGenID/VMClock metadata, exact EOF, bounded UTF-8 fields, and diagnostic
redaction. Filesystem preflight tests must use real regular files, symlinks,
directories, FIFOs, sockets, replacements, and metadata/length changes; prove
that the retained descriptor is read-only/no-follow and that every failed
preflight leaves guest memory and the MMIO dispatcher untouched. Run the
focused codec/preflight surface with
`cargo test -p bangbang-runtime snapshot_device --locked`; retry-scheduler
snapshot tests belong in `cargo test -p bangbang-hvf limiter_retry_snapshot
--lib --locked` and must not sleep.

Native-HVF composite tests pin the `BANGHVF\0` header, five required component
headers/order, deterministic complete round trip, and nested `BANGDEV\0` bytes.
They reject missing, duplicate, reordered, unknown, flagged, empty, truncated,
oversized, trailing, and cross-component-inconsistent values. Cross-validation
must cover machine/binding memory size and ranges, MPIDR, optional-feature
policy, baseline GIC topology and blob budget, fixed PL031 mapping/fresh policy,
and device queue/platform ranges. Unique sentinels in registers, PAC keys,
paths, image identity/checksums, and GIC bytes must remain absent from `Debug`,
`Display`, and errors. Run the focused codec with
`cargo test -p bangbang-hvf snapshot_bundle --lib --locked`.

The aggregate runner test records the exact native-v1 capture order, injects a
failure at every stage, and proves a complete fresh retry. It must exercise
metadata/core/timer/interrupt conflicts in both directions and exactly-once
release after response abandonment, channel closure, queued destruction,
unwind, panic, and shutdown. The process-level fake capture session proves the
outer order from four-scheduler auxiliary quiescence through state preflight,
chunked memory, bundle construction, writer drop, artifact verification and
commit, the successful-publication hook, auxiliary release, and admission
release. It also proves pre-seal cancellation emits no commit, leaves `Paused`,
and permits a fresh capture and resume; post-seal shutdown preserves the exact
publisher success or visibility error. Process/supervisor publication tests
additionally prove path/profile preflight before content capture, direct and
anchored move-only staging publication, required kind 2, writer closure before
commit, cancellation cleanup and fresh retry, terminal worker panic, unchanged
paused controller state, public create publication/collision behavior, public
load paused/resume ordering, and retryable versus terminal failures. A real API
loop test queues MMDS and controller mutations and advances a short periodic
metrics interval while snapshot publication is blocked: none can enter until
release, while the shared atomic cancellation source remains observable out of
band. Run these focused surfaces with
`cargo test -p bangbang-hvf snapshot_v1 --lib --locked` and
`cargo test -p bangbang native_v1_ --locked`.

For process, socket, and multi-bangbang behavior, cover unique resource names,
stale socket handling, shutdown cleanup, replacement races, and concurrent runs
where practical.

For periodic process behavior, test scheduler and timeout paths directly. Do
not wait for real production intervals such as the 60-second metrics flush
period.

For HVF and FFI code, cover resource creation and destruction, platform gating,
error translation, unsupported exits or registers, cancellation, and cleanup
after partial setup failure.
For owner-thread aggregate captures, also cover exact field order, every read
failure and retry, forward and reverse admission conflicts, caller abandonment,
closed command and response channels, queued-command destruction, panic, and
shutdown. Pending-interrupt restore tests must verify IRQ-then-FIQ writes,
both failure positions, exact value-free failed-type/completed-prefix/source
context, complete retry, generalized interrupt-operation conflicts, and every
lifecycle cleanup path. Signed coverage must retain IRQ-only, mutate to
FIQ-only, restore/recapture the complete IRQ-only value twice through fixed
messages, then clear and recapture both levels before shutdown. No guest run may
intervene because HVF would clear the injection levels and invalidate the raw
round trip. Equality proves neither GIC/device composition, delivery/EOI,
automatic per-run reassertion, persistence, nor portable snapshot restore.
Opaque GIC device-state restore tests must verify exact non-null pointer and
`usize`/`size_t` propagation, empty-input rejection without a setter call,
unchanged HVF error provenance, the sticky never-run gate, generalized
interrupt-operation conflicts, caller abandonment, closed channels, queued
destruction, panic, and exactly-once admission cleanup. A setter failure has no
documented rollback or retry guarantee; tests may prove cleanup and shutdown but
must not execute the VM afterwards. Signed coverage must create the GIC and vCPU,
capture a non-empty original blob, reapply that exact value before any run, and
then destroy the VM without parsing, comparing, mutating, or logging opaque
bytes. Both prepared boot-session forms must cover the same pre-run delegate.
GIC ICC capture tests must create the GIC before the vCPU, write architecturally
writable EL1 ICC values from signed guest code, and assert only fields or masked
bits whose readback is stable. Restore unit tests must cover the exact ten-
position sequence of nine mutable writes and a derived RPR read, every write
failure, RPR read failure and mismatch, value-free typed context, full retry,
the sticky never-run gate, shared interrupt conflicts, abandonment, channels,
queued destruction, panic, and cleanup. Signed restore coverage must capture an
idle same-VM opaque blob and ICC value, reapply the blob first, restore the ICC
value, and prove two exact recaptures without guest execution or value logging;
both boot-session delegates must cover the same order. Read-only active-priority
values remain host-defined and must never be passed to the setter.
General-register restore unit tests must verify X0-X30/PC/CPSR write order,
every one of the 33 failure positions, exact failed-register and completed-
write context, complete retry, shared core-operation conflicts, abandonment,
closed channels, queued-command destruction, panic, and cleanup. Signed tests
must restore only a complete capture from the same idle real vCPU, recapture
and compare it with fixed failure messages, and repeat the round trip without
guest execution or logging register values. A failed restore is
nontransactional; tests and callers must retry the complete retained value or
discard the vCPU before any run.
Core system-register restore tests must likewise verify
`SP_EL0`/`SP_EL1`/`ELR_EL1`/`SPSR_EL1` capture-order writes, all four failure
positions, reusable system-register error context, complete retry, 34-way
admission, and lifecycle cleanup. Signed coverage must extend the known-value
guest-written capture with repeated same-vCPU restore/recapture after the HVC
exit, use fixed failure messages that do not format raw state, and never run the
guest after restore or claim the values are portable or validated.
Translation-register restore tests must verify SCTLR_EL1-then-TTBR0_EL1-then-
TTBR1_EL1-then-TCR_EL1-then-MAIR_EL1-then-AMAIR_EL1-then-CONTEXTIDR_EL1
writes, all seven failure positions, the reusable system-register error,
complete retry, 34-way admission, and lifecycle cleanup. Signed coverage must
leave `SCTLR_EL1.M` clear, write back the original SCTLR value before inert
TTBR/TCR/attribute/context values and HVC, then repeat same-vCPU
restore/recapture with fixed messages and no post-restore guest execution.
AMAIR is implementation-defined: preserve the actual captured readback instead
of assuming the guest-written value is writable. The round trip proves only raw
field reapplication, not table-memory capture, validation, barriers,
TLB/cache maintenance, or a safe MMU transition sequence.
Exception-register restore tests must verify
`AFSR0_EL1`/`AFSR1_EL1`/`ESR_EL1`/`FAR_EL1`/`PAR_EL1`/`VBAR_EL1`
capture-order writes, all six failure positions, the reusable system-register
error, complete retry, 34-way admission, and lifecycle cleanup. Signed coverage
must use an aligned VBAR address, preserve the actual captured AFSR readback,
repeat same-vCPU restore/recapture with fixed messages, take no guest exception
or run after restore, and never claim coherent exception semantics or
vector-table memory. AFSR contents are implementation-defined: current Apple
Silicon reads AFSR0 as zero after a guest write while preserving the test's
AFSR1 value.
Execution-control restore tests require macOS 15 for ACTLR and must verify
ACTLR-then-CPACR writes, both failure positions, the reusable system-register
error, complete retry, 34-way admission, and lifecycle cleanup. Signed coverage
must write only the Hypervisor.framework-supported `ACTLR_EL1.EnTSO` bit and
baseline `CPACR_EL1.FPEN`, execute ISB before HVC, then repeat same-vCPU
restore/recapture with fixed messages and no post-restore guest execution. It
must not treat equality as destination feature validation or a complete
transition/ISB policy.
Cache-selection restore tests must verify the single `CSSELR_EL1` write, the
one failure with zero completed writes, the value-free reusable system-register
error, complete retry, 34-way admission, and lifecycle cleanup. Signed coverage
must restore and recapture the first complete same-vCPU idle capture twice with
fixed whole-state messages and no selector logging, CCSIDR query, ISB, cache
maintenance, or guest run. It must not treat equality as selector validation,
an atomic cache manifest, destination compatibility, dependent-read ordering,
or portable snapshot restore.
Thread-context restore tests must verify TPIDR_EL0-then-TPIDRRO_EL0-then-
TPIDR_EL1 writes, all three failure positions, the reusable system-register
error, complete retry, 34-way admission, and lifecycle cleanup. Signed coverage
must extend the known guest-written values with repeated same-vCPU
restore/recapture after HVC, use fixed messages, take no post-restore guest run,
and never claim pointer validation, portability, or complete context semantics.
System-context restore tests must verify SCXTNUM_EL0-then-SCXTNUM_EL1 writes,
both failure positions, the value-free reusable system-register error,
complete retry, 34-way admission, redacted `Debug`, and lifecycle cleanup.
Signed coverage must restore and recapture the first complete same-vCPU idle
capture twice with fixed messages, take no guest run, log no raw values, and
never claim interpretation, feature/destination validation, protected
persistence, wider TPIDR/CONTEXTIDR ordering, rollback, or snapshot semantics.
Pointer-authentication key restore tests must verify APIA low/high, APIB
low/high, APDA low/high, APDB low/high, then APGA low/high writes; all ten
failure positions; value-free reusable system-register error context; complete
retry; 34-way admission; redacted `Debug`; and lifecycle cleanup. Signed
coverage must use only the existing visibly fake guest-written keys, repeat
same-vCPU restore/recapture twice after HVC with fixed whole-state messages,
never enable or execute PAC, never run the guest after restore, and never log
key material or claim feature/destination validation, protected persistence,
zeroization, SCTLR enable ordering, rollback, or portable snapshot semantics.
Baseline SIMD/FP restore tests must verify Q0-through-Q31-then-FPCR-then-FPSR
writes, all 34 failure positions, the typed SIMD/FP-versus-scalar register and
completed-prefix context, complete retry, 34-way admission, and lifecycle
cleanup. The C shim must compile only for macOS arm64, statically assert the SDK
vector size, and accept an ordinary 16-byte pointer so stable Rust never guesses
the by-value vector ABI. Signed coverage must extend the known non-streaming
guest-written capture with repeated same-vCPU restore/recapture after HVC, fixed
whole-state messages, and no post-restore guest run. It must not log Q bytes or
claim feature/destination validation, FPCR/FPSR writable-bit policy, SVE/SME Q/Z
alias ordering, rollback, or portable snapshot semantics.
Identification-register signed tests must capture all eleven stable baseline
values twice within one vCPU lifetime and compare MPIDR with the existing
owner-thread getter. They must not hard-code one Apple MIDR/feature model,
include availability-gated or beta-only IDs, or claim that equal raw values are
a sufficient destination compatibility policy.
CPU-template tests add a separate mutation boundary. Unit/failure-injection
coverage must prove exact core identity/width classification, boot-reserved and
banked-state rejection, all eleven ID mappings, every forbidden ACTLR bit, the
macOS 15.2 available/unavailable outcomes, every KVM class, every named
public-HVF safety family, aliases, unnamed encodings, invalid class fields,
mixed U32/U64/U128 input order, explicit little-endian Q conversion, and
fail-closed U32 scalar transport. They must prove that every requested typed
baseline on every member precedes the first write, unrelated allowlisted
identities are untouched, cross-vCPU baseline/width mismatch performs no
writes, targets are computed once, and every write is immediately reread. Every
failure position must retain only redacted member/completed-count context and
destroy an unpublished startup topology. Signed lifecycle coverage first
captures a disposable in-memory host baseline, then applies all seven new ID
registers and ACTLR.EnTSO as part of one mixed ID/X/core/Q/FP custom template to
two fresh real HVF vCPUs. It must compare the retained typed state without
formatting raw values, prove X0/PC/PSTATE boot precedence, and shut down both
sessions cleanly. Startup success includes mandatory exact readback on both
owners. The signed two-vCPU Linux SMP path must apply
boot-owned X0/PC/PSTATE modifiers and reach userspace on the PSCI-started
secondary, proving that secondary boot setup supersedes them. Signed Linux
ID-register coverage must
boot separate two-vCPU baseline and custom sessions, online and pin a no-stdlib
EL0 reporter to each CPU, write bounded raw reports only to a scratch block
device, and verify `custom == (baseline & !filter) | value` for every CPU and
register without requiring a bit to change. Serial output may contain only
fixed success/failure markers, never report values. Run both through
`scripts/run-integration-tests.sh` without `--allow-unsupported`; the direct
rootfs builder requires the installed stable `aarch64-unknown-linux-musl`
target and embeds the deterministic static helper.
SVE/SME identification signed tests require macOS 15.2 and must capture ZFR0
and SMFR0 twice from one idle real vCPU. They may assert same-vCPU stability but
must not hard-code one feature model, enable SVE/SME, enter streaming mode,
read vector/predicate/matrix state, run the vCPU, or treat equality as a
destination compatibility policy.
SME configuration signed tests require macOS 15.2 and must query the maximum
guest-usable SVL twice before creating a backend or VM. They may compare two
successful same-host values without formatting or logging the byte length, or
accept two exact raw `HV_UNSUPPORTED` results. A missing symbol, mixed result,
or unrelated error must fail. Tests must not infer an effective `SMCR_EL1.LEN`,
create or run a vCPU, change PSTATE or `SMCR_EL1`, read Z/P/ZA/ZT0 contents, or
treat stability as feature or destination compatibility policy.
SME PSTATE signed tests must runtime-resolve the macOS 15.2 getter and call it
twice on one idle real vCPU. SME-capable hosts may compare same-vCPU results but
must not assume or log `PSTATE.SM` or `PSTATE.ZA`. A missing pre-15.2 symbol or
the getter's exact raw `HV_UNSUPPORTED` result may be treated as documented
unavailability; every unrelated error must fail. Tests must not call the setter,
change PSTATE, query maximum SVL, read Z/P/ZA/ZT0, run guest code, or treat the
flags as complete or safely restorable SME state.
SME Z-register signed tests must runtime-resolve the macOS 15.2 getter and may
read Z0-Z31 only when an owner-thread `PSTATE.SM` preflight reports streaming
mode active. They may accept the documented missing-symbol or exact
`HV_UNSUPPORTED` boundaries, the topical inactive-streaming result, or compare
two complete same-vCPU captures. Successful captures must use the separately
queried maximum SVL as the exact width of every bounded accessor and verify
redacted `Debug` output without formatting or logging bytes or width. Tests must
not call any SME setter, enter streaming mode, run guest code, infer effective
`SMCR_EL1.LEN`, or treat equal bytes as portable or safely restorable state.
SME P-register signed tests must runtime-resolve the macOS 15.2 getter and may
read P0-P15 only when an owner-thread `PSTATE.SM` preflight reports streaming
mode active. They may accept the documented missing-symbol or exact
`HV_UNSUPPORTED` boundaries, the topical inactive-streaming result, or compare
two complete same-vCPU captures. Successful captures must preserve the
separately queried maximum SVL, use exactly one eighth of it for every bounded
predicate accessor, and verify redacted `Debug` output without formatting or
logging bytes or widths. Tests must not call any SME setter, enter streaming
mode, run guest code, infer effective `SMCR_EL1.LEN`, or treat equal predicates
as portable or safely restorable state.
SME ZA-register signed tests must runtime-resolve the macOS 15.2 getter and may
read the matrix only when an owner-thread `PSTATE.ZA` preflight reports storage
enabled; streaming mode is not a prerequisite. They may accept the documented
missing-symbol or exact `HV_UNSUPPORTED` boundaries, the topical inactive-ZA
result, or compare two complete same-vCPU captures. Successful captures must
preserve the separately queried maximum SVL, use its exact checked square as
the raw byte length, and verify redacted `Debug` output without formatting or
logging bytes or dimensions. Tests must not call an SME setter, enable ZA or
streaming mode, run guest code, infer row/tile or effective-SVL semantics, or
treat equal matrices as portable or safely restorable state.
SME ZT0-register signed tests must runtime-resolve the macOS 15.2 getter and may
read the fixed 64-byte register only when an owner-thread `PSTATE.ZA` preflight
reports storage enabled; streaming mode and maximum SVL are not prerequisites.
They may accept the documented missing-symbol or exact `HV_UNSUPPORTED`
boundaries, the topical inactive-ZA result, or compare two complete same-vCPU
captures. Successful captures must preserve exactly 64 bytes and verify fully
redacted `Debug` output without formatting or logging raw bytes. Tests must not
call an SME setter, enable ZA or streaming mode, run guest code, infer SME2
feature/destination or lane semantics, or treat equal bytes as portable or
safely restorable state.
SME system-register signed tests require macOS 15.2 and must capture `SMCR_EL1`,
`SMPRI_EL1`, and `TPIDR2_EL0` twice from one idle real vCPU. They may compare
same-vCPU results only with fixed failure messages and must verify that `Debug`
redacts all raw values. They must not log or format those values, write the
registers, query maximum SVL, read Z/P/ZA/ZT0, run guest code, or treat stable
readback as a portable or safely restorable SME state.
System-context signed tests require macOS 15.2 and must capture `SCXTNUM_EL0`
and `SCXTNUM_EL1` twice from one idle real vCPU. They may compare same-vCPU
results only with fixed failure messages and must verify that `Debug` redacts
both raw values. They must then restore and recapture the complete first value
twice without a guest run. They must not log or format those values, hard-code
reset values, or treat the raw round trip as interpretation, feature/destination
compatibility, wider context ordering, or portable snapshot restore.
Default vCPU cache-configuration signed tests must query CTR_EL0, CLIDR_EL1,
and DCZID_EL0 twice before creating a backend or VM. They may compare same-host
values only through fixed failure messages and must not format or log raw
registers. Tests must not create or run a vCPU, read or write `CSSELR_EL1`,
query instruction/data CCSIDR values, perform cache maintenance, or treat the
triple as a complete cache topology or destination-compatibility policy.
Default vCPU cache-geometry signed tests must query all eight data/unified and
all eight instruction CCSIDR values twice before creating a backend or VM. They
may compare same-host arrays only through fixed failure messages and must not
format or log raw values. Tests must not create or run a vCPU, read or write
`CSSELR_EL1`, use the live system-register CCSIDR path, issue ISB, perform cache
maintenance, assume which array entries describe implemented levels, combine
the result atomically with the feature triple, or infer topology or destination
compatibility.
Arm64 cache-presentation unit tests must keep the combined startup source and
the public host facts independently injectable. Cover both legacy and CCIDX
CCSIDR layouts, inactive slots, every checked reserved/overflow field,
CTR/DCZID consistency, supported and rejected CLIDR shapes, unique performance-
level selection, missing/mismatched/ambiguous facts, nested sharing, and vCPU
counts through 32 without treating host physical cores as an admission cap.
The real sysctl boundary must prove the 32-bit widths used by the public
performance-level selectors and accept the platform's `ENOENT` or `EINVAL`
result only as absence for optional selectors. Failure tests must assert that
cache admission precedes VM/GIC creation and guest-memory mapping and that raw
registers, sysctl values, and underlying host diagnostics are absent from
`Debug` and public errors.

FDT tests must parse emitted blobs rather than compare only builder calls. They
must verify exact L1 properties on each CPU, deterministic outer-cache node
names/phandles, direct `next-level-cache` edges, one-CPU and partial final
sharing groups, nested L2/L3 topology, and rejection of malformed geometry or
graphs. Signed Linux cache proof uses the normal production startup path,
mounts the existing sysfs, boots initially with `maxcpus=1`, explicitly onlines
CPU1, and writes one bounded normalized cache report to a scratch block device
with `conv=fsync`. The host compares Linux level/type/size/line/sets/ways and
shared CPU lists to the retained hierarchy. Serial output is only a fixed
success/failure marker; neither raw host facts nor the report belongs there.
Cache-selection signed tests must capture CSSELR_EL1 twice from an idle real
vCPU without hard-coding or validating its architecturally unknown reset value,
then restore and recapture the first complete value twice through fixed
whole-state messages. They must not log the selector, query CCSIDR, execute ISB
or cache maintenance, run guest code, or treat raw same-vCPU equality as cache
topology, an atomic manifest, destination compatibility, dependent-read
ordering, or portable snapshot restore.
Hardware-breakpoint signed tests must read `ID_AA64DFR0_EL1.BRPs`, capture only
the reported 1–16 `DBGBVR<n>_EL1` / `DBGBCR<n>_EL1` pairs from an idle real
vCPU, and assert shape rather than reset values. They must not log raw values,
write debug registers, enable breakpoints or monitor debug, change HVF debug-
register trap policy, execute guest/debug instructions, run the vCPU, or treat
the raw controls as safe restore input.
Hardware-watchpoint signed tests must read `ID_AA64DFR0_EL1.WRPs`, capture only
the reported 1–16 `DBGWVR<n>_EL1` / `DBGWCR<n>_EL1` pairs from an idle real
vCPU, and assert shape rather than reset values. They must not log raw values,
write debug registers, enable watchpoints or monitor debug, change HVF debug-
register trap policy, execute guest/debug instructions, run the vCPU, or treat
the raw address and control values as safe restore input.
Debug-control restore tests must verify MDCCINT_EL1-then-MDSCR_EL1 writes, both
failure positions, the reusable typed failed register and completed prefix,
value-free errors, complete retry, 34-way admission, and lifecycle cleanup.
Signed tests must capture the original pair from one idle real vCPU, restore
and recapture that exact pair twice, and compare whole values without assuming
or logging either register. They must not manufacture active debug controls,
alter comparator or host trap policy, run the vCPU, execute guest/debug
instructions, or treat raw same-vCPU equality as feature/writable-bit or
destination validation, complete debug state, or portable snapshot restore.
Debug-trap restore tests must verify debug-exception-then-debug-register-access
writes, both failure positions, the typed failed operation and completed
prefix, value-free errors, complete retry, 34-way admission, and lifecycle
cleanup. Signed tests must capture the original pair from one idle real vCPU,
restore and recapture that exact pair twice, and compare whole values without
assuming or logging either Boolean. They must not manufacture a policy change,
run the vCPU, execute guest/debug instructions, alter guest controls or
comparators, activate debug behavior, or treat host TDE/TDA-equivalent policy
as guest register state or a complete portable debug-restore configuration.
Physical-timer signed tests require macOS 15 and must create the GIC before the
vCPU. Guest-written validation must keep CNTP disabled and masked and assert
writable control bits separately from derived ISTATUS. No test may claim that
an absolute CVAL or relative TVAL can be restored without elapsed-time and
interrupt-delivery policy. TVAL-only validation must use an idle vCPU with no
guest execution or timer writes, may only prove that capture and the raw
accessor succeed, and must not log, format, compare, narrow, sign-extend, assume
reset state, or assert an exact relationship with the separately timed CVAL
read.
Normalized timer policy tests are separate from those raw-capture rules. Unit
tests must pin wrapping virtual-count/physical-distance arithmetic, strip
ISTATUS, ignore TVAL as a restore source, reject unknown controls, preflight all
eight destination fields plus the counter before writing, exercise every one of
the ten write failures and completed prefix, and prove a full retry takes a new
counter sample. Runner tests must include every admission conflict, abandoned
responses, cleanup, and sticky rejection after a failed run attempt. Signed
coverage must destroy the source VM, create the destination GIC before its fresh
vCPU, restore before any run, and compare stable fields plus the invariant that
virtual-count advance equals physical-distance decrease. Disabled and armed
masked writable controls must both be exercised without comparing ISTATUS or
TVAL and without running a partially restored destination.

VMGenID replacement tests must inject deterministic candidates for random
failure, all-zero and retained-value normalization, exact 16-byte guest writes,
metadata commit-after-write, retry, signal ordering, and redaction. Signed
borrowed and owned boot-session coverage must prove the retained value and
guest buffer change together and that the real edge-rising SPI injection
succeeds before first run. A signal failure is a post-commit partial result,
not a rollback assertion.
VMClock tests must pin all 112 ABI bytes and field offsets, valid enumerations,
required/unknown flags, padding, even capture sequence, wrapping disruption and
generation counters, and the odd/release/counters/release/even publication
order. Failure injection must distinguish a mutation-free first-write failure
from every committed prefix and from post-update SPI failure. Native-v1 codec
tests must cover exact `BANGDEV\0` 1.1.0 state, legacy 1.0.0 recovery from guest
memory, encoded-memory disagreement, malformed ABI, and trailing data. Aggregate
restore tests must prove VMGenID write/notification precedes VMClock
update/notification after runner/GIC restore, and that any later fault is
terminal even after complete cleanup. PL031 load tests must bound the data
register by destination wall clock and assert every unsupported alarm register
is clear.
Pointer-authentication key signed tests must use visibly non-secret sentinels,
must not enable or execute PAC instructions, and must assert that debug output
contains no raw key material. Failure assertions must not format actual key
values. Restore and recapture the same complete value twice after the guest HVC
without another run, then destroy the VM; treat equality only as raw same-vCPU
setter coverage, never as feature compatibility, protected persistence,
zeroization, SCTLR ordering, or a safe snapshot restore round trip.

## Stability Rules

Avoid arbitrary sleeps, fixed polling delays, and timeout increases that hide
races. Prefer explicit state, bounded channels, owned handles, temporary
directories, and public completion signals.

Tests must not share fixed global paths. Use unique temporary files or
directories and verify cleanup when ownership matters. Multiple tests and
multiple `bangbang` processes should not interfere unless the test is explicitly
checking conflict behavior.

Do not ignore HVF tests on hosts that support HVF. If an HVF test cannot run on
hosted CI, use the signed integration runner with `--allow-unsupported` so CI
still validates artifact preparation, compilation, and signing before skipping
execution on unsupported runners.

## Firecracker Capability Inventory

The checked
[Firecracker v1.16.0 capability inventory](../compat/firecracker/v1.16.0/README.md)
is validated by a dedicated non-published workspace tool. Run its focused tests
and delivery-time validation when changing the manifest, overlay, validator,
evidence ledgers, or any Firecracker-facing capability:

```sh
cargo test -p bangbang-firecracker-capability-audit --all-targets --locked
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
```

The workspace suite also validates all four checked JSON files: the general
source manifest and capability overlay plus the logger producer manifest and
human audit. Ordinary validation uses only the pinned checked-in inventory and
does not discover or require a sibling Firecracker checkout.

### Evidence Responsibilities

A Firecracker-facing change must update every affected record in
[`capabilities.json`](../compat/firecracker/v1.16.0/capabilities.json) and its
owner contract. The
[compatibility scope](firecracker-compatibility.md) owns observable API, CLI,
field, and runtime behavior. The
[validation matrix](firecracker-validation-matrix.md) is only the compact
current-state index. Contract ownership is listed in the
[inventory guide](../compat/firecracker/v1.16.0/README.md#contract-index).

Terminal evidence must match the exact claim:

- parser and schema tests prove recognition, not backend behavior;
- a stable unsupported response is not implementation;
- aggregate capabilities require aggregate validation rather than a collection
  of unrelated leaf tests;
- signed HVF or App Sandbox evidence must run through the repository wrapper;
- a corpus reference records audit ownership and does not by itself prove every
  statement in that corpus; and
- platform-impossible claims require the upstream contract, authoritative
  platform evidence, rejected alternatives, stable product behavior, focused
  tests, compatibility/security documentation, and a trusted Challenge result.

The validator is authoritative for current global disposition totals. Contract
documents may retain selected-family counts only when a focused test uses them
as a closure invariant; do not copy global totals or delivery chronology into
prose.

### Wave 7 core API and ownership evidence

The checked
[Wave 7 ownership and specification contract](../compat/firecracker/v1.16.0/observability-tools-specification-contract.md)
pins the exact #1491-owned set, retained handoffs, core API terminal set, and
x86 CPUID/MSR exclusions. Changes to that boundary must run the focused ledger,
strict full-shape request, and real-process nonmutation tests:

```sh
cargo test -p bangbang-firecracker-capability-audit --test checked_inventory wave_7_ownership_and_core_api_policy_is_stable --locked
cargo test -p bangbang-api rejects_complete_x86_cpu_config_shapes_without_retaining_values --locked
cargo test -p bangbang --test process_e2e executable_configures_vm_before_start --locked
```

The API test submits complete CPUID-only, MSR-only, and combined x86 shapes and
requires one fixed value-free malformed result. The process case sends the same
classes through the real Unix socket and proves the process survives, the
instance remains `Not started`, and `GET /vm/config` is unchanged. The ledger
test mechanically prevents dropped or double-owned rows and prevents the core
API schema from absorbing the independently owned logger, metrics, performance,
formal-verification, or Wave 8 interaction results.

### Snapshot Paging Evidence

Changes to the frozen native-v1 Uffd reader, pager protocol, lazy-memory
coordinator, Mach/HVF bridges, contained grant, or consumer table must run the
focused inventory test and the affected implementation suites:

```sh
cargo test -p bangbang-pager --all-targets --all-features --locked
cargo test -p bangbang-runtime lazy_memory --all-features --locked
cargo test -p bangbang-firecracker-capability-audit --test checked_inventory snapshot_paging_terminal_policy_is_stable --locked
cargo test -p bangbang native_v1_uffd --all-features --locked
cargo test -p bangbang-hvf --lib --all-features --locked lazy_composite
cargo test -p bangbang-hvf --lib --all-features --locked lazy_host_fault
cargo test -p bangbang-hvf --lib --all-features --locked lazy_guest
```

On supported Apple Silicon, select the signed cases affected by the change and
then run the complete wrapper without `--allow-unsupported` before promoting
the terminal paging record:

```sh
scripts/run-integration-tests.sh --test hvf_lifecycle -- lazy_host_fault_integration::
scripts/run-integration-tests.sh --test hvf_lifecycle -- hvf_lazy_guest_
scripts/run-integration-tests.sh --test guest_boot -- --exact lazy_guest_boot_integration::boots_guest_entry_from_a_lazy_instruction_page
scripts/run-integration-tests.sh --test production_bundle -- signed_pager_grant_
scripts/run-integration-tests.sh --test production_bundle -- signed_pager_consumer_chain_runs_inside_app_sandbox
scripts/run-integration-tests.sh
```

These cases cover direct and contained authority, execute/read/write-first
demand, removal generations, refault-to-zero, peer failure, cancellation,
entitlements, App Sandbox consumers, and ordered cleanup. Current native-v2
Uffd rejection and native-v1 File behavior remain regression gates.

### Compare, Regenerate, and Final Validation

Maintainers can compare the machine-owned manifest with a clean explicit
checkout at the exact pinned commit:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- compare \
  --firecracker /path/to/firecracker
```

Regeneration always targets an explicit candidate and refuses either checked-in
inventory file:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- regenerate \
  --firecracker /path/to/firecracker \
  --output codex-work/tmp/firecracker-v1.16-source-manifest.candidate.json
```

Review exact identity changes before updating `source-manifest.json`. Never
use regeneration to alter `capabilities.json`; missing and stale overlays
must be resolved deliberately.

The checked logger producer audit uses the same authority split. The pinned
machine manifest contains exactly 429 ordinary and 39 unrestricted public
logger calls across 81 matching files: 446 production, zero test-only, 22
example, 466 direct, and two nonlogger macro-template invocations. Compare both
machine manifests with the command above, or create a logger-only candidate:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- \
  regenerate-logger-producers \
  --firecracker /path/to/firecracker \
  --output codex-work/tmp/logger-producer-manifest.candidate.json
```

The destination must not exist and cannot alias any checked JSON artifact.
Review every identity, syntax/source context, input, count, and fingerprint
change before deliberately updating `logger-producer-manifest.json`. The
command never creates, carries forward, or rewrites semantic classes in
`logger-producer-audit.json`; missing or stale mappings require human review.
The class policy and safe-field boundary are documented in the
[logger producer contract](../compat/firecracker/v1.16.0/logger-contract.md).
The current overlay has exactly 24 implemented, no planned, and seven
not-applicable classes, representing 402 implemented and 66 not-applicable
source mappings. Four i8042 mappings moved to the exact x86-only class because
their upstream source module is shared but construction, PIO, API/runtime
ownership, and observable execution remain x86_64-only.

Host lifecycle logger coverage exercises backend and VM transitions, all three
live device kinds, boot-worker observation, automatic metrics failures,
snapshot create/load success/rejection/failure/cancellation, host signals,
guest power convergence, cancellation, cleanup failure, and terminal ordering.
Tests assert the exact closed vocabulary and verify that paths, identifiers,
MAC addresses, descriptors, guest values, and raw errors never reach a record.
Runtime logger tests separately prove bounded host receipts, nonblocking async
delivery, the independent observability limiter, filtering, and exact loss
accounting. Process and signed integration targets verify the executable
ordering boundary.

Backend and generic transport coverage additionally proves the result-free
`GuestLogger` facade, independent per-controller backend/transport limiters,
filter-before-limit behavior, fixed encoding and redaction, MMIO and PCI
attachment before normal or native-v2 publication, typed classification before
string conversion, required run-loop-wrapper forwarding, virtual-timer success
coalescing before terminal backend admission, debug-only expected device
throttling, and unchanged functional results. The signed production bundle
asserts representative transport publication and HVF guest-exit records from a
real Apple Silicon session, while the entropy snapshot continuation proves
normal throttling cannot split the default logger/serial guest marker.

Storage, network/MMDS, and vsock logger coverage exhaustively checks each
closed event and level, independent class limiter identities, batch-level
deduplication, deferred vsock publication after transport interrupt handling,
partial queue results, provider and interrupt supplements, MMDS detours,
transactional key-rotation success/failure, redaction, and unchanged
functional results. Signed Apple Silicon coverage exercises block
data-plane outcomes through both MMIO and product PCI. The contained production
bundle reuses its product-PCI aggregate-storage and MMDS workloads plus its MMIO
vsock workload to prove block, pmem, network, and vsock records reach an exact
write-only logger grant without following pathname replacement or exposing
backing paths, grant references, sockets, MAC addresses, or guest payloads.

Balloon, virtio-mem, entropy, serial, and time/identity coverage checks every
closed event encoding and level, independent limiter identities, summary-level
deduplication, partial and rollback classifications, provider and interrupt
supplements, committed product-PCI configuration plus endpoint-failure
classification, serial input coalescing across normal and restored
continuations, RTC rejection, and normal/native-v2 VMGenID, VMClock, PVTime,
capture, publication, and ordered-restore outcomes.
Runtime and HVF tests assert unchanged functional results and absence of raw
errors, identifiers, descriptors, byte/page counts, timestamps, and guest
values. The signed executable parser admits the complete fixed vocabulary and
rejects extra fields or invented operation/outcome pairs.
The aggregate remaining-device signed workload proves representative records
over both MMIO and product PCI. Production-bundle cases separately adopt an
exact write-only logger grant and require at least one balloon, virtio-mem,
entropy, serial, and time/identity record after replacing the source pathname;
they also reject fixture paths, grant identities, guest markers, serial or
entropy payloads, and time/identity fingerprints.

The final parent gate is:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --final
```

Final mode fails while any `audit-required` or
`missing-platform-feasible` capability record or any planned logger class
remains. An inventory-only change does not promote a capability without
record-specific implementation and validation evidence.

## Running Tests

Run the standard workspace checks before opening or updating a PR:

```sh
cargo fmt --all -- --check
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
cargo check --workspace --all-targets --all-features --locked
cargo check -p bangbang-launcher --all-targets --all-features --locked --target aarch64-unknown-linux-musl
cargo check -p bangbang-snapshot-tools --all-targets --all-features --locked --target aarch64-unknown-linux-musl
cargo test --workspace --all-targets --all-features --locked --exclude bangbang-hvf
cargo test -p bangbang-hvf --lib --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy -p bangbang --test executable_hvf_e2e --all-features --locked --target aarch64-apple-darwin -- -D warnings
cargo clippy -p bangbang --test app_sandbox_process_e2e --all-features --locked --target aarch64-apple-darwin -- -D warnings
cargo clippy -p bangbang-hvf --test hvf_lifecycle --all-features --locked --target aarch64-apple-darwin -- -D warnings
cargo clippy -p bangbang-hvf --test guest_boot --all-features --locked --target aarch64-apple-darwin -- -D warnings
cargo clippy -p bangbang-launcher --test production_bundle_e2e --all-features --locked --target aarch64-apple-darwin -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
```

The explicit clippy commands cover signed integration targets declared with
`test = false`; ordinary `--all-targets` commands intentionally do not select
them.

Run signed HVF integration tests on macOS Apple Silicon without
`--allow-unsupported`:

```sh
scripts/run-integration-tests.sh
```

Run one signed integration test target when the change is narrower:

```sh
scripts/run-integration-tests.sh --test hvf_lifecycle
scripts/run-integration-tests.sh --test guest_boot
scripts/run-integration-tests.sh --test executable_hvf_e2e
scripts/run-integration-tests.sh --test app_sandbox
scripts/run-integration-tests.sh --test production_bundle
```

The `app_sandbox` target is integration-only. It packages the existing
`hvf_lifecycle` binary and the real `bangbang` executable as minimal app
bundles signed with `com.apple.security.app-sandbox` and
`com.apple.security.hypervisor`. On supported Apple Silicon it reruns the
complete lifecycle suite inside App Sandbox, then runs the disabled-by-default
`app_sandbox_process_e2e` target. The process target proves help execution,
path-redacted denial of the default `/tmp` API socket and a config file outside
the app container, HTTP service through a unique container socket, graceful
`SIGINT`, and owned-socket cleanup. Readiness channels and bounded child
deadlines are used instead of fixed sleeps.

The target deliberately excludes vmnet, guest fixture files, production app
distribution, security-scoped bookmarks, and launcher/resource-broker
protocols. A naked CLI binary is not a valid App Sandbox artifact; bundle
identity is part of this test contract. `--allow-unsupported` still builds and
signs both app bundles before runtime validation may be skipped.

The `production_bundle` target exercises the shipped topology instead of the
minimal App Sandbox fixtures. It first performs an explicit no-default-feature
release build for the normal fixed outer app and separately signed nested
worker. It then builds a visibly test-only second bundle with only the
`grant-integration-probe` feature and marker resource, and compiles the
disabled-by-default `production_bundle_e2e` target before an unsupported runner
may skip execution. On supported Apple Silicon it proves:

- exact launcher and worker identifiers, Hardened Runtime on both, no launcher
  App Sandbox/Hypervisor authority, and exactly those two worker entitlements
  with no embedded profile in the default networkless artifact;
- unchanged help/output and representative nonzero worker exit forwarding
  through the structured lifecycle session;
- exact early jailer help/version output and closed policy parsing, including
  fixed executable/current credentials, ID/timing injection, duplicate and
  forwarded-singleton rejection, last-value resource limits, canonical
  default-denied vmnet grammar, and redacted failure;
- fixed typed rejection of exact, attached, and separated `--cgroup`,
  `--cgroup-version`, `--parent-cgroup`, `--netns`, and `--new-pid-ns` requests
  before an intentionally invalid private grant, profile/staging/spawn work,
  worker output, or socket/session mutation, with every supplied value absent
  from stdout and stderr;
- rejection of every positive host/shared/bridge/count vmnet authority by the
  exact two-entitlement networkless profile before worker execution, plus
  negative private-copy coverage for an unexpected networkless profile, a
  missing profile on the five-key shape, a developer-prefixed extra claim, and
  a five-key profile paired with denied policy;
- a marker-only worker exec environment, absent caller/loader/debug variables,
  current credentials/session identity, descriptor-entered private cwd, exact
  default/explicit limits, real `EMFILE` exhaustion, and kernel `SIGXFSZ` at the
  configured file-size boundary;
- daemon caller return only after API readiness, one exact supervisor PID line,
  new-session `/dev/null` execution, two concurrent noninterchangeable
  supervisors, post-ack signal cleanup, and pre-ack parent loss cancelling the
  worker and namespace;
- rejection before worker execution when a private bundle copy has a modified
  or missing worker;
- suspended and post-`Hello` live-worker validation, bounded malformed bootstrap
  rejection before public readiness, and stable path/identity/frame redaction;
- a default-close spawn allowlist that retains standard streams plus only the
  private lifecycle, grant, dormant vsock-broker, and dedicated vhost-user-
  broker endpoints while making a deliberately inheritable unexpected fd
  unavailable;
- container-only API socket readiness plus path-redacted denial of an outside
  config file;
- `SIGINT` and `SIGTERM` as one graceful session cancellation with successful
  worker/launcher exit and owned-socket cleanup;
- worker-first and launcher-first death cleanup, empty both-killed stale
  namespace recovery, and preservation of the concurrent peer namespace;
- two simultaneous API sessions remaining independent when one worker is
  killed and the other is queried and then gracefully stopped; and
- mandatory lifecycle-v5 acknowledgment for even an empty batch; exact
  SCM_RIGHTS read-only/write-only enforcement; one-session directory bookmark
  scope and outside-parent denial; typed mismatch rollback; path/ID/content
  redaction; signal cancellation during an incomplete batch; one absolute grant
  deadline; both grant-bearing crash orders; and concurrent sessions whose
  distinct grant authority cannot be interchanged;
- one exact `snapshot-pager-stream` grant connecting outside the App Sandbox,
  independent worker descriptor/peer/protocol validation, complete
  page/zero/removal/shutdown, cancellation and terminal sessions, refused
  connection, wrong descriptor/protocol, EOF, timeout, peer and worker death,
  repeat launch, cleanup/redaction, signature inspection, and unchanged
  entitlements;
- rejection of the internal grant probe by the normal production worker with no
  resource mutation, proving the exerciser is absent from the shipped build;
- unlinked shared guest-memory allocation, two-way descriptor coherence, and
  real HVF map/unmap inside the nested App Sandbox worker without adding an
  entitlement or enabling a public socket-backed drive;
- both the sealed baseline and externally granted startup config, metadata,
  kernel, initrd, repeatable read-only/read-write drives, and repeatable
  read-only/read-write pmem launching real sandboxed HVF guests after committed
  no-API readiness and ending through PSCI `SYSTEM_OFF`;
- delayed API-time kernel/initrd adoption after metadata readiness, pathname
  replacement after the launcher opened the files, authorized references in
  `GET /vm/config`, and a real guest boot from the retained identities;
- invalid-command-line, wrong-role, and missing boot requests preserving the
  prior public configuration; grant faults stay redacted and the otherwise
  valid pair remains unconsumed;
- delayed block/pmem API claims with exact role/access, malformed/missing
  rejection, and one-time behavior; same-ID rollback, authorized config tags,
  source-path replacement after the launcher opened every file, guest-visible
  writable block persistence, pmem marker read and flush persistence, and
  path-free block/pmem limiter updates retaining their backing ownership;
- read-only pmem root boot from an exact launcher-opened descriptor after its
  source pathname is replaced, with `/dev/pmem0`, `ro`, an unchanged
  replacement file, and unchanged App Sandbox plus Hypervisor entitlements;
- read-only drive authority reaching a real guest as a failed write while the
  original opened backing remains unchanged; and
- preauthorized after-start block replacement synchronized by the guest's
  virtio-mem ready/grow/shrink markers, proving subsequent guest writes reach
  the launcher-opened replacement object rather than a planted pathname;
- exact write-only logger, metrics, and serial sink adoption through startup
  CLI, config-file, and delayed API paths in the normal bundle; the delayed case
  renames every launcher-opened source and plants replacements before claim,
  then proves API/action logger records, initial and terminal metrics JSON, and
  real guest console bytes append only to the opened originals;
- malformed, missing, wrong-role, repeated metrics, and consumed output claims
  fail without replacing prior sinks or consuming a valid cross-role grant;
  faults and process output stay path/ID/reference-redacted; and
- two simultaneous workers reuse the same three GrantIds in independent
  registries, apply mutually exclusive logger module filters, start real guests,
  and write logger/metrics/serial output only to their own opened objects while
  planted replacement paths remain unchanged;
- exact external snapshot grants creating exact native-v2 2.12 Full
  serial-plus-profile-3 rooted three-drive MMIO and PCI pairs without entropy
  into separate output directories, reusing both retained
  directories for a second successful pair, preserving all finals on
  collision, and keeping same-GrantId concurrent source workers in their own
  directories; granted early description and two fresh complete-set
  state/memory/drive File/COW loads per transport then prove exact graph
  reconstruction, explicit and automatic resume, and a root-read-conditional
  guest `SYSTEM_OFF`;
- a deterministic native-v2 2.7 serial-plus-profile-3 block certifier running
  rooted and rootless graphs over MMIO and PCI through the normal
  launcher/worker boundary. It
  validates all three seeded drives, persists two writable pre-capture epochs,
  rejects audit-drive writes, observes limiter retry metrics, recaptures a
  Paused destination, explicitly resumes one destination, and automatically
  resumes a second rootless destination against the same shared writable
  backings. Representative rootless-MMIO worker-first and launcher-first
  deaths after Paused publication prove exact API socket, session, staging,
  artifact, backing, and replacement cleanup;
- `normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci`,
  which extends that protocol to rooted pmem-only and rootless mixed
  block/pmem cells over MMIO and PCI. It uses exact `PmemBacking` grants after
  pathname replacement, advances shared writable file prefixes, preserves
  read-only peers, verifies zero private DAX tails on each fresh mapping,
  recaptures, resumes explicitly/automatically, and proves staging,
  descriptor, grant, session, replacement, worker-first, and launcher-first
  cleanup;
- `normal_bundle_certifies_native_v2_serial_snapshot_continuation_and_containment`,
  which runs a serial-only default-stdio source/destination through fresh
  launcher pipes and configured-output sources/destinations with profile-3
  storage over MMIO and PCI. It retains a full UART RX prefix, excludes bytes
  left in the terminated source pipe, supplies only destination input, resolves
  fresh write-only serial grants after pathname replacement, verifies
  destination-only metrics and immutable artifacts, redacts every private
  selector, and restores the session namespace after launcher/worker teardown;
- `normal_bundle_certifies_native_v2_entropy_snapshot_continuation_and_containment`,
  which runs entropy-only and profile-3 storage-plus-entropy graphs over MMIO
  and PCI. It replaces every source pathname after launcher adoption, restores
  exact pending queue/dual-limiter/retry state through fresh destination
  entropy, scheduler, notifier, route, endpoint, and metrics owners, completes
  the retained second `/dev/hwrng` read without another guest kick, recaptures,
  reuses immutable artifacts through explicit and automatic resume, rejects a
  checksum-malformed entropy state, and proves graceful cancellation
  plus worker-first and launcher-first staging/session/socket cleanup;
- a feature-gated root-plus-vsock restore-resource probe that uses the real
  coherent contained-session authority, exact typed take/adopt/commit, reverse
  reservation abort and reuse, and all nine deterministic cancellation points;
  signed cases cover launcher-opened root identity after pathname replacement,
  prepared and active cleanup, both independent launcher/worker death orders,
  published-socket replacement preservation, concurrent same-ID session
  isolation, fixed redacted output, unchanged networkless entitlements, and no
  ready-state helper, while the normal bundle rejects the probe option;
- `normal_bundle_certifies_native_v2_vsock_restored_guest_lifecycle_and_containment`,
  which uses the normal launcher/nested App Sandbox worker over MMIO and PCI to
  restore original and overridden selectors, prove old-stream reset/loss,
  event-acknowledged RX release with live TX, preserved guest listeners,
  deterministic bidirectional multistream/half-close, clone-local cursor/
  metric/socket ownership, recapture, immutable inputs, replacement safety,
  malformed/no-device/missing-authority faults, Paused cancellation, both
  independent death orders, later retry, redaction, unchanged entitlements,
  and exact staging/session/socket/helper cleanup;
- source kernel/root/metrics and load state/memory pathnames replaced after the
  launcher opens them, with no tag reopen, no staging residue, redacted
  wrong-role output, and no extra private session namespace;
- a test-only hold immediately after durable snapshot staging ownership is
  recorded, followed by worker `SIGKILL`; launcher recovery removes an exact
  current-user regular `0600` single-link inode but preserves a same-name
  replacement while clearing the private record and namespace;
- exact socket-directory references publishing an owner-only API listener into
  an outside-container granted directory, serving a real client only after
  readiness, and reaping the short-lived signed binder before exposure;
- delayed API `PUT /vsock` retaining the directory claim until startup,
  publishing the supplied main listener, and leaving only launcher plus worker
  in steady state with the worker's exact entitlements unchanged;
- a real guest initiating connections to two distinct host ports through the
  dormant-then-fixed launcher facet, with only port requests and connected fds
  crossing the private protocol; and
- a real host initiating through the supplied granted main listener and
  completing deterministic 1-MiB transfers in both directions plus both peers'
  write-half-close/EOF sequence before identity-owned socket cleanup;
- a contained vhost root and writable scratch child sharing one connect-only
  directory grant alongside vsock, booting a real guest without a steady-state
  helper, proving scratch read/write/flush plus guest-observed ID-only capacity
  refresh on the existing stream, and closing both exact child streams; and
- a contained all-PCI vhost lifecycle that rejects an invalid endpoint without
  killing the live VM, rolls back failed negotiation, attaches a new device,
  rejects duplicate same-ID PUT before a second connection, then performs
  manual guest removal, DELETE, Paused same-ID reuse through another child,
  resumed guest I/O, final DELETE, and exact closure; and
- launcher-first and worker-first abrupt death after replacing the granted API
  pathname, proving both surviving cleanup owners preserve the replacement,
  clear only the matching private record, and remove the session namespace.

The production target receives the same generated direct-boot ext4 fixture as
the signed executable target, but supplies it only as an external drive grant;
it is never embedded in the worker bundle. The runner's resource overlays and
grant exerciser are internal signed-test inputs.
`scripts/build-production-bundle.sh` explicitly excludes the feature, does not
expose an overlay, and places no guest resources in a normal product. The
all-features development binary is not a shippable bundle. Tests use readiness
events and bounded deadlines rather than fixed sleeps.

Portable `bangbang-session` tests exhaustively split and coalesce every v5
message frame and cover the fixed reserved-zero redacted `WorkerPolicy`, wrong
magic/version/reserved data, exact frame/buffer
limits, oversized input, EOF rejection, replay, sequence gaps, cross-session and
wrong-role/state input, reserved identity use, monotonic API/early-command/
cancellation/grant state, and payload/identity-redacted formatting. Grant codec
tests cover every closed record, limit and descriptor declaration, including
connected-stream source/peer identity and the 255-byte redacted snapshot child
grammar. Socket
broker codec tests cover every closed kind, exact fixed frame/reserved fields,
session/sequence/child/port/status encoding, descriptor declarations,
truncation, malformed ancillary data, and value-redacted formatting. The
separate fixed 256-byte `BBU1` vhost-user broker codec covers exact
session/sequence/grant/child/status correlation, one-stream rights, retryable
failures, stale or malformed response rejection, and facet poisoning. Darwin
tests also cover the fixed 256-byte `BBC1` block-control codec's exact
session/sequence/grant/access/status/identity/geometry correlation, closed
inspect/cache-sync operations, no-rights replies, bounded failure classes,
stale or malformed response rejection, and facet poisoning. Darwin
unit tests cover SCM_RIGHTS and FD_CLOEXEC, payload/control truncation, malformed
ancillary cleanup, exact descriptor access/type/identity, sequence/session/batch
poisoning and rollback, fragmented bookmark scope, kernel peer acceptance and
PID rejection, exact namespace naming/root derivation, bounded independent
directory iteration across repeated checks, stale empty-directory recovery,
populated-entry preservation, strict socket ownership records, identity-safe
fixed-staging cleanup, anchored exclusive publication/rollback, binder
framing/descriptor validation, broker state and relative-target validation, and
replacement-safe cleanup. Snapshot registry/runtime tests additionally cover
non-consuming exact file duplication, validate-all-before-remove state/memory/
root and output-directory batches, shared/distinct output anchors, strict
per-artifact record encoding, record-before-producer ordering, clear-on-success,
supplied-root preparation without persisted-selector reopen, and exact versus
replacement-preserving launcher cleanup. Socket readiness helpers use bounded kernel event
waits instead of active polling. These tests do not replace the signed target:
default-close spawning, dynamic code identity, App Sandbox root resolution,
crash order, and real HVF claims require the packaged execution above.

Build a local production bundle without running the integration suite:

```sh
scripts/build-production-bundle.sh --output /path/to/Bangbang.app
```

The destination must be absent and named `Bangbang.app`. The wrapper builds for
`aarch64-apple-darwin`, uses ad-hoc signing by default, and accepts one optional
signing identity for both independently signed code objects.

The default path is the profile-absent two-entitlement `networkless` worker. A
caller with an Apple-approved profile can exercise the same nonpublishing
assembly, signing, exact entitlement/profile/certificate inspection, and
current-host authorization gate with:

```sh
scripts/preflight-production-vmnet.sh \
  --output /path/to/Bangbang.app \
  --signing-identity "Developer ID Application: Example (TEAMID)" \
  --provisioning-profile /private/path/vmnet.provisionprofile
```

Success is exactly `bangbang vmnet preflight: ready` and exit 0; any runtime
credential/profile/signing/authorization failure is exactly
`bangbang vmnet preflight: blocked` and exit 3. CI deliberately supplies no
credential and asserts the blocked contract. Unit tests use synthetic decoded
profiles and signing tools to prove bounds, ordering, leaf matching,
nonpublication on authorization failure, cleanup, and that the disposable
probe—not the supplied worker—is the only executable handed to the
authorization runner. None of those tests claim `vmnet_start_interface` or
packet connectivity; that positive signed matrix remains #1378.

The signed `hvf_lifecycle` native-v1 composite case builds the accepted one-
vCPU/read-only-root session and gives the production generalized publisher two
absent final paths. Its producer captures the complete non-memory state and
streams memory directly to the publisher staging writer while block, PMEM,
network, and entropy retry schedulers remain quiesced through the publisher's
durable memory-first/state-last commit, returns kind 2, and leaves no staging
residue. The test loads that pair through the production loader,
decodes and validates the bundle and nested device state without logging raw
values, and repeats capture with a fresh image identity. The guest first leaves
non-default serial scratch state;
after both captures, the original source continues from its retained PC to the
next fixed HVC and the runner owner remains usable before shutdown. After source
shutdown, the already loaded production-published pair constructs a fresh
destination VM and verifies pre-run vCPU/ICC/pending/device state, normalized
timer equivalence, ordered VMGenID/VMClock restore, absent boot-origin metadata,
and continuation from the captured PC. Opaque GIC bytes are asserted nonempty and
bounded after recapture rather than byte-equal because Hypervisor.framework's
stable versioned serialization is not a canonical encoding.
This one-vCPU artifact transaction combines with the signed executable's exact
two-vCPU topology-wide pause/resume barrier as the SMP barrier evidence. It does
not claim an SMP native-v1 artifact. External vmnet/vsock peer and host/kernel
buffers remain outside both tests' snapshot-state claims.
Run the repository command without `--allow-unsupported`; this evidence must
execute on supported Apple Silicon hosts.

Run only the process-level executable e2e test when the change is limited to
the `bangbang` process boundary:

```sh
cargo test -p bangbang --test process_e2e --all-features --locked
```

The process contract cases include a 64-byte multibyte Unicode ID returned
unchanged through `GET /`, Unicode symbol and byte-overlong rejection with exit
153 before socket publication, a zero HTTP body limit that preserves bodyless
requests while returning 413 for nonempty bodies, a zero MMDS data-store limit
that rejects every serialized object without preventing startup, and
Firecracker's first-`--` behavior that ignores all following main-process
tokens. Colocated parser unit tests also cover zero and `usize::MAX`, Unicode
punctuation, exact UTF-8 byte boundaries, and ignored non-UTF-8 bytes after the
separator as a bangbang robustness extension.

The process suite covers native snapshot inspection without starting HVF. It
checks exact `v2.13.0` output for `--snapshot-version`, exact description of
native-v1, legacy `2.3.0`, `2.4.0`, `2.5.0`, `2.6.0`, `2.7.0`, `2.8.0`, and
`2.9.0`, `2.10.0`, `2.11.0`, and `2.12.0` native-v2 fixtures, plus
explicit pinned
Firecracker/unknown incompatibility. It also
covers missing, non-regular,
oversized, malformed, truncated, trailing/inconsistent-length, corrupt,
unsupported-version, incompatible-architecture, and incompatible-page-size
files. Fixtures use unique temporary paths; failures must use the
bad-configuration exit code, publish no API socket, and expose neither path nor
payload sentinels.
The contained external-file variant belongs to the signed production-bundle
target above because it requires lifecycle grant delivery, App Sandbox bookmark
scope, and the fixed launcher/worker topology.

Run the same process-level e2e test against a signed `bangbang` executable:

```sh
scripts/run-signed-process-tests.sh
```

This builds and signs a temporary `bangbang` executable, then sets
`BANGBANG_PROCESS_E2E_BIN` so `process_e2e` launches that signed binary instead
of Cargo's default test binary. The script verifies process startup, API socket
serving, configuration requests, multi-process socket isolation, and clean
shutdown. It requires macOS Apple Silicon because the signed executable target
is `aarch64-apple-darwin`, but it does not start HVF or send `InstanceStart`.

Build a signed `bangbang` executable artifact for future HVF-backed process e2e
tests without running it:

```sh
scripts/build-signed-bangbang.sh --output .tmp/signed-bangbang/bangbang
```

This requires macOS `codesign` and the `aarch64-apple-darwin` Rust target. The
command only builds and signs the executable; HVF execution remains the job of
the signed integration runner.

Run executable-level HVF e2e through the signed integration runner:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e
```

This target runs the dedicated `executable_hvf_e2e` Cargo test target. It builds
and signs a temporary `bangbang` executable, prepares the pinned Firecracker
kernel, deterministic tiny initrd, and generated direct-boot ext4 rootfs,
starts `bangbang` as a child process, configures the VM through the Unix-socket
API or a Firecracker-shaped config file depending on the scenario, and waits
for deterministic guest progress in host-observable outputs and backings. The
retained real-root native-v2 smoke configures one read-only Sync root and runs
the public lifecycle over both default MMIO and `--enable-pci`. It waits until
a positive root-read metric is stable for 500 ms before
pausing, proving the source has already followed live MMIO or PCI root I/O.
The source UART must remain canonical, while its Linux-consumed FDT bytes are
captured and CRC-bound without being reparsed as trusted post-boot topology.
Creation through `/snapshot/create` then verifies the real CLI reports
`v2.12.0`. Fresh signed processes repeatedly load the same immutable pair: one
remains paused until public `PATCH /vm`, and one uses `resume_vm: true`. The
paused destination is also publicly recaptured and its decoded root graph must
equal the source graph. After resume Linux reads the known root marker through
the reconstructed virtio-block owner, observes completion through the restored
transport interrupt path, and calls PSCI shutdown only when that read matches;
`panic=0` and a failure sleep make any boot or read failure time out instead of
producing a false success. State and memory artifacts remain byte-identical
after every destination. The companion two-vCPU bare-guest case retains
deterministic pre-capture memory
checkpoints, collision/no-clobber redaction, all-vCPU private-COW continuation,
recapture, and root-owner rollback. Its terminal destination closes the sole
stdout reader before resume; deterministic guest UART output reaches the
ordinary `BrokenPipe` terminal path without SIGPIPE killing the process, and
the API socket is cleaned.

The retained block-storage completion case
`macos_arm64::signed_executable_certifies_native_v2_multi_block_epochs_over_mmio_and_pci`
runs one deterministic `snapshot-block-init` protocol in four signed cells:
rooted/rootless × MMIO/PCI. Each cell carries three regular-file drives with
mixed read-only/read-write, Sync/Async, Unsafe/Writeback, partuuid, and limiter
configuration. The guest validates seeded bytes, drives limiter retry, writes
and flushes both writable pre-capture epochs, and proves the audit drive
rejects writes before the source is paused. A fresh process loads the pair
initially Paused, exposes the exact graph and a fresh metrics owner, recaptures
an equivalent stable graph, then resumes and advances both writable epochs.
Rootless cells add a second automatically resumed fresh process against the
same immutable state/memory pair and shared external drive files. This proves
that guest memory is private while writable backings deliberately are not
COW-isolated. State/memory inputs stay byte-identical throughout.

Run just the retained real-root smoke with:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  macos_arm64::signed_executable_restores_native_v2_root_io_over_mmio_and_pci \
  --exact
```

Run just the retained block-only completion matrix with:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  macos_arm64::signed_executable_certifies_native_v2_multi_block_epochs_over_mmio_and_pci \
  --exact
```

Run the profile-3 pmem certification matrix with:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  macos_arm64::signed_executable_certifies_native_v2_storage_epochs_over_mmio_and_pci \
  --exact
```

Run the exact-2.7 serial continuation and configured-output matrices with:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  signed_executable_certifies_native_v2_serial_continuation_over_fresh_stdio \
  --exact
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  signed_executable_reopens_configured_serial_snapshot_file_and_fifo_destinations \
  --exact
```

The first shared bare-arm64 guest programs nondefault UART registers, fills the
64-byte FIFO, leaves a distinct suffix in the source process pipe, terminates
the source, and accepts only a different suffix from each fresh destination.
It runs serial-only plus MMIO/PCI storage products, decodes exact state,
recaptures while paused, exercises explicit/automatic resume, verifies
destination-only metrics, and preserves the immutable pair. The second test
renames source regular-file/FIFO output endpoints and creates fresh destination
endpoints at the persisted selector; restored TX must reach only the
destination endpoint.

Run the exact-2.8 entropy continuation matrix with:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  signed_executable_certifies_native_v2_entropy_snapshot_continuation \
  --exact
```

Its `/snapshot-entropy-init` guest consumes one 64-byte entropy prefix and
retains a second read behind both limiter buckets before capture. MMIO/PCI
entropy-only and storage-plus-entropy destinations reconstruct fresh owners,
complete the retained read without a new guest kick, recapture, resume
explicitly or automatically, expose fresh metrics, and preserve the immutable
pair after source termination.

The
tiny-initrd scenarios write `BANGBANG_BLOCK_WRITE_OK` to scratch block backing
files and include API/config-file coverage for configured serial output files.
The API-request, API-enabled config-file, and no-api config-file scenarios
verify vsock listener binding during startup and owned vsock listener cleanup
on shutdown. The API-request and API-enabled config-file scenarios verify
one session-initial metrics line plus explicit runtime `FlushMetrics` and logger
output before shutdown, then verify exactly one additional normal-terminal
metrics line after clean process exit. The config-file guest
stop scenarios boot the tiny initrd's `/poweroff-init` or `/reboot-init`, which
invoke Linux reboot syscalls so the kernel issues PSCI `SYSTEM_OFF` or
`SYSTEM_RESET`, and verify that API-enabled and no-api `bangbang` processes
exit successfully. The
direct-rootfs scenarios boot the generated ext4 rootfs without an initrd. They
include a public `/serial` scenario that waits for
`BANGBANG_DIRECT_ROOTFS_BOOT_OK` in the configured serial output file, plus
scratch-drive scenarios that write `BANGBANG_DIRECT_ROOTFS_BLOCK_OK` through a
second writable drive. A boot-timer scenario starts the signed executable with
`--boot-timer`, boots the Firecracker rootfs-provided `/usr/local/bin/init`
wrapper, and waits for `Guest-boot-time` in the configured logger output after
that wrapper writes the Firecracker magic byte to the boot-timer MMIO address.
This verifies the public process/API/config-file/HVF path, including public
serial output redirection and implemented observability reachability. The executable HVF
e2e target also includes direct-rootfs MMDS v1 and v2 token-flow scenarios that
configure a `vmnet:shared` network interface, configure MMDS for that
interface, fetch a deterministic MMDS value from the guest through
`169.254.169.254`, and write host-observable markers to unique scratch drives.
The API-driven v1 case also configures a nondefault `1280` MTU and requires the
guest's selected Linux interface to report that value before the MMDS fetch can
write its success marker.
It also includes paired direct-rootfs entropy lifecycle scenarios over default
MMIO and product PCI. Each configures both limiter buckets, checks that Linux
selected `virtio_rng`, completes one nonempty `/dev/hwrng` read, and then waits
on a scratch-sector host continuation marker. The host establishes a metrics
baseline, releases the guest, waits for a new limiter throttle, pauses the VM,
and invokes public native-v2 creation. The expected optional-profile rejection
occurs only after capture-ready entropy traversal and creates no artifacts.
After resume, the guest completes eight additional nonempty reads and publishes
a terminal marker; the host requires a retry-event metric and clean shutdown.
The marker gate and two-second refill window make the pending retry observation
causal rather than sleep-based.
The #1481 aggregate cases
`signed_executable_certifies_remaining_devices_over_mmio` and
`signed_executable_certifies_remaining_devices_over_product_pci` use one shared
guest protocol to compose balloon, virtio-mem, entropy, default serial stdio,
PL031, VMGenID, VMClock, and PVTime. They assert exact transport identity,
reject an unaligned memory update without projection mutation, overlap a valid
virtio-mem grow with a balloon-statistics update, exercise entropy throttling,
send more than the UART FIFO capacity, pause with input queued, require ordered
capture-ready optional-profile rejection with no artifacts, resume, shrink and
deflate, detach stdin at EOF while the API remains live, and repeat a short
session with the same socket/control-resource names. This proves live and
capture-ready coexistence only. Exact serial encoding/restore is certified by
the dedicated 2.7 matrix above and exact entropy encoding/restore by the
dedicated 2.8 matrix. This aggregate case does not by itself prove balloon,
virtio-mem, network, or PVTime encoding/clone portability.
It also includes a direct-rootfs balloon scenario that configures `/balloon`,
enables free-page reporting, checks that the guest bound a virtio-balloon driver
and negotiated reporting feature bit 5, observes periodic optional statistics,
updates the polling interval from 1 to 2 seconds without losing the exact
reported optional-field set, observes guest STOP plus automatic host DONE and
an explicit hinting stop, and flushes public metrics until
`balloon.free_page_report_count` is nonzero. While paused, the public snapshot
path traverses the capture-ready MMIO owner before the guest resumes. The test
then changes the target from 8 MiB to zero and requires exact target/actual page
convergence before shutdown. The product-PCI aggregate scenario independently
proves paused capture traversal, resume, and target-to-zero convergence for the
selected PCI owner. The guest writes a host-observable marker only after driver
binding and reporting negotiation are visible. These scenarios prove signed
guest inflation, deflation, statistics, hinting, reporting, MMIO/PCI capture
ownership, and cleanup; they do not impose a process-footprint threshold.

Exact native-v2 2.9 continuation is certified separately by
`signed_executable_certifies_native_v2_balloon_snapshot_continuation` and
`normal_bundle_certifies_native_v2_balloon_snapshot_continuation_and_containment`.
Both loop over MMIO and PCI. The source guest inflates exactly 8 MiB/2,048
4-KiB pages, supplies optional statistics, changes its polling interval from
one to two seconds, leaves one statistics descriptor pending, completes a
hinting run at DONE, reports free pages, and captures a byte-stable Full/File
pair. Decoding asserts exact kind 10, coherent transport and optional-product
shape, latest values, pending cursor identity, hint normalization, and exact
accounting.

One fresh destination stays Paused beyond a full interval and proves no
statistics scheduling, then recaptures the normalized semantic state before
explicit resume. A second uses automatic resume. Both prove exact API
continuity, completion of the retained descriptor only after a full
destination-local interval, a new hint run after DONE, reporting,
deflate/inflate convergence, fresh destination-only metrics, and an attempted
best-effort discard without asserting synchronous RSS. The direct test reuses
the same immutable artifacts; the normal production/App Sandbox test also
proves exact state/memory/root/data/audit/metrics/socket grants after pathname
replacement, checksum-malformed rejection, graceful cancellation,
launcher-first and worker-first death cleanup, recapture, and independent
session/socket ownership.

The focused exact-2.9 suites additionally pin the 262,144-range and 4-MiB
component bounds under the 16-MiB state cap, every feature/layout/queue/
statistics/hint/accounting/transport relationship, all eight optional product
combinations, destination-unmapped accounting rejection, source recovery after
over-bound create, fresh owner construction, rollback, and exact 2.3–2.8
compatibility. Guest PFNs stay 4 KiB even though Darwin host-page reclaim
normally aligns/coalesces at 16 KiB.

Exact native-v2 2.10 virtio-mem continuation is certified separately by
`signed_executable_certifies_native_v2_memory_hotplug_snapshot_continuation`
and
`normal_bundle_certifies_native_v2_memory_hotplug_snapshot_continuation_and_containment`.
Both loop over MMIO and PCI. The checked Linux
`bangbang.memory-hotplug-snapshot=1` guest records its pre-plug available
memory, grows from 0 to 128 MiB, retains a Python mapping larger than that
baseline, and writes a deterministic nonzero sentinel on every page. Each
destination verifies every page as its first restored action before releasing
the mapping or changing topology.

The source publishes a byte-stable Full/File exact-2.10 pair with kind 11
bound to the plugged kind-1 extents and terminates while Paused. One fresh
destination remains Paused, verifies API/config/queue/accounting and the
restored bytes, recaptures normalized state, and explicitly resumes. A second
fresh destination automatically resumes from the same unchanged pair. Each
then offlines the aperture, shrinks requested size from 128 to 64 MiB, unbinds
and rebinds `virtio_mem`, observes the real reprobe UNPLUG_ALL transition,
reissues the retained 64-MiB request to re-PLUG, grows to 128 MiB, and finally
shrinks to zero. Exact destination metrics require 128 MiB plugged, 192 MiB
unplugged, at least one successful UNPLUG_ALL, and zero failure counters.

The normal production/App Sandbox case uses exact kernel/root/data/metrics/
snapshot/API grants, replaces every source pathname after open, and requires
the replacements and immutable pair to stay unchanged. Its representative MMIO
faults cover checksum-corrupted state, structurally truncated memory, graceful
cancellation, worker-first death, and launcher-first death with no published
socket or retained session namespace. The lower-layer exact-2.10 suites retain
fixed and hostile codec/bitmap/geometry/extent tests, a 523,264-block and
65,408-byte bitmap bound, the 128-KiB component and 16-MiB state caps,
block-granular materialization/dynamic mappings, all sixteen products,
same-process immutable peer loads, controller commit/cancellation, owner
rollback, signed recapture, and exact 2.3–2.9 readers.

Run the focused signed proofs with:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  signed_executable_certifies_native_v2_memory_hotplug_snapshot_continuation \
  --nocapture
scripts/run-integration-tests.sh --test production_bundle -- \
  normal_bundle_certifies_native_v2_memory_hotplug_snapshot_continuation_and_containment \
  --nocapture
```

Exact native-v2 2.11 network/MMDS continuation is certified separately by
`signed_executable_certifies_native_v2_network_mmds_snapshot_continuation` and
`normal_bundle_certifies_native_v2_network_mmds_snapshot_continuation_and_containment`.
Both loop over MMIO and PCI with MMDS V1 and V2 sources. The checked guest
confirms its restored virtio device, fixed MAC, 1280-byte MTU, and transport,
fetches source data, retains a half-open source TCP connection and (for V2) a
source token, then signals capture readiness.

The source publishes exact kind 12 and terminates while Paused. One destination
uses a distinct complete clone-local selector set, remains Paused, recaptures
normalized state, and explicitly resumes; a second destination uses another
selector and automatic resume. Before reseeding, host GET returns exact JSON
`null`. After reseeding, the guest proves the source connection is lost, the
source V2 token returns `401`, and a fresh V1 session or newly minted V2 token
retrieves only destination data. State and memory stay byte-identical, and each
destination exposes fresh metrics and owners.

The normal production/App Sandbox case repeats the four transport/version cells
through exact snapshot and API grants with every pathname replaced after open.
Its representative MMIO/V2 failures use independent contained processes for
missing, duplicate, and unknown overrides, checksum-corrupted state, truncated
memory, graceful SIGTERM cancellation, worker-first death, and launcher-first
death. Selector/path contents stay redacted; no failed case publishes a VM or
metrics, mutates the valid state/memory pair, retains a private namespace, or
consumes vmnet authority. Later valid destinations prove retry and cleanup.
Focused lower-layer tests pin the `BANGNW2\0`, `BANGNI2\0`, and `BANGMD2\0`
codecs, 16-interface and 83,552-byte worst-case bounds, 512-KiB component and
16-MiB state caps, all 32 products, exact override-set validation, placement,
resource construction, cancellation, rollback, redaction, and exact 2.3–2.10
readers.

Run the focused signed proofs with:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  signed_executable_certifies_native_v2_network_mmds_snapshot_continuation \
  --nocapture
scripts/run-integration-tests.sh --test production_bundle -- \
  normal_bundle_certifies_native_v2_network_mmds_snapshot_continuation_and_containment \
  --nocapture
```

Exact native-v2 Full 2.12 vsock continuation is certified by the separate exact
tests
`signed_executable_certifies_native_v2_vsock_snapshot_over_mmio`,
`signed_executable_certifies_native_v2_vsock_snapshot_over_product_pci`, and
`normal_bundle_certifies_native_v2_vsock_restored_guest_lifecycle_and_containment`.
The direct pair creates one real source with a guest-to-host stream and a
host-to-guest stream retained across capture readiness. Full creation closes
both source streams, queues `TRANSPORT_RESET`, records the exact host-local
cursor, and emits kind 13 with no live work.

Two independent direct destinations load the same immutable pair: one through
the original selector remains Paused while 16 host requests are queued, and a
second through `vsock_override` automatically resumes and completes first.
The Paused destination shows no guest marker or reply progress until explicit
resume. Each guest observes reset on the old streams, acknowledges it, then
completes four fresh guest-to-host and 16 fresh host-to-guest deterministic
4-KiB streams through preserved guest listeners. Every stream proves complete
payload/reply integrity, half-close/EOF behavior, and the exact clone-local
cursor increment. Recapture records `saved + 16`; source artifacts, destination
metrics, socket ownership, and clone cursors remain independent.

The production/App Sandbox test repeats original and overridden MMIO/PCI
destinations through the normal outer launcher, nested worker, and exact grant
manifests. Process-local serial output is the guest evidence channel. Its
representative MMIO hostile matrix covers checksum-corrupted state, truncated
memory, a no-device override, a missing exact vsock-selector grant, Paused
graceful cancellation, worker-first `SIGKILL`, launcher-first `SIGKILL`, and a
later valid retry. It also proves pathname replacement resistance, exact
session/socket/helper cleanup, immutable inputs, and redaction of selector,
descriptor, grant/session, connection, and payload authority.

Run the focused signed proofs with:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  signed_executable_certifies_native_v2_vsock_snapshot_over_mmio --exact
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  signed_executable_certifies_native_v2_vsock_snapshot_over_product_pci --exact
scripts/run-integration-tests.sh --test production_bundle -- \
  normal_bundle_certifies_native_v2_vsock_restored_guest_lifecycle_and_containment \
  --exact
```

Runtime `PATCH /balloon` target-size updates are covered by unit, API socket,
and process-session tests that verify stored config updates, active config-space
generation changes, and config interrupt signaling. Guest-reported statistics
queue records are covered by runtime unit, API response, and process-session
tests. Runtime statistics interval updates are covered by unit, API socket,
process-session, and signed guest tests. The signed guest observes timer-driven
polling and exact optional-statistics preservation across a live interval
change. Linux's statistics notification after `FEATURES_OK` but before
`DRIVER_OK` is retained until activation over both virtio-MMIO and virtio-PCI;
focused tests cover both admission and deferred dispatch. Hinting
queue guest-command acknowledgement, automatic host DONE
acknowledgement, active/stale range selection, best-effort advice outcomes, and
inflate/hint metrics are covered by runtime unit and MMIO handler tests.
Reporting queue tests cover compact queue-index routing with hinting enabled,
multi-descriptor chains, multiple available chains, writable-direction checks,
empty and overflowing ranges, unmapped memory, injected platform failures,
bad-then-valid best-effort progress, used-ring and later-available failures,
discard-before-ack ordering, interrupt intent, and requested/advised/skipped/
failure metric separation. Startup and HVF signal tests cover reporting queue
notification routing and shared metrics recording.
Guest-memory tests inject page sizes and zero/free failures to verify complete
validation, per-region segmentation, inward alignment, 4-KiB-within-16-KiB
neighbor safety, partial failures, byte accounting, repeats, and independent
owners. A macOS-only real anonymous-mapping test requires zero contents after
`MADV_ZERO` plus `MADV_FREE` reuse without asserting RSS.
Shared-profile tests additionally require exact file length and zero offset,
mode `0600`, link count zero, close-on-exec duplication, bidirectional
descriptor/mapping coherence, redacted debug output, inherited dynamic-region
backing, dirty writes, native snapshot round trips, typed low
`RLIMIT_FSIZE`/`RLIMIT_NOFILE` preflights, and zero-safe `F_PUNCHHOLE` discard.
The signed `hvf_lifecycle` case write-protects shared RAM, observes the first
guest dirty fault, retries to HVC, reads the guest write through an independent
descriptor, restores permissions, unmaps, and destroys the VM. The test-only
production bundle repeats shared creation, descriptor coherence, and HVF
map/unmap inside the real nested App Sandbox worker with the unchanged App
Sandbox plus Hypervisor entitlements.
It also includes a direct-rootfs writeback block scenario that configures a
non-root data drive with `cache_type=Writeback`, writes through `/dev/vdb`,
calls `fsync` on the block-device file descriptor, and writes a host-observable
marker only after that flush returns.
It also includes a direct-rootfs pmem scenario that configures `/pmem/pmem0`
with a valid rate limiter through the public API, applies a live limiter
replacement, waits for `BANGBANG_PMEM_READ_FLUSH_OK` in a scratch drive, and
then verifies the guest-written pmem marker in the host backing file.
The normal production-bundle target repeats the block/pmem guest evidence with
outside-container files transferred by the launcher. It renames every source
after API readiness, plants replacement pathnames, and requires writes and pmem
flushes only in the already-opened objects. A separate read-only block case
observes `BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL_WRITE`, and a staged virtio-mem
guest checkpoint proves a live block grant swap receives later guest writes.
Because every configured network interface is bound to MMDS in these scenarios,
startup uses the process-local MMDS-only packet path and does not require
external vmnet packet movement.

### Observability Evidence Map

Exact logger timing and failure semantics are normative in focused runtime
tests, not wall-clock signed tests. Injected monotonic time covers the initial
ten-record boot-timer burst, 500-ms refill, five-second budget, backwards time,
saturating suppression count, bounded CAS exhaustion, concurrent conservation,
named-identity/controller independence, and the ordered recovery batch. Fixed
record tests cover API receipt, all seven control outcomes, all four HTTP
results, retained action events, normal process startup, panic/terminal, timer,
and limiter-recovery shapes; they also cover exact levels, the 512-byte/UTF-8
ceiling, normalized absolute/multibyte origins, and static
drive/network/pmem route templates.
Deterministic worker gates cover queue full/disconnect, timeout before dequeue
and during write, zero/short/`EAGAIN`/write/flush outcomes, a genuinely full
FIFO, and exact per-record accounting. Replacement tests cover initial spawn,
same-worker success, cancellation/commit races, held writer, repeated queue
pressure with one worker generation, stale clones, disconnected recovery, and
path-free retention. They also prove that API, action, startup, and guest boot
timer MMIO outcomes do not change. API socket tests cover level/origin/module
filters, one result after each parsed dispatch, control-only `400`/`413` parse
rejections, deprecated/completed control seams, discarded connection failure,
and the absence of request bodies, dynamic selectors, paths, or fault text.

Emergency logger tests separately prove one compare-exchange publication,
priority before an ordinary wake message, bounded idle polling, independence
from a full ordinary queue, held and failed writers, disconnect and worker
unwind closure, failure-atomic replacement, a fresh disconnected successor,
late prefix/filter publication, and immediate contended or poisoned attachment
fallback with one deferred loss. Fixed panic and terminal variants are checked
for exact text, level, UTF-8 validity, and the 512-byte ceiling.

Every test that changes the global panic hook self-spawns the exact unit-test
child with one case marker and `--test-threads=1`; the parent pipes output,
enforces a five-second deadline, and kills only a timeout failure. These cases
cover default and sentinel prior-hook suppression/restoration, string and
custom payload redaction, resumed payload identity, pre-attachment and filtered
fallback, an occupied logger ingress, first/second invocation behavior, a
stalled fallback writer, secondary-payload isolation, and true panic-on-drop
double panic. Direct ingress tests add unavailable, occupied, claimed, closed,
and failed fallback outcomes without mutating a hook. The double-panic case
requires only bounded non-success and absence of both secrets; it deliberately
makes no persistence claim.

Metrics transaction tests use injected outputs to cover every implemented
increment family and persistent store, first/no-new/new-event lines, lower/new
producer generations, keyed disappearance and reappearance, independent
owners, sparse omission, saturation, and writes that accept bytes before
returning an error. They prove that only a complete success advances the typed
baseline and that ambiguous failures replay at least once with
`missed_metrics_count`. `metrics_flush_count` is asserted as `1` per successful
line rather than as a cumulative producer.

Process-lifecycle tests cover configuration-origin-independent initial output,
preboot scheduler dormancy, a session-epoch deadline, Running and Paused
periodic output, due work that is not starved by ready API clients, periodic
failure/rearm/recovery, explicit failure propagation, initial/final sink
failure, guest stop, worker terminal error, ordinary server error, exact result
preservation, terminal logger-before-final-metrics ordering, terminal logger
failure accounting, all fixed categories, idempotent finalization, and
independent process ownership. Ordinary real-binary tests additionally prove
normal default logger records on stdout, retention across path-free updates,
unavailable and completely full stdout independence, explicit-target readiness
on blocking stdout, and concurrent process target isolation. Focused process/
startup tests prove stdout-adapter flag isolation from default serial capture
and provided config-file fields/target overriding matching CLI values. The suite
also covers successful API shutdown plus configuration and process failures
after logger setup. The 60-second rule is checked with injected `Instant` values
and due schedulers; tests do not sleep for a production interval.

Run the focused process-observability evidence with:

```sh
cargo test -p bangbang-runtime logger --all-features --locked
cargo test -p bangbang --bin bangbang panic_bridge::tests --all-features --locked -- --test-threads=1
cargo test -p bangbang --bin bangbang terminal_observability --all-features --locked
cargo test -p bangbang --test process_e2e terminal_record --all-features --locked
```

Serial unit tests cover nullable output, the bounded 64-KiB internal TX buffer,
nonblocking file/FIFO behavior, path redaction, exact token-bucket refill/drop
decisions, the 64-byte RX FIFO, DR/OE/RDA/FCR transitions, coalesced typed
interrupt/drain intents, prefix acceptance and recovery, malformed MMIO metrics,
fallible redacted capture/restore, and exhaustive short state-machine sequences.
Process-stdio tests additionally use pipes and a pseudo-terminal to prove
close-on-exec duplication, access preservation, byte-exact raw input, ignored
nonpollable/invalid stdin, and restoration of input/output status flags and
terminal attributes only after the final shared endpoint owner drops. HVF
run-loop tests prove capacity-bounded reads, full-FIFO disarming, guest-drain
rearming, EOF/error detachment, retained interrupt intent after failed GIC
delivery, and no serial side thread.
Focused vectors compare the shared FIFO/DR/RDA/IIR/drain surface against pinned
vm-superio 0.8.1. Snapshot tests prove that bangbang-native v1 keeps its six-byte
UART encoding, constructs a fresh output pipeline with empty metrics, and
rejects nonrepresentable live RX/status/intent state rather than silently
discarding it. It does not preserve public output configuration, TX bytes,
limiter state, counters, or the complete capture-ready UART state.

Default logger-stdout tests use pipes, sockets, and descriptor flag inspection
to prove a close-on-exec nonblocking internal pipe, ordered logger/status
forwarding plus bounded convergence drain, temporary nonblocking-backpressure
recovery on the same forwarder, terminal target failure and later closed-pipe
admission, write-access rejection, and preservation of real stdout flags across
logger replacement and serial capture/restoration. These tests distinguish
exact worker-to-pipe admission accounting from the non-durable downstream byte
hop; convergence never joins a forwarder that can remain blocked until process
exit. Contained output
tests separately cover transferred regular
files: the shared adoption helper rejects non-regular or non-`O_WRONLY`
descriptors, verifies
append/nonblocking status without upgrading access, and appends across multiple
writes. Logger prepare/commit tests cover path-free sink retention and atomic
replacement; metrics tests retain duplicate-before-claim and flush-baseline
ordering; serial/startup tests cover clear/replacement, move-only prepared and
consumed state, one-attempt failure, explicit reconfiguration, and debug
redaction. Direct create/FIFO/open timing remains covered by the original tests.

Production reachability is intentionally narrower than those normative tests.
The existing API-driven and config-file-driven signed executable scenarios each
observe API server/startup control, receipt, closed result, session-initial plus
explicit output before shutdown, and one additional normal-terminal metrics
line and final fixed success logger record after exit.
The normal production-bundle output-grant cases check the same terminal logger
record, while module-filtered concurrent cases prove it is suppressed. The
signed boot-timer scenario proves a guest
magic write reaches the configured logger; signed initrd/direct-rootfs serial
scenarios prove configured public TX output and clear behavior. Dedicated
signed serial-stdio cases use a raw `/dev/ttyS0` guest protocol and an exact
104-byte payload to prove default stdout, 64-byte FIFO backpressure/rearm,
stdin exclusion for configured output, limiter drops, queued input across
pause/capture/resume, EOF with a live API, two-process isolation, metrics, and
clean process termination. Their mixed default-output assertions prove record
reachability but intentionally impose no logger/serial cross-producer ordering;
device tests that require an exact concurrent serial marker protocol configure
an explicit logger file before guest startup. The production-bundle case
repeats default stdin/stdout flow across the launcher/App Sandbox worker
boundary and verifies socket/session cleanup. Its granted logger cases also
exercise representative balloon, virtio-mem, entropy, serial, and time/identity
records without following a replaced source pathname. Signed device cases
otherwise cover representative block, pmem, network/MMDS, vsock, entropy, RTC,
balloon, UART, signal, latency, and startup producers. Guest poweroff/reset
cases separately prove API and no-api terminal process paths. The two-process
MMDS case proves that one process's flush and teardown cannot rewrite its
peer's metrics file.
None of these signed cases claims exact limiter timing, a synchronous footprint
threshold, production telemetry policy, or Firecracker snapshot artifacts.

Hosted macOS CI may use:

```sh
scripts/run-integration-tests.sh --allow-unsupported
```

That option is for CI-style build/sign validation on runners that cannot
execute HVF. Local Apple Silicon verification should omit it so unsupported or
misconfigured hosts fail.

## Guest Boot Artifacts

Guest boot, executable HVF e2e, and production-bundle tests use the pinned
Firecracker arm64 kernel, a deterministic tiny initrd, and rootfs artifacts
where their scenarios require them. The integration runner prepares the
relevant artifacts when `guest_boot`, `executable_hvf_e2e`, or
`production_bundle` is selected. To prepare only the kernel cache, run:

```sh
scripts/fetch-firecracker-kernel.sh
```

The default cache lives under `.tmp/guest-artifacts`. Set
`BANGBANG_GUEST_ARTIFACTS_DIR` to use a different cache root. By default,
`scripts/fetch-firecracker-kernel.sh` stores the pinned kernel at
`.tmp/guest-artifacts/firecracker-ci/v1.15/aarch64/vmlinux-6.1.155`; when a
custom cache root is configured, the same relative path is used under that
root. The script verifies the pinned SHA-256 before reusing or installing the
cached kernel.

The `guest_boot` runner also generates a deterministic tiny initrd under
`.tmp/guest-artifacts/bangbang/guest-boot/` by default. That initrd contains its
own `/init`, so a rootfs drive is not required for the minimal guest boot
integration test. It also contains `/smp-init`, whose raw arm64 syscalls pin PID
1 to CPU1 and verify the observed CPU before emitting its deterministic marker.
The separate `/smp-progress-init` clones a shared-VM child, pins and verifies the
parent on CPU0 and child on CPU1, releases them only after both are ready, and
emits distinct non-ASCII one-byte progress tokens with a brief guest nanosleep
and cooperative yield after each write. Token counts are safe to observe
independently without multi-byte UART interleaving or host-side fixed sleeps.
The baseline test succeeds when the guest emits `BANGBANG_BOOT_OK` on the
internal serial console. The same signed target also includes a raw
virtio-block read scenario: the test configures one temporary drive whose first
sector contains `BANGBANG_BLOCK_READ_OK`, mounts `devtmpfs` from the tiny
`/init`, reads `/dev/vda`, and expects the marker to appear on serial. It also
mounts procfs and writes `/proc/cmdline` to serial between deterministic markers
so a root-drive scenario can verify guest-visible `root=/dev/vda ro` arguments.
A writable virtio-block scenario writes `BANGBANG_BLOCK_WRITE_OK` from the
guest to `/dev/vda`, and the host-side test verifies the marker in a scratch
backing file. A rootfs artifact scenario attaches the cached Firecracker
squashfs as a read-only root drive, mounts it from the tiny initrd, reads
`/mnt/etc/os-release`, and expects `BANGBANG_ROOTFS_READ_OK` plus stable Ubuntu
os-release content on serial. This verifies guest-visible rootfs access through
virtio-block.

The pinned Firecracker CI rootfs artifact can be prepared separately:

```sh
scripts/fetch-firecracker-rootfs.sh
```

By default this stores and verifies
`.tmp/guest-artifacts/firecracker-ci/v1.15/aarch64/ubuntu-24.04.squashfs` and
prints its path. The script verifies the pinned SHA-256 before reusing or
installing the cached squashfs. The upstream Firecracker artifact is a
read-only squashfs; do not mutate it in tests. The signed `guest_boot`
integration target uses this cached squashfs directly for its read-only rootfs
access scenario.

To prepare a local ext4 image from that squashfs, install the local tools and
request ext4 output:

```sh
brew install squashfs e2fsprogs
scripts/fetch-firecracker-rootfs.sh --format ext4
```

Homebrew's `e2fsprogs` package is keg-only, so `mkfs.ext4` is not normally on
`PATH`. The script first looks for `mkfs.ext4` on `PATH`, then checks
`$(brew --prefix e2fsprogs)/sbin/mkfs.ext4`. Set `BANGBANG_MKFS_EXT4` to
override the tool path. The generated ext4 image is stored under
`.tmp/guest-artifacts/bangbang/rootfs/`; tests that need writable rootfs state
should use a scratch copy of that image.

The ext4 preparation path intentionally does not require `sudo`. Files copied
into the generated ext4 image keep the local extraction ownership rather than
Firecracker's root-owned demo ownership. This is suitable for local development
artifacts and is not a substitute for a production rootfs build process.

The signed `guest_boot` and executable HVF e2e targets also validate a
deterministic direct-rootfs boot. For those scenarios,
`scripts/run-integration-tests.sh` prepares
`.tmp/guest-artifacts/bangbang/rootfs/ubuntu-24.04-512M-direct-boot-v100.ext4`
after confirming the host can execute HVF. The generated image is an ext4 copy
of the pinned Firecracker rootfs with a test-specific
`/bangbang-direct-rootfs-init` script added before image creation. The test
boots without the tiny initrd, attaches that ext4 image as a read-only root
drive, and passes `init=/bangbang-direct-rootfs-init`. The `guest_boot` target
expects deterministic serial markers plus Ubuntu os-release content from
`/etc/os-release`; one direct-rootfs executable HVF e2e scenario configures
public `/serial` output and waits for `BANGBANG_DIRECT_ROOTFS_BOOT_OK` in the
host output file. Most other direct-rootfs executable HVF e2e scenarios observe
guest success through a second writable scratch drive, using markers such as
`BANGBANG_DIRECT_ROOTFS_BLOCK_OK`, because they do not configure a public serial
output path. With `bangbang.memory-hotplug-snapshot=1`, the script uses separate
output, continuation, and reprobe sectors. It discovers the real `virtio_mem`
device, retains and verifies the pressure-backed nonzero mapping described
above, offlines aperture blocks, coordinates requested-size changes, unbinds
and rebinds the driver, requires a real UNPLUG_ALL/replug transition, and
publishes bounded success/failure markers for every restored topology stage.
When the boot args also include `bangbang.mmds-fetch=1`, the same
init script configures the
first non-loopback guest interface with a link-local address, runs a bounded
`curl` request for `/meta-data/bangbang-marker`, and writes
`BANGBANG_MMDS_GUEST_FETCH_OK` to the scratch drive only after the expected
MMDS value is returned. With `bangbang.virtio-net-semantics=1`, the script
instead uses one corked Python socket write plus a temporary low advertised MSS
at MTU 1500 for the bounded multi-segment request, renews its v2 token, and
restores the normal route before requesting the large response. The signed
harness drops its final cumulative ACK while the guest keeps that connection
open for five seconds, verifies retransmission, then changes the live MTU to
50000 and validates every byte of a second merged large response. With
`bangbang.mmds-v2-fetch=1`, it first requests a v2 token from
`/latest/api/token`, then fetches the same marker with the token header and
writes `BANGBANG_MMDS_V2_GUEST_FETCH_OK`. The init script emits only static
success or failure markers for this path; it must not print generated tokens or
metadata values. With `bangbang.mmds-multi-fetch=1`, it instead finds two guest
interfaces by their configured MAC addresses, gives them distinct link-local
`/32` source addresses, replaces the MMDS host route before each device-bound
request, and writes the `eth0` and `eth1` results to separate fixed sectors of
the scratch drive. The host requires both static success markers under one
deadline and checks that both API interface metric objects report RX and TX
activity. This MMDS-only scenario does not open direct vmnet resources or need
the restricted networking entitlement. The process-specific MMDS boot modes
extend that protocol to two concurrently running signed executables with
unique instance IDs, API sockets, interface IDs, metadata, metrics, and scratch
drives. Each guest obtains a 48-character standard-Base64 v2 token, verifies
its own metadata, stores the opaque token in a reserved scratch sector, and
publishes only a static token-ready marker. The host pauses both VMs, reads
exactly 48 bytes from each token sector without including them in diagnostics,
writes each token into the peer's reserved sector, publishes a static
continuation marker, and resumes both VMs. Each guest must receive
`401 Unauthorized` for the peer token and then re-fetch its own value
successfully with its original token. The second guest next verifies its initial
process-local release state and writes the existing ready marker before the
host pauses it. After the first guest succeeds and its process exits, the host
patches only the surviving process's release field and resumes it; that guest
must again fetch its original value with the same token before writing a
distinct terminal marker. Bounded kqueue-backed marker waits replace fixed
sleeps, and
the test verifies that each metrics file contains only its own interface key,
that peer flush/teardown cannot rewrite it, and that API socket cleanup cannot
remove or stop the survivor. Dynamic tokens are added to the redaction set as
soon as the host reads them and must be absent from both process stdout and
stderr. Tokens, metadata values, scratch bytes, private paths, and raw worker
output are excluded from failure diagnostics. Both
interfaces are completely covered by their process-local MMDS configuration,
so this concurrent scenario also stays on MMDS-only packet I/O without the
restricted networking entitlement. When the boot args include
`bangbang.network-hotplug=1`, the init script records the startup network BDF,
removes that function, and uses fixed control-drive sectors to coordinate two
host mutation rounds. For each round it rescans PCI, finds the configured MAC
and `1af4:1041` identity, requires the original BDF, configures a link-local
route, fetches the expected MMDS value with bounded curl timeouts, removes the
function through sysfs, and publishes only a static success/failure marker.
When the boot args include
`bangbang.entropy-read=1`, the same
init script checks `/sys/class/misc/hw_random/rng_current` for `virtio_rng`,
reads bytes from `/dev/hwrng`, and writes
`BANGBANG_ENTROPY_GUEST_READ_OK` only after the read returns non-empty data.
When the boot args include `bangbang.entropy-lifecycle=1`, it performs the
marker-gated lifecycle used by #1475: validate `virtio_rng`, publish
`BANGBANG_ENTROPY_LIFECYCLE_READY` after the first nonempty read, wait for
`BANGBANG_ENTROPY_HOST_CONTINUE` in the next scratch sector, perform eight
additional bounded nonempty reads, and publish
`BANGBANG_ENTROPY_LIFECYCLE_OK`. Static failure markers classify driver,
first-read, control, and repeated-read failures without copying entropy bytes
into diagnostics.
When the boot args include `bangbang.balloon-check=1`, the same init script
checks the virtio bus for a device bound to the `virtio_balloon` driver and
requires the device's negotiated feature bitmap to include free-page reporting
bit 5 before writing `BANGBANG_BALLOON_REPORTING_GUEST_CHECK_OK`. The signed
host test separately polls `/balloon/statistics` until `actual_pages` is nonzero
and uses public `FlushMetrics` requests until
`balloon.free_page_report_count` is nonzero before accepting the scenario.
When the boot args include `bangbang.memory-hotplug-check=1`, the same init
script checks the virtio bus for a device bound to `virtio_mem`, writes
`BANGBANG_MEMORY_HOTPLUG_GUEST_READY` after observing requested size zero,
follows `dmesg` for the 128-MiB requested-size transition, writes
`BANGBANG_MEMORY_HOTPLUG_GUEST_GROWN`, and writes
`BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK` only after a final transition back to
zero. The host-side e2e advances on those markers, sends the grow and shrink
`PATCH /hotplug/memory` requests, and requires public requested and plugged
sizes to complete `0 -> 128 MiB -> 0`.
When the boot args include `bangbang.remaining-device-certification=1`, the
wrapper instead runs the aggregate protocol before the single-device profiles.
It requires an exact `bangbang.expect-remaining-device-transport=mmio|pci`
selector, checks either `pci=off` plus at least five virtio-mmio nodes or the
exact five PCI virtio identities, and emits phase/failure markers for memory
hotplug, time/identity discovery, entropy, balloon, and greater-than-FIFO serial
input. Host tests use control-drive sectors and public API state as the source
of progress; no entropy bytes, serial bytes, raw PVTime values, or paths appear
in failure diagnostics. The final guest stdout success line is progress rather
than a durability boundary: the host then uses a bounded kqueue-backed wait for
the exact sector-5 control marker written with `fsync`, followed by an
independent exact-byte assertion.
When the boot args include `bangbang.rtc-check=1`, the same init script checks
that Linux exposes `/dev/rtc0` as a character device and finds PL031 RTC
evidence in sysfs, procfs, or dmesg before writing
`BANGBANG_RTC_GUEST_CHECK_OK`.
When the boot args include `bangbang.vmgenid-check=1`, the same init script
checks Linux device-tree evidence for `/vmgenid`, verifies the
`microsoft,vmgenid` compatible string and 16-byte `reg` property tuple, and
writes `BANGBANG_VMGENID_GUEST_CHECK_OK`.
When the boot args include `bangbang.vmclock-check=1`, the same init script
checks Linux device-tree evidence for a Firecracker-shaped `amazon,vmclock`
`ptp@...` node, verifies its 16-byte `reg` property tuple, checks that the
guest-visible region size is 4 KiB, and writes
`BANGBANG_VMCLOCK_GUEST_CHECK_OK`.
Startup VMClock restore and interrupt coverage is still intentionally limited:
runtime tests verify the initialized ABI fields, HVF unit tests verify
deterministic SPI allocation, and signed executable coverage proves only guest
visibility at startup. Do not treat this as signed guest VMClock restore or
generation-counter coverage.
When the boot args include `bangbang.block-writeback-flush=1`, the same init
script opens `/dev/vdb`, writes a deterministic pre-flush marker, calls `fsync`
on that block-device file descriptor, and writes
`BANGBANG_BLOCK_WRITEBACK_FLUSH_OK` only after the flush call returns.
When the boot args include `bangbang.pmem-read-flush=1`, the same init script
finds the first `/dev/pmem*` block device, reads a deterministic host marker,
writes a deterministic guest marker at a fixed offset, runs `sync` for the
device path, and emits `BANGBANG_PMEM_READ_FLUSH_OK` only after those steps
complete. The signed executable scenario configures a valid initial pmem
limiter and applies a live partial replacement through `PATCH /pmem/{id}`;
deterministic unit tests cover throttle timing, cursor retention, and retry.
When the boot args include `bangbang.pmem-root=ro` or
`bangbang.pmem-root=rw`, the init requires `/dev/pmem0`, the exact
`root=/dev/pmem0` command-line argument, and the matching root mount mode. The
read-only case proves a write fails; the writable case writes, reads, and syncs
a root-filesystem probe before emitting its mode-specific success marker.
When the boot args include `bangbang.vsock-guest-connect=1`,
the same init script uses the rootfs-provided Python `AF_VSOCK` support to
connect to host CID 2 on the test port, stream and incrementally verify exactly
1 MiB of deterministic content in each direction using bounded 16-KiB chunks
with a host Unix listener at the Firecracker-style `uds_path_<PORT>` path, and
write `BANGBANG_VSOCK_GUEST_CONNECT_OK` only after every byte and aggregate
count matches. After both fixed-length directions complete, the host
write-half-closes; the guest verifies all reverse bytes, write-half-closes, and
requires clean EOF before publishing success. The signed e2e then requires host
EOF and process-owned listener cleanup. With
`bangbang.vsock-guest-multistream=1`, Python opens two guest-initiated
AF_VSOCK streams to distinct host ports before payload exchange, sends distinct
guest payloads on both streams, waits for distinct host replies, and writes
`BANGBANG_VSOCK_GUEST_MULTISTREAM_OK` only after both streams complete. When
the boot args include `bangbang.vsock-snapshot-reset=1`, Python instead keeps
one guest-initiated connection blocked while the harness proves pause alone
does not close it. The harness invokes public exact-2.12 snapshot creation while
paused and requires durable state and memory artifacts after the production
vsock capture has published `TRANSPORT_RESET`, captured state, and detached
source-only work. A fresh signed process then loads the immutable pair, first
through a Paused commit with the original selector and then through automatic
resume with an overridden selector. The restored guest must acknowledge
termination of the old socket, connect to a distinct host port, and complete a
fresh marker/ack exchange before writing
`BANGBANG_VSOCK_SNAPSHOT_RESET_OK`. The Paused destination is recaptured, and
separate signed MMIO and product-PCI plus normal production/App Sandbox cases
exercise the sequence and listener cleanup.
With `bangbang.vsock-snapshot-certify=1`, the rootfs instead runs the independent
2.12 completion protocol. It retains one stream in each direction, publishes a
source-ready marker, observes both restored resets, and then runs four fresh
guest-to-host plus 16 prebound-listener host-to-guest deterministic 4-KiB
streams. Exact per-stream markers expose reply integrity, half-close/EOF,
listener completion, cursor order, and final success without writing a shared
control drive. Serial markers are emitted in bounded chunks so the guest
protocol cannot block on UART backpressure.
When
the boot args include `bangbang.vsock-host-connect=1`, Python instead binds and
listens on the test AF_VSOCK port, writes
`BANGBANG_VSOCK_HOST_CONNECT_READY` only after the guest listener is ready,
accepts the host's Firecracker-style `CONNECT <PORT>` request through the main
`uds_path` after the host consumes the `OK <local_port>` response, exchanges
and incrementally verifies the same exact 1-MiB deterministic streams, and
writes `BANGBANG_VSOCK_HOST_CONNECT_OK` only after every byte and aggregate
count matches. The guest sends its full stream and immediately write-half-closes;
the host verifies that stream before sending its full reverse stream and
write-half-closing. The guest then verifies the reverse stream and host EOF,
and the host finally requires guest EOF. With `bangbang.vsock-host-multistream=1`,
Python binds two guest AF_VSOCK listeners on distinct ports, reports ready only
after both listeners are active, accepts two host `CONNECT <PORT>` streams
through the main `uds_path`, sends distinct guest payloads on both streams,
waits for distinct host replies, and writes
`BANGBANG_VSOCK_HOST_MULTISTREAM_OK` only after both streams complete. These
checks prove the kernel mounted the virtio-block root drive as `/`, give
executable-boundary MMDS v1/v2 fetch coverage through the process-local
MMDS-only packet path, and pause, traverse the complete network capture-ready
producer, and resume after real guest exchange plus live limiter updates. The
capture validates transport, generation, queue, features, limiter, metrics,
and MMDS identity while excluding live TCP/ARP/output/timer state. The checks
also prove guest-visible virtio-rng reads plus limiter retry and
capture-ready MMIO/PCI ownership through `/dev/hwrng`, prove
guest virtio-balloon driver binding, prove guest-visible virtio-mem driver
binding plus a guest-completed and public-API-observed requested/plugged
`0 -> 128 MiB -> 0` lifecycle, prove guest-visible PL031 RTC
device discovery, prove guest-visible VMGenID device-tree evidence, prove the
current writeback virtio-block flush path, prove the current virtio-pmem
read/flush path, and cover guest-initiated plus host-initiated virtio-vsock
connection exchange through the signed executable, including sustained
bidirectional streams and multi-stream retention in both directions. They
do not claim that bangbang can boot an arbitrary distro image through its
default init, that full networking compatibility is complete, that RTC alarm
interrupts, KVM's ARM steal-time attribute, distinct-host clock portability,
or broader RTC-adjacent behavior beyond the checked
PL031/VMGenID/VMClock/PVTime contract is supported, or that full
block, balloon, memory-hotplug, pmem, and vsock runtime behavior is complete.
Exact native-v2 2.11 network/MMDS encoding, restore, and fresh clone-local
sessions have their own signed certification; live-peer migration, external
vmnet connectivity, and broad cross-host policy remain outside that proof.
Entropy is terminal only for the
exact native-v2 2.8 contract and evidence above; it does not claim Firecracker
artifacts or broad portability. The network producer intentionally requests a
fresh lossy destination rather than serializing peer packets, callbacks, active
protocol sessions, or source clock deadlines.

For vsock specifically, this evidence validates the **implemented supported live MMIO-or-PCI startup/Unix-socket subset**:
dynamic 64-KiB credit windows with wrapping
counters, two-second request/shutdown cleanup, one 1023-connection active
budget shared across both initiation directions, a separate 256-entry
incomplete-host-handshake bound, round-robin host-local-port allocation,
`EVENT_IDX`, ≥1-MiB bidirectional signed transfer for both initiation paths,
two-stream isolation, and process-local Unix-listener ownership with
path/payload-redacted transport diagnostics. Indirect descriptors are a
supported bangbang extension. Focused runtime tests additionally validate the
real event queue's reset payload and used-ring transaction, EVENT_IDX state,
mandatory MMIO queue intent and PCI queue-2 delivery, typed empty/malformed
failures and metrics, runtime-only restored-origin acknowledgement gate, TX
progress, preserved RX work, post-ack drain, and EVENT_IDX rearming against a
pre-filled event ring. Repeated pre-boot `PUT /vsock`
replaces stored configuration and post-start PUT is stably rejected; PATCH,
DELETE, runtime hotplug, and broader CID routing are not supported. Focused
capture tests now cover repeatable inactive/active MMIO values, endpoint-locked
PCI state including masked MSI-X reset intent, all three saved queue cursors and
`EVENT_IDX`, smaller valid queue sizes, malformed identity/feature/activation/
ring/range/cursor/reset mutations, redaction, and
listener/connector-parameterized reconstruction with empty live work and an armed
RX gate. Production quiesced traversal cross-checks the controller, runtime,
HVF MMIO-or-PCI owner, CID, selector, placement, activation, metrics owner, and
guest memory; it keeps reset-attempt and source-normalization evidence separate
while detaching connection work only after validation and retaining the source
listener/connector for fresh traffic. Signed HVF coverage exercises
inactive, published, empty-queue, cancellation, ack, and both transport owners.
The activation process tests prove the real Linux reset/reconnect boundary and
exact native-v2 2.12 kind-13 artifact creation. Focused destination tests prove
pure captured/override selector resolution before resource access, owner-only
stale-safe direct publication and exact cleanup, transactional contained
directory/broker reservation with no ambient fallback, cancellation rollback,
single-use runtime consumption, retryable preactivation failure, and terminal
postactivation failure. The strict #1736 direct MMIO/PCI tests add simultaneous
immutable original/override clones, Paused work gating, preserved listeners,
four guest-to-host plus 16 host-to-guest deterministic 4-KiB streams,
half-close/EOF, exact clone-local cursor continuation, recapture, fresh metrics,
and cleanup. The strict normal-production/App Sandbox MMIO/PCI test adds exact
grant authority, replacement, malformed/no-device/missing-authority failures,
cancellation, both death orders, retry, redaction, containment, and
session/socket/helper cleanup. The checked vsock ledger certifies all 14
API/live/snapshot records. This is a bounded Bangbang-native compatibility/
progress gate, not general performance, Firecracker artifact, live-peer
migration, or unconstrained portability evidence.

The production-bundle socket-directory cases exercise the same guest protocol
through contained host authority. Host initiation enters through the supplied
granted main listener. Guest initiation keeps queue, credit, routing, and
shutdown state in the worker but asks the already authenticated launcher only
for one relative `<SocketChild>_<port>` connection at a time; the launcher never
receives payload bytes. API-only and direct-path cases keep that broker dormant.
These tests prove the narrow fixed facet, not general dynamic brokerage,
outbound-network entitlement, cross-filesystem publication, or hard revocation.

For Network/MMDS specifically, this evidence validates the supported
public MMDS-only subset over the selected startup transport: guest-visible MTU,
MMDS v1 and v2 through API and
metadata-file/no-api startup, limiter-driven guest progress without a second
queue notification, two MAC-selected interfaces, and two process-local V2
token/value/session/metrics/cleanup domains with post-peer-exit survivor
progress. The signed cases use bounded marker/event synchronization, redact
private values and diagnostics, select every configured interface in MMDS
config, and therefore do not open vmnet or require its restricted entitlement.
The direct-rootfs MMIO and PCI cases also renew a v2 token, receive the
49,152-byte response as multiple TCP segments, deliberately suppress one guest
ACK, observe retransmitted response bytes after the protocol deadline, and then
resume ACK progress without an external network credential.
The separate hidden PCI conformance case and the product all-virtio case reuse
the same authority-free MMDS packet implementation and prove a modern
virtio-pci network endpoint. The direct and contained two-round hotplug gates
add Running/Paused PUT, rescan, real MMDS exchange, sysfs removal, DELETE,
live-config projection, exact BDF/capacity reuse, and clean shutdown; the
contained case proves this needs no vmnet entitlement and that unauthorized
non-MMDS insertion rolls back. The tests do not execute direct-vmnet external
connectivity. The networkless production test additionally rejects positive
host, shared, and bridged launch policies before any session is created.
Unsigned injected-system, runtime, transport, HVF-loop, and
process-registry tests cover returned MAC/MTU/maximum-packet/UUID/batch
reconciliation, allocated-MAC uniqueness, finite start/stop deadlines,
packet-event enable/disable/drain ordering, callback storms and closed/full wake
channels, pre-bind wakeups, stale-generation reuse, exact-interface readiness
before the first vCPU step, zero/full/partial/malformed batch counts,
one-batch-per-pass RX caching, publication-safe staged TX, MMDS effect order,
partial results, and terminal cleanup uncertainty. MMDS-only entries open no
vmnet backend and register no packet callback or bridge work. Packet-available
callbacks and bounded batch dispatch are therefore implemented but remain
outside positive signed vmnet evidence because the signed cases intentionally
open no vmnet interface. Portable checksum/segmentation semantics, merged RX,
ring behavior, limiter/backend metrics, bounded MMDS TCP sessions, and merged
protocol/limiter scheduling have focused unsigned coverage plus signed MMIO and
PCI MMDS-only packet-path evidence. Exact native-v2 2.11 network/MMDS encoding,
restore, and clone-session freshness have the separate signed direct and
contained matrix above. Positive direct-header vmnet I/O, automatic PCI
notification, and external connectivity remain outside the signed boundary as
described by their owning issues.

For block specifically, this evidence validates the supported public
file-backed subset over MMIO by default or PCI with `--enable-pci`, including
initial attachment, guest I/O, root/data ordering, cache/flush behavior,
runtime refresh and limiter updates, and PCI-only non-root runtime
PUT/bodyless DELETE. Normal production-bundle evidence additionally validates
exact read-only/read-write drive-grant adoption, one-time identity,
failure-atomic public state, preauthorized live refresh, and runtime attach
from exact unused initial grants without ambient path reopening. The two-round
direct and contained hotplug cases prove guest PCI rescan, seed read,
write/readback/fsync, sysfs removal, Paused DELETE/PUT ordering, exact capacity
reuse, success-only config projection, and clean shutdown.

Block identity has independent host/guest proof. Unit tests derive the expected
20 bytes directly from backing metadata, cover Sync-to-Async and path refresh
commit/rollback behavior, and validate current, legacy, and unrelated
native-v1 IDs. Signed direct evidence compares a default MMIO/Sync data
backing's host metadata with Linux `/sys/block/vdb/serial`. Signed production
evidence repeats the comparison for a PCI/Async rootfs after the launcher has
opened the grant and its source pathname has been replaced, using
`/sys/block/vda/serial` to prove the contained descriptor identity.

The internal regular-file asynchronous executor has a separate deterministic
unsigned gate under `block::async_executor`. Its injected-host tests cover the
fixed task and staging budgets, completed-but-unapplied lease ownership,
multi-chunk progress, write snapshots, read staging and dirty publication,
partial/error byte counts, same-drive conflict and flush barriers, cross-drive
parallelism, stale generations, discard and cache-sensitive final flush,
worker-panic recovery, non-owning handle cleanup, pipe saturation, and Darwin
`kqueue` readiness clearing. Shared-runtime tests additionally cover lazy
single-pool construction, multiple generation routing, foreign-completion
parking without retaining global leases, selected-generation quiescence, and
readiness re-arming when another device becomes publishable during the same
monitor pass. The focused
`boot_run_loop_supervisor_stays_responsive_while_async_block_host_call_blocks`
test additionally holds a host call inside the block pool and requires a
second owner command to finish before that call is released. Public
`DriveIoEngine::Async` is also covered end to end: process tests prove API and
configuration projection; signed executable cases prove MMIO live path PATCH,
config-file startup, concurrent Async root/data drives, first-use PCI hotplug,
DELETE/reuse, and paused same-ID Sync-to-Async replacement; signed production
cases prove contained Async root/control startup, preauthorized same-ID backing
and engine replacement, limiter PATCH, and runtime hotplug/delete/reuse.
Native-v1 serialization remains Sync-only: a paused Async create completes live
storage preflight and reopens the same generation before the explicit profile
gate rejects it without artifact creation.

Capture-ready storage changes additionally require focused block, pmem, Async,
and virtio-pci tests for exact value equality and redaction; atomic
stop-all/drain-all/publish-all/capture-all/resume-all ordering; foreign
completion routing; ordered MMIO SPI and PCI MSI-X completion delivery with
per-drive failure metrics; pressure/counter retention; partial-publication and
reopen failure classification; and cancellation recovery. Process tests must
prove the traversal precedes profile rejection, contained output claims, and
staging. The signed
`capture_ready_storage_traverses_signed_mmio_and_pci_owners` case must cover
startup/runtime MMIO/PCI Sync, Async, and direct pmem ownership and
same-generation reopen. Signed executable scenarios must retain the public
preflight gates for Async, direct pmem, dynamic PCI block/pmem, and typed
path-redacted vhost rejection with no visible artifact or staging entry.

Direct pre-boot vhost-user block has its own signed executable gate. The MMIO
case first connects an intentionally incompatible backend and proves that
discovery failure leaves the instance unstarted, then retries with a valid
backend and boots a read-only socket root. The PCI case boots a writable
MBR-partitioned socket root through `PARTUUID` and checks both expected PCI
identities. Both cases use a second exact-eight-sector scratch device and prove
host-seed reads, guest direct synchronous write/readback, FLUSH, exact
socket-only `GET /vm/config`, one complete shared-memory export, one 256-entry
queue, backend-call interrupts, snapshot rejection before staging, backend
death metrics, continued API responsiveness, frontend close, and socket
cleanup. The MMIO case additionally resizes its scratch backing, uses ID-only
PATCH to fetch the second exact config, and makes Linux observe and write the
new capacity through a real SPI notification. Across the direct and production
dynamic MMIO/PCI cases, vhost and virtio-mem are configured in both possible
pre-boot orders, storage I/O runs before memory growth and again while the guest
has completed a 128-MiB plug, and shrink completes back to zero. Each backend
must receive exactly one immutable memory-table request containing boot RAM plus
the exact configured aperture, while public plugged size and guest markers
complete `0 -> 128 MiB -> 0`. The product-PCI lifecycle repeats
capacity refresh through MSI-X, rejects invalid negotiation without
publication, attaches a new non-root backend, performs guest read/write/fsync,
manually removes and DELETEs the function, then repeats the same ID and released
slot while Paused. Its dynamic-memory variant grows before runtime insertion,
proves the initial, inserted, and reinserted backends all receive the same exact
table, and shrinks after final DELETE without changing the surviving control
backend's table. The ordinary anonymous-memory case proves a candidate vhost
listener sees zero connections and no public mutation; duplicate IDs likewise
reject before connection.

The signed production-bundle gate separately supplies one repeatable
connect-only vhost-user directory grant. One normal sandboxed worker boots from
an exact vhost root with virtio-mem configured first, performs real I/O and
flush through a scratch child before and during the completed grow/shrink
lifecycle, verifies the same exact one-table aperture geometry, coexists with
the independent vsock authority, refreshes guest-visible capacity over the
existing stream, retains unchanged App Sandbox plus Hypervisor entitlements and
no steady-state helper, and closes both streams. A second all-PCI dynamic-memory
guest grows before runtime vhost insertion, then proves invalid-target and
negotiation rollback, exact table geometry, runtime attach and guest I/O,
duplicate zero-connect rejection, manual removal, DELETE, Paused same-ID reuse
through another exact child, resumed I/O, final DELETE, shrink, stable control
table, and complete control/runtime closure. Unit and
process tests additionally cover exact grant and child parsing,
lifecycle/session/sequence correlation, malformed or extra SCM_RIGHTS
rejection, anchored no-symlink/current-user/socket/single-link validation, cwd
restoration, retry after a normal broker failure, startup zero-request
preflight, runtime zero-request owner preflight, multiple children, ID-only
PATCH, duplicate PUT, DELETE lease release, and same-ID reinsertion.
The fixture rejects a second memory-table request and reports only bounded guest
addresses, sizes, and file offsets; it never reports host addresses, paths,
descriptor numbers, or payloads. Focused tests cover reservation/view geometry,
active accounting, exact discard offsets, dirty add/remove, resource limits,
rollback with the same retained mapping, backend death, pause/run, deletion,
and descriptor/slot/child reuse. Same-ID vhost replacement without DELETE,
automatic guest PCI notification, and vhost snapshot state remain outside the
combined direct and contained vhost subset. File-backed Async uses the portable
session executor described above rather than Linux io_uring.

The `bangbang-vhost-user` crate retains a portable protocol boundary.
Native-endian golden tests cover the exact pinned
owner/features/protocol/config/memory/vring request IDs, flags, lengths, zero
padding, and CONFIG/REPLY_ACK replies. Fault-injected senders and real Unix
streams cover partial progress, SCM_RIGHTS lifetime/CLOEXEC, fragmentation,
timeouts, wrong replies, cleanup, and terminal poisoning. Pipe tests cover
exact eight-byte units, saturation coalescing, malformed units, EOF/EPIPE, and
Darwin kqueue readability. Runtime tests additionally prove feature
intersection, exact config preservation, shared-memory and ring bounds,
pre-activation reset, Firecracker's pre-acknowledged protocol bit, activation
order, calls/kicks, disconnect terminalization, and snapshot/update rejection.
The active peer also polls repeated post-activation CONFIG requests; focused
tests prove exact replacement, generation/interrupt publication, malformed
reply preservation, optional config-change latency metrics, and generation-safe
removal/reuse.
The signed fixture is a separate strict regular-file backend that maps only
transferred regions and validates direct/indirect guest descriptors; it is test
infrastructure, not a shipped storage service.

bangbang appends Firecracker-style root-drive command-line arguments during
startup resource assembly when a configured drive has `is_root_device=true`.
Root drives with `partuuid` append `root=PARTUUID=<partuuid>`; other root
virtio-block drives append `root=/dev/vda`. Read-only root drives append `ro`,
and writable root drives append `rw`. Rootfs boot tests should still pass the
other boot args they need, for example:

```sh
console=ttyS0 reboot=k panic=1 pci=off
```

The VMM supplies `pci=off` when the default MMIO transport is selected, so new
product tests normally should not duplicate it. PCI-mode tests must omit it and
may use a separate guest-test selector only to choose fixture assertions.

Set `is_read_only=true` when attaching the cached squashfs rootfs so the guest
receives `ro`. Use writable root mode only with a scratch copy of the generated
ext4 image.

## PR Expectations

Bug fixes should include a regression test unless the behavior cannot be tested
practically in the current scaffold. New public behavior should be tested
through the public CLI, API, crate, filesystem, or HVF boundary that users or
future code will rely on.

List only verification commands that were actually run on the reviewed head. If
a command is intentionally skipped, explain why it does not add useful signal
for the PR.
