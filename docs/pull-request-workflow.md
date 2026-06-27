# Pull Request Workflow

This document defines the standard Codex-assisted workflow for bangbang pull
requests. Keep each PR small enough to review independently and tied to one
clear issue.

## 1. Start From Main

Always begin from an up-to-date default branch:

```sh
git switch main
git pull --ff-only
```

Do this before creating, planning, or implementing the next PR-sized issue.

## 2. Select Or Create The PR Issue

Use one PR for one issue. If a suitable issue already exists, use that issue for
the PR instead of creating a duplicate. If no suitable issue exists, use
`$github-workflow:issue-create` to create one.

If the parent issue is broad, create or choose one sub-issue that can be
completed and merged on its own.

The issue should explain:

- what problem this PR solves
- what is intentionally out of scope
- which later work remains in the parent issue

Do not split tests or documentation into separate follow-up issues when they are
needed to make the code change reviewable. They belong in the same PR.

## 3. Plan The Issue

Use `$github-workflow:issue-plan` for the issue before coding. The plan should
read the relevant local code, issue discussion, `AGENTS.md`, README, and docs.
For Firecracker-facing behavior, compare against `docs/firecracker-compatibility.md`
and the relevant Firecracker source before choosing an interface or validation
rule.

Prefer the smallest implementation that makes the issue complete. Avoid adding
future abstractions, public APIs, or compatibility shims that this issue does
not need. Do not implement before the plan is approved.

## 4. Implement The Approved Plan

Use `$github-workflow:issue-implement` after the plan is approved. The
implementation workflow should create or use a branch for the issue, implement
the approved plan, run validation, commit, push, and create or update the PR.

Use `feat/`, `fix/`, `docs/`, or `chore/` branch prefixes that match the
intended Conventional Commit type, for example:

```sh
feat/issue-123-short-name
```

Keep edits scoped to the crate or boundary owned by the issue:

- `crates/bangbang` for process and CLI behavior
- `crates/api` for Firecracker-shaped API parsing and responses
- `crates/runtime` for backend-neutral runtime types and helpers
- `crates/hvf` for Hypervisor.framework-specific behavior
- `docs/` and README for user-facing behavior and process documentation

Update tests and documentation with the behavior change. Public-facing changes
should update compatibility docs when the Firecracker target or current scope
changes.

Do not amend existing commits during review updates. Add a new focused commit
for each fix or documentation update.

Use Conventional Commit messages, for example:

```sh
git commit -m "feat: add vm configuration model"
git commit -m "fix: reject duplicate drive ids"
git commit -m "docs: document PR workflow"
```

## 5. Verify Locally

Before opening or updating a PR, run the repository checks from the workspace
root or as documented:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-targets --all-features --locked --exclude bangbang-hvf
cargo test -p bangbang-hvf --lib --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
```

On macOS Apple Silicon, also run the signed HVF integration tests:

```sh
scripts/run-hvf-tests.sh
```

Only list commands in the PR body if they were run on the reviewed head.

The PR body should include:

- summary of the behavior changed
- explicit scope exclusions when relevant
- linked issue, such as `Closes #123`
- verification commands actually run

## 6. Run The Review Loop

Review changed behavior, not only changed lines. Use `docs/review-guidelines.md`
for the project-specific checklist.

Run these focused review passes repeatedly. The review loop stops only after one
complete pass through the checklist finds no new issues to fix or record:

1. Check logic, performance, tests, security, documentation, and code
   structure.
2. Check transient failures, races, and deadlocks, including multiple
   concurrent `bangbang` processes.
3. Check resource leaks, including multiple concurrent `bangbang` processes.
4. Check active sleeps, artificial delays, performance issues, and flaky tests.
5. Check edge cases that could produce incorrect behavior.

If any review pass finds an issue that belongs to the current PR, fix it
directly, commit the fix, rerun relevant validation, and restart the review loop
from the first pass. If the issue is real but outside the PR scope, link an
existing suitable issue, or create one if no suitable issue exists. Record the
relationship on the parent issue when one exists, and then restart the loop. Do
not stop the loop just because one category was clean; stop only after a full
pass finds no new issues.

## 7. Post The PR Review

After the review loop has no new findings, run
`$github-workflow:pr-review`. The PR review comment should summarize the
reviewed scope, findings, verification, and verdict.

If `pr-review` finds new issues, fix them and return to the review loop before
merging.

## 8. Merge

Merge only when the review loop is clean, `$github-workflow:pr-review` has no
blocking findings, CI is green, and GitHub reports a clean merge state:

```sh
gh pr checks <pr-number>
gh pr view <pr-number> --json mergeable,mergeStateStatus
```

Use squash merge unless the repository has a more specific instruction:

```sh
gh pr merge <pr-number> --squash --delete-branch
git switch main
git pull --ff-only
```

After merge, continue from the latest `main` before starting the next issue.
