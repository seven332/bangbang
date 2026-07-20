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
`boots_direct_rootfs_and_fetches_mmds_over_modern_virtio_pci_network` through
the same wrapper. They check stable BDF/vendor/device identities in guest sysfs,
perform real block write/`fsync`, pmem read/write/flush, and MMDS curl traffic,
and require programmed, unmasked, distinct queue/configuration MSI-X vectors.
The retained runtime inventory must contain no block, pmem, or network MMIO
registration/FDT node in this mode, and explicit reverse teardown must finish
before VM destruction. Existing MMIO signed cases remain required. These hidden
cases do not by themselves certify the public selector, runtime attach/delete,
guest rescan/removal, hotplug, or PCI snapshot state.

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
and exact PCI lease reuse. Apple-approved vmnet credentials and real external
connectivity remain separate #1351/#1378 gates.

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
and destroy the VM. Accepted signed syndromes are EC `0x24`, WnR set,
CM/S1PTW clear, and exact DFSC `0x07` for initial protection or `0x0f` after
re-protection at a tracker-owned currently protected IPA. Every other encoding
must fail closed and reopen feasibility; tests must not broaden this pair.

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
The lightweight verifier is not a substitute for loader CRC/GPA validation.
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
and delivery-time validation when changing the manifest, overlay, validator, or
any Firecracker-facing capability:

```sh
cargo test -p bangbang-firecracker-capability-audit --all-targets --locked
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
```

The workspace test suite includes an integration test of both checked-in files.
Ordinary tests do not discover or require a sibling Firecracker checkout.

Process-facing changes must also keep the checked
[`process-contract.md`](../compat/firecracker/v1.16.0/process-contract.md)
aligned with production code, executable tests, and the exact overlay records.
The current main-process audit has 21 `implemented-and-verified`, two
`proven-platform-impossible`, and six nonterminal process-family records. #1383
separately promotes seven offline-seccompiler records; #1384 certifies the two
runtime seccomp leaves, five jailer leaves, and `corpus:seccomp` as exact macOS
platform exclusions. #1389/#1390 promote two lifecycle records, and #1391
promotes the vCPU/memory/SMT leaves while certifying exact `2M` and its pinned
hugepages corpus. The complete 417-record delivery inventory is therefore 38
implemented-and-verified, 366 audit-required, three missing-platform-feasible,
and ten proven-platform-impossible. Delivery
validation permits the explicit handoffs, while final validation continues to
reject them. Parser recognition or native snapshot output is not sufficient
evidence for a Firecracker runtime or artifact claim.

After the remaining #1388 slices and the #1408 closure audit, the generated
manifest contains 381 identities and the 418-record overlay contains 70
`implemented-and-verified`, 328 `audit-required`, three
`missing-platform-feasible`, and 17 `proven-platform-impossible` outcomes. The
checked `machine_lifecycle_closure_policy_is_stable` integration test pins the
original 28-record split, all 22 Wave 7 handoffs, the 18 directly promoted API
identities, count arithmetic, and removal of stale future-#1388 summaries. The
[closure ledger](../compat/firecracker/v1.16.0/machine-lifecycle-audit.md)
records the corresponding evidence and later-wave boundaries. Generic source,
reference, and disposition invariants remain owned by the validator itself.

Run the offline compiler's focused surface with:

```sh
cargo test -p bangbang-seccompiler --all-targets --all-features --locked
```

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

