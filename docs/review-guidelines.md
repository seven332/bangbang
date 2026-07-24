# Pull Request Review Guidelines

This document defines the project-specific review standard for bangbang pull
requests. Review changed behavior, not only changed lines, and keep each review
scoped to the issue and capability being implemented.

## Review Scope

Start each review by reading the issue, pull request description, changed files,
nearby tests, `AGENTS.md`, `README.md`, and relevant documents under `docs/`.
Separate current PR requirements from future capability work. A PR should not be
blocked for missing unrelated features, but it should document intentional scope
exclusions when they affect compatibility, security, or follow-up design.

Prefer concrete findings over broad style comments. Findings should explain the
failing scenario, impacted file or behavior, and the smallest credible fix.

## Required Verification

Run the repository checks before opening or updating a pull request:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
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

On macOS Apple Silicon, also run `scripts/run-integration-tests.sh` for signed
HVF-backed integration targets. These tests should not be skipped or ignored on
hosts that support HVF. Hosted CI may use
`scripts/run-integration-tests.sh --allow-unsupported` to validate build/sign
behavior without executing HVF when the runner does not support it.
Changes to signing, entitlements, host-resource policy, the launcher, or macOS
isolation must retain both `app_sandbox` and `production_bundle` targets in that
wrapper. The former is the narrow containment-building-block gate. The latter
must exercise the fixed production topology, separately inspect both code
objects, reject modified nested code, and launch a real sandboxed HVF guest.

Reviewers should confirm the PR body lists the checks that were run. If any
command is intentionally skipped, the PR should explain why the skipped command
does not add useful signal for that change.

Do not list verification commands that were not actually run on the reviewed
head. If a command is copied from a template, either run it or remove it from
the PR body.

Add targeted smoke tests when the PR changes process startup, CLI behavior, API
socket serving, signal handling, filesystem cleanup, FFI, or platform gating.
For example, API server changes should usually be exercised with a real Unix
socket request, not only by calling parser helpers.

## Correctness and Compatibility

Compare Firecracker-facing behavior against the pinned compatibility baseline in
`docs/firecracker-compatibility.md`. API paths, methods, status codes, response
field names, CLI arguments, exit codes, and validation rules should either match
the documented target or call out an intentional macOS/HVF difference.

Reject unsupported Firecracker options instead of accepting no-op compatibility
shims. Unknown fields, invalid paths, invalid methods, malformed HTTP, and
unsupported state transitions should fail with the documented Firecracker-shaped
error policy.

Check boundary inputs: empty values, duplicate options, invalid UTF-8, malformed
HTTP headers, oversized payloads, missing bodies, duplicate identifiers, and
path-like values that must not be echoed in errors.

For address, size, and range logic, review the exact range semantics. Tests
should cover both accepted boundary values such as `end_exclusive == limit` and
the first rejected value past the limit. Documentation must use the same
inclusive or exclusive language as the implementation.

For prevalidated operations, verify failure atomicity where practical.
Validation, read, placement, and range failures should not partially mutate
guest memory, destination buffers, configuration state, or accepted metadata.

## Security Review

Treat CLI values, API request bodies, identifiers, host paths, and guest input as
untrusted. Review validation, redaction, and ownership checks before any input
can affect host resources or VM state.

Host path handling should be reviewed per resource type instead of assuming one
resource covers another. For example, kernel, initrd, block-device, and socket
paths each need their own missing-path, empty-path, non-regular-file, and
redacted-error coverage when those surfaces are introduced.

File-backed inputs should reject directories and special files before payload
reads unless the resource has an explicit descriptor-kind contract. The macOS
drive exception accepts only an exact block-special descriptor with checked
access, identity, geometry, and cache-control ownership; it does not generalize
to FIFOs, character devices, sockets, ambient device lookup, or physical-media
selection. If a path may reference any special object or a replaced inode,
review whether open/read behavior can block, follow an unsafe replacement, or
leak the path through errors.

The API socket is currently an unauthenticated local control interface. PRs
touching socket behavior must cover filesystem permission assumptions, stale
socket handling, symlink or replacement races, cleanup ownership, and behavior
when multiple `bangbang` processes run concurrently.

Unsafe code belongs behind small FFI wrappers. Every unsafe block must have a
specific `SAFETY:` explanation, and the wrapper should translate platform errors
into project errors without panics.

