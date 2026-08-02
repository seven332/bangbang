# Firecracker v1.16.0 Diff and rebase closure contract

This is the checked closure ledger for the 15 Firecracker v1.16.0 identities
selected by bangbang's native-v2 differential snapshot and public rebase-tool
delivery. All fifteen identities are `implemented-and-verified`; the two mixed
aggregates compose this focused evidence with the checked
[Wave 6 snapshot contract](snapshot-wave6-contract.md). The immutable upstream
baseline is Firecracker commit
`d83d72b710361a10294480131377b1b00b163af8`.

The two snapshot-editor state aggregates formerly retained here are now
terminal in the checked
[snapshot-editor state contract](snapshot-editor-contract.md). This ledger
continues to own only the Diff/create/version/rebase identities below.

## Evidence keys

- **FC-API** — pinned `src/firecracker/swagger/firecracker.yaml` and
  `src/firecracker/src/api_server/request/snapshot.rs`: strict
  `PUT /snapshot/create`, required `snapshot_type`, `snapshot_path`, and
  `mem_file_path`, and the `Full`/`Diff` choice.
- **FC-DIFF** — pinned `docs/snapshotting/snapshot-support.md` and
  `src/vmm/src/vstate/vm.rs`: complete state per call, tracked dirty pages or
  the broader untracked `mincore` selection, ordered memory layers, dirty
  reset after success, no state merge, and final-state-only restore.
- **FC-REBASE** — pinned `src/rebase-snap/src/main.rs` and
  `src/snapshot-editor/src/edit_memory.rs`: deprecated
  `rebase-snap --base-file <path> --diff-file <path>` and replacement
  `snapshot-editor edit-memory rebase --memory-path/-m <path> --diff-path/-d <path>`.
- **FC-VERSION** — pinned `src/firecracker/src/main.rs`: the early
  `--snapshot-version` command shape. Bangbang reports its own native version;
  it does not claim Firecracker snapshot bytes or the upstream v10 format.
- **LOCAL-API** — strict request parsing and public action projection in
  `crates/api/src/http.rs` and `crates/bangbang/src/api_server.rs`.
- **LOCAL-DIFF** — paused-only Full/Diff policy, linear dirty epochs,
  exact-2.13 state/layer/result closure, two-output no-clobber publication,
  zero-root or matching-result load, MMIO/PCI reconstruction, and public
  dispatch in `crates/runtime/src/{snapshot,snapshot_diff_v2_13,
  snapshot_artifact,snapshot_lineage}.rs`, `crates/hvf/src/snapshot_v2.rs`,
  and `crates/bangbang/src/vmm.rs`.
- **LOCAL-REBASE** — macOS staged complete-result materialization and atomic
  base replacement in `crates/runtime/src/snapshot_rebase.rs`. The Diff input
  is immutable; component inodes, lineage, GPA topology, file facts, staging,
  cancellation, exchange, verification, cleanup, and directory durability are
  checked without reopening substituted pathnames.
- **LOCAL-TOOLS** — the two Clap frontends in
  `tools/snapshot-tools/src/bin/{rebase-snap,snapshot-editor}.rs` converge on
  one executor in `tools/snapshot-tools/src/lib.rs`, one signal owner, and one
  public outcome classifier.
- **LOCAL-VERSION** — the early `--snapshot-version` command in
  `crates/bangbang/src/main.rs` reports the native-v2 compatibility ceiling
  before socket publication without relabeling Firecracker artifacts.
- **DIFF-PORTABLE** —
  `current_dynamic_add_and_remove_topologies_write_canonically`,
  `repeated_complete_application_handles_add_then_remove`,
  `exact_minor_thirteen_diff_closes_all_sixty_four_mmio_and_pci_products`,
  `diff_publication_commits_and_zero_root_loads_as_one_closed_pair`,
  `diff_load_accepts_a_complete_rebased_result_image`,
  `tracked_zero_diff_abort_restores_exact_generation`,
  `image_diff_durable_reset_commits_exact_result`, and
  `untracked_image_diff_commits_without_inventing_epoch`.
- **REBASE-CORE** — `complete_and_zero_root_bases_exchange_durably`,
  `sparse_cross_directory_and_repeated_rebases_are_exact`,
  `every_outer_precommit_stage_failure_preserves_inputs`,
  `observed_component_and_parent_replacements_abort_safely`,
  `postcommit_preserves_first_uncertainty_and_records_later_sync`, and the
  remaining injected cancellation, alias, special-file, stale-lineage,
  corruption, cleanup, and durability cases in
  `crates/runtime/src/snapshot_rebase/tests.rs`.