Review exact identity changes before updating the machine-owned file. Never use
regeneration to alter `capabilities.json`; missing and stale overlays must be
resolved deliberately. The parent certification gate is:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --final
```

Final mode is expected to fail while any `audit-required` or
`missing-platform-feasible` record remains. Signed HVF integration remains
regression evidence for an inventory-only change and does not promote a record
without record-specific evidence.

## Running Tests

Run the standard workspace checks before opening or updating a PR:

```sh
cargo fmt --all -- --check
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
cargo check --workspace --all-targets --all-features --locked
cargo check -p bangbang-launcher --all-targets --all-features --locked --target aarch64-unknown-linux-musl
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
- exact external snapshot grants creating a native-v1 pair into separate
  output directories, reusing both retained directories for a second successful
  pair, preserving all finals on collision, and keeping same-GrantId concurrent
  source workers in their own directories; granted early description and two
  fresh state/memory/root loads then prove explicit and automatic resume through
  guest `SYSTEM_OFF`;
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
the 255-byte redacted snapshot child grammar. Socket
broker codec tests cover every closed kind, exact fixed frame/reserved fields,
session/sequence/child/port/status encoding, descriptor declarations,
truncation, malformed ancillary data, and value-redacted formatting. The
separate fixed 256-byte `BBU1` vhost-user broker codec covers exact
session/sequence/grant/child/status correlation, one-stream rights, retryable
failures, stale or malformed response rejection, and facet poisoning. Darwin
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
timer equivalence, VMGenID replacement, absent boot-origin metadata, and
continuation from the captured PC. Opaque GIC bytes are asserted nonempty and
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
checks exact `v1.0.0` output for `--snapshot-version` and a valid
`--describe-snapshot`, plus missing, non-regular, oversized, malformed,
truncated, trailing/inconsistent-length, corrupt, unsupported-version,
incompatible-architecture, and incompatible-page-size files. Fixtures use
unique temporary paths; failures must use the bad-configuration exit code,
publish no API socket, and expose neither path nor payload sentinels.
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
API or a Firecracker-shaped config file depending on the scenario, and waits for
the guest to write deterministic markers to host-observable outputs. The
native-v1 snapshot scenario uses a test-only arm64 Image with a valid Linux
header and no rootfs dependency for guest control flow. The guest saves both
halves of VMGenID, writes one UART readiness byte, and loops at the captured PC.
The host polls public `FlushMetrics` until `uart.write_count` changes, pauses and
creates through `/snapshot/create`, checks public collision/no-clobber
redaction, and terminates the source. Two fresh signed processes load the same
immutable pair: one remains paused until public `PATCH /vm`, and one uses
`resume_vm: true`. Guest PSCI `SYSTEM_OFF` is reachable only after a changed
VMGenID is observed, so clean process exit proves VMGenID replacement and
continuation from captured register/memory state without a fixed readiness
sleep. Run just this proof with:

```sh
scripts/run-integration-tests.sh --test executable_hvf_e2e -- \
  macos_arm64::signed_executable_creates_and_restores_native_v1_snapshot_across_processes \
  --exact
```

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
It also includes a direct-rootfs entropy scenario that configures `/entropy`,
checks that the guest selected `virtio_rng` as the current hardware RNG, reads
from `/dev/hwrng`, and writes a host-observable marker only after a non-empty
read succeeds.
It also includes a direct-rootfs balloon scenario that configures `/balloon`,
enables free-page reporting, checks that the guest bound a virtio-balloon driver
and negotiated reporting feature bit 5, exercises the minimal hinting start/stop
command-state APIs, requires the public statistics response to reach nonzero
`actual_pages`, and flushes public metrics until
`balloon.free_page_report_count` is nonzero. The guest writes a host-observable
marker only after driver binding and reporting negotiation are visible. This
proves signed guest inflation and reporting reach the runtime discard owner; it
does not impose a process-footprint threshold.
Runtime `PATCH /balloon` target-size updates are covered by unit, API socket,
and process-session tests that verify stored config updates, active config-space
generation changes, and config interrupt signaling. Guest-reported statistics
queue records are covered by runtime unit, API response, and process-session
tests. Runtime statistics interval updates are covered by unit, API socket, and
process-session tests because they update stored/active interval state without
timer-driven guest
polling. Hinting queue guest-command acknowledgement, automatic host DONE
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
saturating suppression count, clone sharing, independent logger states, and the
single unrestricted recovery warning. Failing, contended, and poisoned sink
tests prove `missed_log_count` and that API, action, startup, and guest boot
timer MMIO outcomes do not change. API socket tests cover level/origin/module
filters and verify that request bodies never reach logger output.

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
preservation, idempotent finalization, and independent process ownership. The
60-second rule is checked with injected `Instant` values and due schedulers;
tests do not sleep for a production interval.