For production bundle changes, review signing order, exact entitlements and
identifiers, fixed executable placement, strict nested validation, same-volume
exclusive publication, private staging cleanup, and error redaction. Existing
destinations must never be replaced or merged. Static-code validation is an
at-rest tamper gate; do not treat it as atomic protection from concurrent
same-user replacement without a stronger launch-constraint design.
Vmnet packaging must additionally keep the caller profile bounded and
open-once, preserve separate App ID-prefix and Team ID relationships, require
the documented runtime entitlement and an allowed signing leaf, and complete
the disposable same-authorization AMFI probe before publication. The probe may
execute only the already-running package tool's immediate-success command,
never the caller-supplied worker. A blocked preflight is gate evidence, not
connectivity evidence.

For launcher-worker session changes, review both asymmetric authentication
directions: the launcher must bind the unreaped PID to the expected dynamic
signed worker before resume and again after child-attributed `Hello`; the worker
can require matching real/effective credentials, session identity, the inherited
endpoint, and `LOCAL_PEERPID == getppid()` because App Sandbox denies its
parent-code lookup.
No public/VM/resource side effect may precede random-session `Start` and the
independently validated `Prepared`/grant-ack/`Proceed` gates. Keep lifecycle v5
frames at 4096 bytes or less, sequences exact, message/state variants closed,
diagnostics redacted, and the all-zero identity exclusive to the initial
greeting. Even an empty grant batch must be acknowledged before `Proceed`.

For production launch-policy changes, preserve exact argv-position-one
activation and the mandatory policy delimiter, fixed executable/current
credential binding, singleton and forwarded-timing conflict rejection,
last-value `fsize`/`no-file` behavior, the 2048 no-file default, and unchanged
nested grant/worker bytes. `Start(WorkerPolicy)` must remain fixed-size,
reserved-zero, authenticated, and value-redacted. Vmnet changes must preserve
canonical default denial, exact bounded mode/bridge/count grammar, immutable
contained retention, all-MMDS no-authority behavior, and the final pre-resource
admission gate. A positive policy may be admitted only by an exactly validated
matching worker profile; the networkless profile must fail before worker
spawn/resume. The worker must install and read back exact soft/hard limits
without raising an inherited hard bound, then
descriptor-enter and recheck the locked private namespace before `Prepared`.
The exec environment remains a closed marker-only input; platform-created
runtime variables are not caller authority and must not justify forwarding
ambient values.

For daemon-mode changes, require same-code static/live validation, default-close
`SETSID` re-exec, `/dev/null` standard streams, one fixed marker and handoff fd,
kernel peer checks after resume, closed reserved-zero frames, and one absolute
Ready/ack deadline. The printed PID is the still-live supervisor and may appear
only after committed worker readiness plus exact acknowledgment. Original loss
before ack cancels the worker; after ack the handoff closes. Review both crash
orders, signal ownership, PID reuse safety, concurrent sessions, and cleanup.

For startup-grant changes, separately review its position-one envelope for
ordinary launches and its exact position immediately after a jailer delimiter,
strict manifest bounds, component-by-component no-follow opening, role/access
matrix, alias/type/identity checks, and all-before-spawn preparation. Grant
datagrams must remain at most 1024 bytes and bind session, batch, sequence,
payload length, reserved fields, and actual descriptor count. Ancillary parsing
must own every received fd before later validation, reject truncation or
malformed cmsghdr values, set FD_CLOEXEC, and drop the entire staged batch on any
error. Directory bookmarks are allowed only for the closed create-children
roles with an exact anchor, balanced scope, concrete access validation, and no
operator-supplied or persisted bytes. Treat the stale bit as private evidence,
not sufficient validity or invalidity. Registry adoption must be one-time and
typed with no ambient path fallback. Closing a sender duplicate is cleanup, not
hard revocation. Only contained mode may interpret the exact, case-sensitive
`bangbang-grant:<GrantId>` form; direct mode must preserve it as a pathname.
Validate public state and request shape before claiming, and validate every
member of a multi-file claim before removing any registry entry. Keep adopted
boot descriptors beside the public configuration, never reopen a tag, and
separate authorized `GET /vm/config` output from redacted diagnostics. Review
the cancellation/disconnect race against pending claims, descriptor lifetime on
every error path, and the singleton retry rule after boot consumes a grant.
For repeatable block/pmem grants, derive access from the validated immutable
device mode, key retained ownership by configured device ID, preflight every
consumed startup entry before moving any backing, and reject unexpected backing
map entries. Review same-ID `PUT` replacement separately from after-start block
`PATCH`: both commit public state only after the consumer transition, while a
successfully claimed live replacement remains one-time even if the later swap
fails. Path-free block and pmem limiter updates must retain ownership and claim
nothing. Authorized configuration output may contain the submitted tag; all
diagnostics and nested `Debug` output must remain value-redacted.