- **TOOLS-PROCESS** —
  `help_and_version_expose_only_the_two_selected_firecracker_surfaces`,
  `invalid_invocations_are_deterministic_and_do_not_echo_values`,
  `both_commands_materialize_byte_identical_complete_images`,
  `malformed_stale_and_alias_inputs_fail_without_mutation_or_leaks`,
  `sequential_commands_apply_repeated_lineage_exactly`, and
  `process_signals_and_path_substitutions_preserve_precommit_inputs`.
  `durable_and_committed_uncertain_results_are_distinct_and_redacted` pins the
  shared executor's outcome mapping.
- **SIGNED-DIFF** —
  `signed_native_v2_diff_process_loads_zero_root_and_rebased_products` creates
  tracked and untracked real-HVF layers, invokes both separately signed tools
  on one real chain, compares complete results, restores contained PCI, and
  recaptures the exact loaded predecessor.
- **PRODUCTION-DIFF** —
  `normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox`
  crosses the ordinary fixed launcher and nested App Sandbox + Hypervisor
  worker over MMIO and PCI with exact path-scoped outputs, post-adoption
  replacement of every descriptor-backed source/input pathname, sparse
  zero-root Diff, `v2.13.0` description, Paused-first
  load, real guest `SYSTEM_OFF`, immutable inputs, and complete cleanup.
- **VERSION-PROCESS** —
  `executable_reports_native_snapshot_versions_before_socket_publication` and
  `sandboxed_bundle_reports_current_native_v2_snapshot_version` pin the direct
  and production App Sandbox command result.
- **WAVE6** — `snapshot-wave6-contract.md` composes exact native-v1 external
  paging, the complete load/backend schema, exact native-v2 2.3–2.13 profile
  evolution, all device products, production time/identity recapture, and the
  bounded same-host portability policy without claiming Firecracker bytes,
  Linux UFFD wire identity, or an untested cross-host success pair.

## Exact 15-record ledger

| Identity | Disposition | Upstream | Implementation | Portable/core validation | Process/signed validation | Downstream |
| --- | --- | --- | --- | --- | --- | --- |
| `api-operation:PUT /snapshot/create` | `implemented-and-verified` | `FC-API + FC-DIFF` | `LOCAL-API + LOCAL-DIFF` | `DIFF-PORTABLE` | `SIGNED-DIFF + PRODUCTION-DIFF` | `terminal` |
| `api-path:/snapshot/create` | `implemented-and-verified` | `FC-API` | `LOCAL-API + LOCAL-DIFF` | `DIFF-PORTABLE` | `SIGNED-DIFF + PRODUCTION-DIFF` | `terminal` |
| `api-property:SnapshotCreateParams.mem_file_path` | `implemented-and-verified` | `FC-API + FC-DIFF` | `LOCAL-API + LOCAL-DIFF` | `DIFF-PORTABLE` | `SIGNED-DIFF + PRODUCTION-DIFF` | `terminal` |
| `api-property:SnapshotCreateParams.snapshot_path` | `implemented-and-verified` | `FC-API + FC-DIFF` | `LOCAL-API + LOCAL-DIFF` | `DIFF-PORTABLE` | `SIGNED-DIFF + PRODUCTION-DIFF` | `terminal` |
| `api-property:SnapshotCreateParams.snapshot_type` | `implemented-and-verified` | `FC-API + FC-DIFF` | `LOCAL-API + LOCAL-DIFF` | `DIFF-PORTABLE` | `SIGNED-DIFF + PRODUCTION-DIFF` | `terminal` |
| `api-schema:SnapshotCreateParams` | `implemented-and-verified` | `FC-API + FC-DIFF` | `LOCAL-API + LOCAL-DIFF` | `DIFF-PORTABLE` | `SIGNED-DIFF + PRODUCTION-DIFF` | `terminal` |
| `firecracker-argument:snapshot-version` | `implemented-and-verified` | `FC-VERSION` | `LOCAL-VERSION` | `DIFF-PORTABLE` | `VERSION-PROCESS + SIGNED-DIFF + PRODUCTION-DIFF` | `terminal` |
| `tool-argument:rebase-snap/base-file` | `implemented-and-verified` | `FC-REBASE` | `LOCAL-TOOLS + LOCAL-REBASE` | `REBASE-CORE` | `TOOLS-PROCESS + SIGNED-DIFF` | `terminal` |
| `tool-argument:rebase-snap/diff-file` | `implemented-and-verified` | `FC-REBASE` | `LOCAL-TOOLS + LOCAL-REBASE` | `REBASE-CORE` | `TOOLS-PROCESS + SIGNED-DIFF` | `terminal` |
| `tool-argument:snapshot-editor/edit-memory/rebase/diff-path` | `implemented-and-verified` | `FC-REBASE` | `LOCAL-TOOLS + LOCAL-REBASE` | `REBASE-CORE` | `TOOLS-PROCESS + SIGNED-DIFF` | `terminal` |
| `tool-argument:snapshot-editor/edit-memory/rebase/memory-path` | `implemented-and-verified` | `FC-REBASE` | `LOCAL-TOOLS + LOCAL-REBASE` | `REBASE-CORE` | `TOOLS-PROCESS + SIGNED-DIFF` | `terminal` |
| `tool-operation:rebase-snap/rebase` | `implemented-and-verified` | `FC-REBASE` | `LOCAL-TOOLS + LOCAL-REBASE` | `REBASE-CORE` | `TOOLS-PROCESS + SIGNED-DIFF` | `terminal` |
| `tool-operation:snapshot-editor/edit-memory/rebase` | `implemented-and-verified` | `FC-REBASE` | `LOCAL-TOOLS + LOCAL-REBASE` | `REBASE-CORE` | `TOOLS-PROCESS + SIGNED-DIFF` | `terminal` |
| `semantic.snapshot:diff-dirty-tracking-and-memory-backends` | `implemented-and-verified` | `FC-DIFF` | `LOCAL-DIFF + WAVE6` | `DIFF-PORTABLE + REBASE-CORE + WAVE6` | `SIGNED-DIFF + PRODUCTION-DIFF + WAVE6` | `terminal` |
| `corpus:snapshot-versioning` | `implemented-and-verified` | `FC-DIFF + FC-REBASE` | `LOCAL-DIFF + LOCAL-TOOLS + LOCAL-REBASE + WAVE6` | `DIFF-PORTABLE + REBASE-CORE + WAVE6` | `TOOLS-PROCESS + SIGNED-DIFF + PRODUCTION-DIFF + WAVE6` | `terminal` |