Serial unit tests cover nullable output, the bounded 64-KiB internal buffer,
nonblocking file/FIFO behavior, path redaction, TX register behavior, exact
token-bucket refill/drop decisions, and saturating UART counters. Snapshot
device tests prove that bangbang-native v1 accepts only default serial config,
round-trips serial MMIO metadata and six mutable register bytes, and constructs
a fresh empty output buffer with empty UART metrics. It does not preserve
public output configuration, buffered/in-flight TX bytes, limiter state, or
counters.

Contained output tests separately cover transferred regular files: the shared
adoption helper rejects non-regular or non-`O_WRONLY` descriptors, verifies
append/nonblocking status without upgrading access, and appends across multiple
writes. Logger prepare/commit tests cover path-free sink retention and atomic
replacement; metrics tests retain duplicate-before-claim and flush-baseline
ordering; serial/startup tests cover clear/replacement, move-only prepared and
consumed state, one-attempt failure, explicit reconfiguration, and debug
redaction. Direct create/FIFO/open timing remains covered by the original tests.

Production reachability is intentionally narrower than those normative tests.
The existing API-driven and config-file-driven signed executable scenarios each
observe session-initial plus explicit output before shutdown and one additional
normal-terminal line after exit. The signed boot-timer scenario proves a guest
magic write reaches the configured logger; signed initrd/direct-rootfs serial
scenarios prove public TX output and clear behavior; signed device cases cover
representative block, pmem, network/MMDS, vsock, entropy, RTC, balloon, UART,
signal, latency, and startup producers. Guest poweroff/reset cases separately
prove API and no-api terminal process paths. The two-process MMDS case proves
that one process's flush and teardown cannot rewrite its peer's metrics file.
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
`.tmp/guest-artifacts/bangbang/rootfs/ubuntu-24.04-512M-direct-boot-v35.ext4`
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
output path. When the boot args also include `bangbang.mmds-fetch=1`, the same
init script configures the
first non-loopback guest interface with a link-local address, runs a bounded
`curl` request for `/meta-data/bangbang-marker`, and writes
`BANGBANG_MMDS_GUEST_FETCH_OK` to the scratch drive only after the expected
MMDS value is returned. With
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
unique API sockets, interface IDs, metadata, metrics, and scratch drives. The
second guest obtains a v2 token, verifies its own value plus an initial
process-local release state, and writes a static ready marker before the host
pauses it. After the first guest succeeds and its process exits, the host
patches only the surviving process's release field and resumes it; that guest
must re-fetch its original value with the same token before writing a distinct
terminal marker. Bounded kqueue-backed marker waits replace fixed sleeps, and
the test verifies that each metrics file contains only its own interface key,
that peer flush/teardown cannot rewrite it, and that API socket cleanup cannot
remove or stop the survivor. Tokens, metadata values, scratch bytes, private
paths, and raw worker output are excluded from failure diagnostics. Both
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
executable-boundary MMDS fetch coverage through the process-local MMDS-only
packet path, prove guest-visible virtio-rng reads through `/dev/hwrng`, prove
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
interrupts, mutable VMClock restore signaling or guest observation,
or broader RTC-adjacent time/identity behavior is supported, or that full
block, balloon, memory-hotplug, pmem, and vsock runtime behavior is complete.