For logger/metrics/serial sink grants, require exact singleton role plus
write-only access after complete lifecycle/input preflight. Descriptor adoption
must preserve `O_WRONLY`, require an existing regular file, and set and verify
append/nonblocking status without reopening a submitted reference. Review
logger path-free updates as no-claim sink retention with all requested fields
committed together; review metrics repeat initialization before any reference
inspection or claim. Keep serial private ownership synchronized with wholesale
replace/clear config: `Prepared` moves once into startup, `Consumed` blocks
retry before any other resource moves, and a later validated `PutSerial` is the
only reset. Direct path creation, FIFO-like support, and logger/metrics versus
serial open timing must remain unchanged. Signed normal-bundle evidence must
cover source-path replacement, append sentinels, logger/metrics/guest-serial
writes, redacted mismatch rollback, cleanup, and concurrent session isolation.

For snapshot grants, review input and output grammars separately. Describe,
state, and memory use exact file tags and distinct read-only roles; outputs use
`bangbang-grant:<GrantId>/<SnapshotOutputChild>` with one 1–255 byte UTF-8
component, no NUL or `/`, and no `.`/`..`. State preinspection may duplicate
only the exact descriptor and must not consume authority. Decode once, discover
any persisted root tag, then validate and atomically take every tagged state,
memory, and read-only `DriveBacking`; no registry lock should span decode or
memory I/O, and no submitted or persisted tag may be reopened.

Review snapshot output adoption after complete request/profile preflight. The
role is repeatable across distinct grants; a shared grant with distinct children,
two grants, mixed ordinary/granted destinations, and retained reuse must preserve
no-clobber memory-first/state-last semantics. All staging, checks, barriers, and
final renames must stay relative to exact retained anchors. Do not infer that an
open descriptor bypasses App Sandbox security-scope pathname rules: moving the
authorized directory can legitimately deny later writes.

Each granted staging inode needs a strict record before producer content. Check
record-clear ordering on publication, conclusive cleanup, and failure; launcher
recovery must select an anchor by exact directory identity and unlink only a
current-user regular `0600`, link-count-one device/inode match. Missing and
replaced entries must survive. Keep the create-before-record interval and
simultaneous uncatchable process death explicit, and require the hidden
post-record hold to remain test-feature-only.

For API/vsock directory grants, require the distinct exact
`bangbang-grant:<GrantId>/<SocketChild>` grammar, where the child is one bounded
ASCII component and direct mode preserves identical bytes as a path. Review
validation-before-claim, exact singleton role/access, owner-thread scope and
anchor lifetime, no-API non-consumption, API readiness after publication, and
deferred vsock claim/startup failure atomicity. The transient signed binder must
use a default-close fd5/fd6 allowlist, authenticate its parent, bind only the
fixed private staging name, validate and transfer exactly one listener, and be
killed/reaped before exposure. Publication must require matching filesystems,
use fd-relative exclusive rename, reject replacement and traversal races, and
install a strict value-redacted ownership record before the public name exists.

Review the fixed vsock broker separately from general resource brokerage. Its
initial endpoint is always inherited but dormant; activation must bind exact
peer PID, lifecycle SessionId, first sequence, retained singleton anchor, cwd
identity, and the already validated child. Later requests may contain only a
monotonic sequence and `u32` port, producing only relative
`<SocketChild>_<port>` targets and at most one validated connected AF_UNIX
stream descriptor. Require closed frame/reserved/status variants, exact rights
counts, pre/post target identity checks, bounded nonblocking connect, shutdown,
EOF, and fail-closed lifecycle coupling. The launcher must never receive guest
bytes, a grant ID, bookmark, resolved path, arbitrary child, or general selector;
the worker must not gain `network.client`. Direct, API-only, and unused-vsock
sessions must leave the facet dormant.

## Concurrency and Resource Management

Review file descriptors, Unix sockets, temporary files, signal handlers, and VM
resources for ownership and cleanup on success, failure, and shutdown paths.
Cleanup must not delete resources that were replaced by another process.

Look for races, deadlocks, missed wakeups, and transient error handling. Signal
shutdown should not depend on unreachable state, arbitrary delays, or a socket
path remaining available after startup.

