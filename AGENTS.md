# Repository Guidelines

## Project Structure, Crates, and Modules

This is a Rust workspace for `bangbang`, a macOS-oriented VMM scaffold intended to track Firecracker’s process model and API shape.

- `crates/bangbang` -> package `bangbang`: executable VMM process entrypoint.
- `crates/api` -> package `bangbang-api`: Firecracker-compatible API endpoint names.
- `crates/runtime` -> package `bangbang-runtime`: backend-neutral VM trait and error type.
- `crates/hvf` -> package `bangbang-hvf`: Apple Hypervisor.framework backend skeleton.
- `README.md`: current project scope and build instructions.

Unit tests live next to the code they exercise under each crate’s `src/` tree. There are no assets or generated source directories currently checked in.

## Build, Test, and Development Commands

- `cargo fmt --all -- --check`: verify Rust formatting.
- `cargo check --workspace --all-targets --all-features --locked`: type-check the full workspace using the committed lockfile.
- `cargo test --workspace --all-targets --all-features --locked --exclude bangbang-hvf`: run non-HVF tests with all targets and features enabled.
- `cargo test -p bangbang-hvf --lib --all-features --locked`: run unsigned HVF unit tests.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: run lint checks with warnings treated as errors.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked`: build documentation without dependency docs.
- `scripts/run-hvf-tests.sh`: sign and run HVF integration tests on macOS Apple Silicon; use `--allow-unsupported` only for CI runners that cannot execute HVF.
- `cargo run -p bangbang`: run the current VMM process skeleton.

Use these commands before opening or updating a pull request. For local or self-hosted HVF verification, run `scripts/run-hvf-tests.sh` without `--allow-unsupported` so unsupported hosts fail instead of being ignored.

## Coding Style & Naming Conventions

Use Rust 2024 edition style and `rustfmt` defaults. Keep modules small and aligned with crate boundaries. Public names should describe stable concepts, not future plans; for example, prefer explicit names like `is_supported_target()` when checking compile-target support only.

Library package names use the `bangbang-*` pattern even when their directory names are shorter, such as `crates/hvf` for `bangbang-hvf`. The executable package is `bangbang` in `crates/bangbang`.

Workspace lints deny Rust warnings, strict rustdoc issues, and selected Clippy lints. Keep test-only Clippy exceptions in `clippy.toml` instead of adding broad inline `allow` attributes.

Unsafe code must stay isolated behind small FFI wrappers, with `SAFETY:` comments explaining each unsafe call.

## Testing Guidelines

Use Rust’s built-in test framework with `#[test]`. Add focused unit tests for argument parsing, error formatting, and backend state transitions as those surfaces grow. Test names should describe behavior, such as `parse_help_arg` or `displays_hypervisor_error`.

Real Hypervisor.framework integration tests must stay in `crates/hvf/tests/` and run through `scripts/run-hvf-tests.sh` so the test binary is signed and unsupported hosts are handled explicitly. Do not run or add real HVF integration tests through the unsigned workspace test path.

## Commit & Pull Request Guidelines

The repository uses Conventional Commits, as seen in `feat: add initial Rust scaffold (#1)`. Use messages such as:

- `feat: add vm configuration model`
- `fix: reject duplicate drive ids`
- `chore: update workspace metadata`

Pull requests should include a concise summary, note scope exclusions when relevant, and list verification commands run. Link related issues when available. UI screenshots are not relevant for this repository unless a future tool adds a visual interface.