For vsock specifically, this evidence validates the **implemented supported live MMIO-or-PCI startup/Unix-socket subset**:
dynamic 64-KiB credit windows with wrapping
counters, two-second request/shutdown cleanup, 256 retained connections per
direction, `EVENT_IDX`, ≥1-MiB bidirectional signed transfer for both initiation
paths, two-stream isolation, and process-local Unix-listener ownership with
path/payload-redacted transport diagnostics. Indirect descriptors are a
supported bangbang extension. Repeated pre-boot `PUT /vsock` replaces stored
configuration and post-start PUT is stably rejected; PATCH, DELETE, runtime
hotplug, and broader CID routing are not supported. Native-v1 snapshot UDS
override, event-queue `TRANSPORT_RESET`, and post-restore RX gating remain the
precise #543 exclusions. The signed transfer is a compatibility/progress gate,
not a general performance, Firecracker artifact, or snapshot-parity claim.

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
token/value/queue/metrics/cleanup domains with post-peer-exit survivor
progress. The signed cases use bounded marker/event synchronization, redact
private values and diagnostics, select every configured interface in MMDS
config, and therefore do not open vmnet or require its restricted entitlement.
The separate hidden PCI conformance case and the product all-virtio case reuse
the same authority-free MMDS packet implementation and prove a modern
virtio-pci network endpoint. The direct and contained two-round hotplug gates
add Running/Paused PUT, rescan, real MMDS exchange, sysfs removal, DELETE,
live-config projection, exact BDF/capacity reuse, and clean shutdown; the
contained case proves this needs no vmnet entitlement and that unauthorized
non-MMDS insertion rolls back. The tests do not execute direct-vmnet external
connectivity, returned MAC/MTU/maximum-packet reconciliation, packet-available
callbacks, broader MMDS TCP behavior, limiter-specific metrics, automatic PCI
notification, or network snapshot state.

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

The internal regular-file asynchronous executor has a separate deterministic
unsigned gate under `block::async_executor`. Its injected-host tests cover the
fixed task and staging budgets, completed-but-unapplied lease ownership,
multi-chunk progress, write snapshots, read staging and dirty publication,
partial/error byte counts, same-drive conflict and flush barriers, cross-drive
parallelism, stale generations, discard and cache-sensitive final flush,
worker-panic recovery, non-owning handle cleanup, pipe saturation, and Darwin
`kqueue` readiness clearing. The focused
`boot_run_loop_supervisor_stays_responsive_while_async_block_host_call_blocks`
test additionally holds a host call inside the block pool and requires a
second owner command to finish before that call is released. These are engine
and wakeup-boundary proofs only: public `DriveIoEngine::Async` remains rejected,
and #1446 owns API activation, queue integration, capability-inventory changes,
and signed guest evidence.

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
new capacity through a real SPI notification. The product-PCI lifecycle repeats
capacity refresh through MSI-X, rejects invalid negotiation without
publication, attaches a new non-root backend, performs guest read/write/fsync,
manually removes and DELETEs the function, then repeats the same ID and released
slot while Paused. The ordinary anonymous-memory hotplug case proves a candidate
vhost listener sees zero connections and no public mutation; duplicate IDs
likewise reject before connection.

The signed production-bundle gate separately supplies one repeatable
connect-only vhost-user directory grant. One normal sandboxed worker boots from
an exact vhost root, performs real I/O and flush through a scratch child,
coexists with the independent vsock authority, refreshes guest-visible capacity
over the existing stream, retains no steady-state helper, and closes both
streams. A second all-PCI guest starts with a contained vhost control device so
shared memory is established, then proves invalid-target and negotiation
rollback, runtime attach and guest I/O, duplicate zero-connect rejection,
manual removal, DELETE, Paused same-ID reuse through another exact child,
resumed I/O, final DELETE, and complete control/runtime closure. Unit and
process tests additionally cover exact grant and child parsing,
lifecycle/session/sequence correlation, malformed or extra SCM_RIGHTS
rejection, anchored no-symlink/current-user/socket/single-link validation, cwd
restoration, retry after a normal broker failure, startup zero-request
preflight, runtime zero-request owner preflight, multiple children, ID-only
PATCH, duplicate PUT, DELETE lease release, and same-ID reinsertion.
Dynamic-memory coexistence, same-ID replacement without DELETE, async/io_uring,
automatic guest PCI notification, and vhost snapshot state remain outside the
combined direct and contained subset.

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