The production launcher owns one child. Review signal-versus-reap ordering so a
PID is never signaled after the child has been reaped and could be reused;
ordinary child exits must be preserved, while signal exits use the documented
`128 + signal` mapping.

The production session owns one locked runtime namespace that is empty at the
authorization gate. Review worker
cleanup after launcher EOF, launcher cleanup after worker exit, and bounded
both-killed recovery separately. Every removal must compare exact type, owner,
mode, device/inode, emptiness, pathname identity, and lock state; missing,
replaced, populated, ambiguous, live, or excess entries must be preserved.
Later graceful signals must coalesce behind one cancellation and its bounded
escalation, and concurrent launchers must never share session state or signal,
reap, or clean each other's workers and namespaces. Absolute handshake
deadlines must survive fragmented reads and `EINTR`; `Terminal` or EOF must also
start a bounded owned-process exit grace rather than permitting an indefinite
wait. Grant sending must remain nonblocking in the same event loop, continue to
observe signals, lifecycle input, and child exit, and use one absolute
send-plus-acknowledgment deadline.

Authorized socket construction may transiently add only one fixed role-specific
staging socket. Snapshot construction may transiently add one strict record per
active artifact; successful publication leaves neither. Otherwise the namespace
may contain only the two fixed socket records. Review worker-side normal cleanup and
launcher-side worker-first recovery independently: both must compare the exact
role anchor, safe child or fixed staging name, socket owner/mode/link/device/inode,
record contents, and namespace identity before unlinking, preserve a
missing/replaced/non-socket target, then clear only the matching record.
Simultaneous uncatchable launcher/worker death may leave the documented stale
external name and private ownership record; do not imply unlink-on-close or
populated-namespace recovery.

## Performance Review

Avoid active sleeps, fixed polling delays, and timeout-based tests that make
behavior slow or flaky. Blocking setup work is acceptable for the scaffold, but
future VM and vCPU fast paths should stay free of API parsing, logging-heavy
work, filesystem scans, and avoidable allocation loops.

Resource limits must be bounded and documented. API request size, connection
timeouts, memory sizes, and device queues should have tests for upper-bound and
overflow behavior when introduced.

## Test Expectations

Use `docs/testing.md` as the contributor-facing testing guide. Reviewers should
apply that document when deciding whether a change needs unit coverage, a new
normal integration test, or a signed HVF integration test.

Unit tests live next to the code they exercise under each crate's `src/` tree.
Test public behavior where practical, and add narrower unit tests for parsing,
error formatting, state transitions, FFI wrappers, and edge cases.

Use real isolated filesystem or Unix socket fixtures when reviewing IO behavior.
Fixtures should have unique names, robust cleanup, and no shared global paths so
parallel test processes do not interfere with each other. When cleanup ownership
matters, tests should prove a process does not delete a path or resource it no
longer owns.

Avoid arbitrary sleeps, broad fake clocks, or tests that only verify
implementation details. If platform or privilege requirements make a test unsafe
for normal CI, gate it clearly and document what remains unverified.

When a fix covers a shared helper, add at least one test through a public or
resource-specific path that proves the affected behavior. Boundary tests should
usually include exact-fit success, one-over failure, overflow failure, and
no-partial-mutation assertions for failed reads or writes.

## Documentation Expectations

Behavior changes should update user-facing docs and compatibility docs in the
same PR. Document security boundaries, host-platform differences, unsupported
features, and validation policy when they are part of the changed surface.

Every Firecracker-facing capability PR must also update all owned records in
the checked
[v1.16.0 capability inventory](../compat/firecracker/v1.16.0/README.md).
Review the machine-owned source manifest separately from the human-owned
overlay. Regeneration must not manufacture or overwrite a disposition, owner,
evidence reference, delivery issue, or Challenge result. Require exact
implementation and validation references for `implemented-and-verified`, a
delivery issue for `missing-platform-feasible`, and the complete strict
platform evidence plus current Challenge result for
`proven-platform-impossible`. Keep unresolved behavior `audit-required` rather
than inferring support from parser recognition, stable rejection, historical
issue closure, or family-level prose.

