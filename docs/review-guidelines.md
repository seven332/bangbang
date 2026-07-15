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
reads. If a path may reference a FIFO, device node, socket, or replaced inode,
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

For launcher-worker session changes, review both asymmetric authentication
directions: the launcher must bind the unreaped PID to the expected dynamic
signed worker before resume and again after child-attributed `Hello`; the worker
can require only matching effective credentials, the inherited endpoint, and
`LOCAL_PEERPID == getppid()` because App Sandbox denies its parent-code lookup.
No public/VM/resource side effect may precede random-session `Start` and the
independently validated `Prepared`/grant-ack/`Proceed` gates. Keep lifecycle v2
frames at 4096 bytes or less, sequences exact, message/state variants closed,
diagnostics redacted, and the all-zero identity exclusive to the initial
greeting. Even an empty grant batch must be acknowledged before `Proceed`.

For startup-grant changes, separately review the argv-position-one envelope,
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
hard revocation.

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

The production session owns one empty locked runtime namespace. Review worker
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
