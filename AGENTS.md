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
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: run lint checks with warnings treated as errors.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked`: build documentation without dependency docs.
- `cargo run -p bangbang`: run the current VMM process skeleton.

Use these commands before opening or updating a pull request. On macOS Apple Silicon, also sign and run the `bangbang-hvf` test binary as described in `README.md`; HVF lifecycle tests should fail rather than be ignored when the host cannot run them.

## Coding Style & Naming Conventions

Use Rust 2021 edition style and `rustfmt` defaults. Keep modules small and aligned with crate boundaries. Public names should describe stable concepts, not future plans; for example, prefer explicit names like `is_supported_target()` when checking compile-target support only.

Library package names use the `bangbang-*` pattern even when their directory names are shorter, such as `crates/hvf` for `bangbang-hvf`. The executable package is `bangbang` in `crates/bangbang`.

Unsafe code must stay isolated behind small FFI wrappers, with `SAFETY:` comments explaining each unsafe call.

## Testing Guidelines

Use Rust’s built-in test framework with `#[test]`. Add focused unit tests for argument parsing, error formatting, and backend state transitions as those surfaces grow. Test names should describe behavior, such as `parse_help_arg` or `displays_hypervisor_error`.

Do not add integration tests that require creating real Hypervisor.framework VMs until the runtime can gate them clearly by platform and privileges.

## Commit & Pull Request Guidelines

The repository uses Conventional Commits, as seen in `feat: add initial Rust scaffold (#1)`. Use messages such as:

- `feat: add vm configuration model`
- `fix: reject duplicate drive ids`
- `chore: update workspace metadata`

Pull requests should include a concise summary, note scope exclusions when relevant, and list verification commands run. Link related issues when available. UI screenshots are not relevant for this repository unless a future tool adds a visual interface.