For the checked
[snapshot paging contract](../compat/firecracker/v1.16.0/snapshot-paging-contract.md),
review feasibility and implementation as separate claims. File/COW, eager
population, or parser recognition must never be relabeled as UFFD-equivalent.
Review standalone protocol changes against the normative
[`bangbang-pager-v1` document](snapshot-pager-protocol.md): keep the closed
header/kind set, pre-allocation bounds, nonzero session binding (fresh random
for standalone callers and exact image-ID/checksum/length binding for
native-v1), monotonic request IDs, exact response tuples, terminal
cancellation, drained shutdown, absolute deadlines, poison-on-stream-failure,
and value-redacted diagnostics.
Review `LazyGuestMemory` changes as a distinct ownership boundary: ordinary
initialized-memory APIs must remain unavailable; page metadata must stay
compact; operations and waiters must remain independently bounded; duplicate
faults may coalesce contents but not later permissions; generation validation
must precede scoped publication; and current abandoned work must fail closed.
A population superseded by removal must retain its negotiated protocol slot
until response/drop/terminal retirement. Removal must reserve a different slot
before mutation, serialize with an already-started publication/removal action,
and stay `Removing` until exact acknowledgement commit after local zero and
future protection work. Explicit teardown must wake waiters and drain
already-linearized guards, while destructors remain nonblocking and retain
mapping lifetime safely.
Review the HVF host adapter as a task-wide authority boundary. MIG input must
come from the public active SDK; installation must capture only bad access,
construct all aliases before protection, and roll every partial owner back.
The original mapping must stay hidden until one exact alias page is complete
and ordered. Address ownership and ARM64 fault form must be revalidated before
coordinator/source access. Unowned and unsupported messages must preserve the
captured legacy/Mach behavior, flavor, returned state, and port disposition.
Shutdown must drain admitted callbacks and restore only while still current;
do not claim public Mach provides compare-and-swap restoration or that a task
handler precedes thread-specific handlers. Owned callback failure must retain
the fixed supervised exit, while unrelated faults must never be swallowed.

Review the HVF guest adapter as the second protection owner. Lazy mappings must
start with no stage-two access and become active only after complete
transactional protection. Admit only evidenced data/instruction abort forms,
validate IPA plus instruction VA/PC state, and keep HVC/SYS64 precedence.
Resolve every touched page before any permission; serialize per-page
read/write/execute unions so concurrent vCPUs cannot downgrade one another;
synchronize instruction bytes before execute permission; retry without
advancing PC. One peer-stale exit may count as progress, but repeated
no-progress, resolver, cache, or protection failure must poison the path.
Dirty-write tracking and raw vCPU dispatch must remain mutually exclusive with
lazy guest paging until a reviewed composition exists.

Keep Mach task/thread ports, aliases, and host virtual addresses inside the
VMM, reject unmodified Linux UFFD wire traffic, and require pre-resource
rejection for bypass profiles. Native-v1 `Uffd` may succeed only for the
reviewed macOS Apple-Silicon fixed-memory profile with dirty tracking disabled,
one validated `bangbang-pager-v1` peer, exact state-bound session/layout/source
offsets, and transactional owner construction. Direct mode must connect with a
bounded deadline; contained mode must consume only the exact launcher-connected
pager grant and must not gain snapshot-memory path/file authority. File/COW
must not become an implicit fallback. `corpus:snapshot-page-faults` is terminal
only because #1555 binds signed paused-host and exact restored-guest
instruction/read/write demand, before/during/after removal, peer and process
failure, repeat/cleanup, exact nested entitlement dictionaries, and the full
repository matrix. Changes that weaken any of those gates must demote the row
or add equivalent direct evidence; component-only inference is insufficient.

Run
`cargo run -p bangbang-firecracker-capability-audit --locked -- validate`
when the PR changes Firecracker-facing behavior or inventory data. Final parent
certification additionally runs `validate --final`; ordinary feature PRs must
not weaken that gate to make unresolved records pass.

Avoid overstating scaffold behavior: if a PR adds constants, internal helpers,
or planning docs without public API behavior, describe that narrower state
explicitly.

Documentation should match implemented boundary wording. Prefer precise phrases
such as "must not overlap" over stricter wording such as "must end before" when
end-exclusive equality is accepted.

Do not add generic `Follow-Up Work` sections for routine gaps. Prefer linked
issues for planned work, and keep PR docs focused on the behavior being merged.

## Pull Request Hygiene

Use Conventional Commits. Keep PRs narrow enough to review independently, link
the tracking issue, summarize scope exclusions, and list verification commands
and manual smoke tests in the PR body.

Before requesting review, confirm the worktree only contains intended files.
Before approval, confirm the PR diff does not include unrelated formatting,
generated artifacts, or metadata churn.