## Observable native-v2 Diff contract

- `PUT /snapshot/create` is strict and paused-only. `Full` publishes exact
  native-v2 2.12 state plus a complete memory image; `Diff` publishes exact
  native-v2 2.13 complete state plus one packed GPA-addressed memory layer.
  `snapshot_path` and `mem_file_path` are distinct required outputs and are
  committed as one state-last no-clobber pair.
- A tracked first Diff is rooted at zero and selects pages dirtied since boot.
  A later Diff names the exact predecessor result. Untracked capture selects a
  conservative complete current range rather than inventing a dirty epoch.
  A visible successful snapshot advances/resets lineage once; failed,
  cancelled, or uncommitted publication restores the prior epoch exactly.
- Every state remains complete. Memory layers are applied in lineage order by
  GPA over zero or a matching complete predecessor. Rebase produces one
  complete result image; it does not merge state. Restore always uses the
  complete state paired with the last applied layer/result.
- Exact-2.13 state binds the base, selected extents, complete result topology,
  and final memory identity. Direct, contained, production, and nested App
  Sandbox loads reject detached or stale layers before VM publication and
  commit the destination Paused before optional resume.

## Public rebase command and outcome contract

- `rebase-snap --base-file <path> --diff-file <path>` accepts only the two
  required long options. It prints exactly: “This tool is deprecated and will be removed in the future. Please use 'snapshot-editor' instead.”
- `snapshot-editor edit-memory rebase --memory-path <path> --diff-path <path>`
  is the replacement. `-m` and `-d` are exact aliases. Editor inspection and
  VM-state/register commands are certified separately by the
  [snapshot-editor state contract](snapshot-editor-contract.md); they are not
  part of this rebase ledger.
- Both frontends invoke the same macOS-only transaction. Unsupported targets
  reject before path access. Success is exit 0; operational failure is 1;
  Clap syntax/configuration failure is 2; committed-but-uncertain completion
  is 3; precommit SIGINT/SIGTERM cancellation is 130/143. Diagnostics redact
  host paths and snapshot values.
- Unlike Firecracker's Linux in-place `SEEK_DATA`/`SEEK_HOLE` plus
  `sendfile64` mutation, bangbang validates native-v2 lineage and GPA topology,
  materializes a private owner-only staged complete image, synchronizes it,
  atomically exchanges it with the base, verifies the committed inode, cleans
  the displaced file, and synchronizes the directory. Before exchange, every
  failure preserves both inputs; after exchange, failures report uncertainty
  and never claim rollback. The Diff remains byte- and inode-immutable.

## Certification boundaries

The two mixed ledger rows are terminal only through the exact Wave 6
composition. Frozen native-v1 owns eager File plus reviewed macOS external
paging; current native-v2 deliberately rejects Uffd at its profile gate.
Snapshot-editor inspection, VM-state/register editing, and the two editor
aggregates remain detailed in the separate twelve-record state contract. No
terminal row here claims live-peer preservation, state merging, Linux/KVM
dirty-bitmap mechanics, Linux UFFD or sparse-byte compatibility, Firecracker
artifact bytes, or a distinct-host success pair.
