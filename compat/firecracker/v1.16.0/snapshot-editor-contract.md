# Firecracker v1.16.0 snapshot-editor state contract

This is the checked closure ledger for the twelve Firecracker v1.16.0
snapshot-editor identities selected by bangbang's native-state inspection and
reviewed register-edit delivery. All twelve records are
`implemented-and-verified`. The immutable upstream baseline is Firecracker
commit `d83d72b710361a10294480131377b1b00b163af8`.

The ledger covers ten direct state-operation leaves plus the two mixed
snapshot-editor aggregates. Memory rebase remains owned by the checked
[Diff and rebase contract](snapshot-diff-rebase-contract.md); the aggregate
rows become terminal here only by composing that prior terminal evidence with
the state evidence below.

## Evidence keys

- **FC-CLI** — pinned `src/snapshot-editor/src/main.rs` and
  `docs/snapshotting/snapshot-editor.md`: the nested `info-vmstate` and
  aarch64 `edit-vmstate remove-regs` command and argument shapes.
- **FC-INFO** — pinned `src/snapshot-editor/src/info.rs`:
  `version`, `vcpu-states`, and `vm-state`, each requiring
  `--vmstate-path/-v`. Firecracker emits its version or Rust `Debug` views of
  Firecracker state.
- **FC-EDIT** — pinned `src/snapshot-editor/src/edit_vmstate.rs`:
  one-or-more decimal or hexadecimal `u64` register IDs, required
  `--vmstate-path/-v` and `--output-path/-o`, per-vCPU filtering, and a
  removed/not-present report.
- **LOCAL-CLI** — `tools/snapshot-tools/src/bin/snapshot-editor.rs` and
  `tools/snapshot-tools/src/lib.rs`: the exact nested names and aliases,
  pre-path semantic admission, shared signal ownership, bounded output, and
  deterministic 0/1/2/3/130/143 outcome classes.
- **LOCAL-INFO** —
  `crates/hvf/src/snapshot_document/inspection.rs` and its explicit modules:
  schema `bangbang.snapshot-editor.info.v1`, exact version/profile fields,
  value-bearing portable semantics, literal authority redaction, and
  domain-separated SHA-256 equality fingerprints for confidential state.
- **LOCAL-EDIT** —
  `crates/hvf/src/snapshot_document/register_removal.rs`: an exact reviewed
  Firecracker v1.16.0 aarch64 registry of 67 `u64` IDs. Removal resets only an
  admitted explicit optional value to destination-default state and rebuilds
  the exact native document profile.
- **LOCAL-TRANSACTION** — `crates/runtime/src/snapshot_state_edit.rs` and
  `crates/runtime/src/snapshot_state_edit/unix.rs`: retained input and parent
  descriptors, bounded exact reads, source/entry/content revalidation,
  owner-only staging, exclusive hard-link publication, committed uncertainty,
  cleanup, and directory durability.
- **INFO-TESTS** —
  `crates/hvf/src/snapshot_document/inspection/tests.rs`: every exact native
  profile, deterministic canonical views, field ordering, explicit versus
  fingerprinted values, and literal redaction.
- **EDIT-TESTS** —
  `crates/hvf/src/snapshot_document/register_removal/tests.rs` and
  `crates/runtime/src/snapshot_state_edit/tests.rs`: exact 67-ID admission,
  requested order, per-vCPU results, profile preservation, unsupported IDs,
  every transaction stage, cancellation, races, no-clobber, cleanup, and
  uncertainty.
- **TOOLS-PROCESS** — `tools/snapshot-tools/tests/cli.rs`: actual-binary help,
  version, all native-v1/native-v2 profiles, deterministic JSON, Firecracker
  bitcode rejection, decimal/hex/space-delimited requests, immutable inputs,
  canonical owner-only outputs, aliases, path substitution, faults, signals,
  stream closure, no stdin wait, redaction, and unchanged rebase behavior.
- **SIGNED-PRODUCT** —
  `normal_bundle_adopts_native_v2_snapshot_grants_for_create_describe_and_restore`
  and
  `normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox` in
  `crates/launcher/tests/production_bundle_e2e.rs`. The runner independently
  builds, ad-hoc signs, strictly verifies, and supplies the actual editor.
  Real Full 2.12 and Diff 2.13 MMIO and PCI products are inspected, edit
  DBGBVR0 value ID `0x6030000000138004` exactly once, retain every other
  canonical inspection field, and load the edited state with unchanged
  memory/layer and drives through the fixed launcher plus nested App Sandbox
  worker. Each destination publishes Paused, resumes only on request, and
  reaches guest SYSTEM_OFF.
- **REBASE-CLOSURE** — the prior Diff/rebase contract, core tests, process
  tests, and signed real-chain tests for `edit-memory rebase`.

## Exact twelve-record ledger

| Identity | Disposition | Upstream | Implementation | Portable/core validation | Signed validation | Downstream |
| --- | --- | --- | --- | --- | --- | --- |
| `tool-argument:snapshot-editor/edit-vmstate/remove-regs/output-path` | `implemented-and-verified` | `FC-CLI + FC-EDIT` | `LOCAL-CLI + LOCAL-EDIT + LOCAL-TRANSACTION` | `EDIT-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `tool-argument:snapshot-editor/edit-vmstate/remove-regs/regs` | `implemented-and-verified` | `FC-CLI + FC-EDIT` | `LOCAL-CLI + LOCAL-EDIT` | `EDIT-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `tool-argument:snapshot-editor/edit-vmstate/remove-regs/vmstate-path` | `implemented-and-verified` | `FC-CLI + FC-EDIT` | `LOCAL-CLI + LOCAL-EDIT + LOCAL-TRANSACTION` | `EDIT-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `tool-argument:snapshot-editor/info-vmstate/vcpu-states/vmstate-path` | `implemented-and-verified` | `FC-CLI + FC-INFO` | `LOCAL-CLI + LOCAL-INFO + LOCAL-TRANSACTION` | `INFO-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `tool-argument:snapshot-editor/info-vmstate/version/vmstate-path` | `implemented-and-verified` | `FC-CLI + FC-INFO` | `LOCAL-CLI + LOCAL-INFO + LOCAL-TRANSACTION` | `INFO-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `tool-argument:snapshot-editor/info-vmstate/vm-state/vmstate-path` | `implemented-and-verified` | `FC-CLI + FC-INFO` | `LOCAL-CLI + LOCAL-INFO + LOCAL-TRANSACTION` | `INFO-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `tool-operation:snapshot-editor/edit-vmstate/remove-regs` | `implemented-and-verified` | `FC-CLI + FC-EDIT` | `LOCAL-CLI + LOCAL-EDIT + LOCAL-TRANSACTION` | `EDIT-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `tool-operation:snapshot-editor/info-vmstate/vcpu-states` | `implemented-and-verified` | `FC-CLI + FC-INFO` | `LOCAL-CLI + LOCAL-INFO` | `INFO-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `tool-operation:snapshot-editor/info-vmstate/version` | `implemented-and-verified` | `FC-CLI + FC-INFO` | `LOCAL-CLI + LOCAL-INFO` | `INFO-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `tool-operation:snapshot-editor/info-vmstate/vm-state` | `implemented-and-verified` | `FC-CLI + FC-INFO` | `LOCAL-CLI + LOCAL-INFO` | `INFO-TESTS + TOOLS-PROCESS` | `SIGNED-PRODUCT` | `terminal` |
| `semantic.snapshot:editor-rebase-and-inspection` | `implemented-and-verified` | `FC-CLI + FC-INFO + FC-EDIT` | `LOCAL-CLI + LOCAL-INFO + LOCAL-EDIT + LOCAL-TRANSACTION + REBASE-CLOSURE` | `INFO-TESTS + EDIT-TESTS + TOOLS-PROCESS + REBASE-CLOSURE` | `SIGNED-PRODUCT + REBASE-CLOSURE` | `terminal` |
| `corpus:snapshot-editor` | `implemented-and-verified` | `FC-CLI + FC-INFO + FC-EDIT` | `LOCAL-CLI + LOCAL-INFO + LOCAL-EDIT + LOCAL-TRANSACTION + REBASE-CLOSURE` | `INFO-TESTS + EDIT-TESTS + TOOLS-PROCESS + REBASE-CLOSURE` | `SIGNED-PRODUCT + REBASE-CLOSURE` | `terminal` |

## Public inspection contract

- `snapshot-editor info-vmstate version --vmstate-path/-v <path>` prints the
  exact Bangbang native token `v<major>.<minor>.<patch>` and one newline.
  `vcpu-states` and `vm-state` use the same required path option and emit
  bounded deterministic pretty JSON plus one newline.
- Both JSON views use schema `bangbang.snapshot-editor.info.v1` and expose
  `family`, `profile`, exact structured `version`, and the complete ordered
  vCPU vector. `vm-state` additionally exposes memory, machine, global,
  topology, time, devices, and an optional Diff layer.
- Portable machine/device values remain explicit. Confidential high-entropy
  state uses domain-separated SHA-256 equality fingerprints. Host paths,
  descriptors, selectors, inode-derived identities, boot arguments, grants,
  and other low-entropy authority are the literal `<redacted>` and never an
  equality oracle.
- The reader retains the state file and parent directory, rejects final
  symlinks and special files, bounds the exact read, and rechecks source,
  pathname entry, parent, content, and cancellation before publishing output.

## Public reviewed register-edit contract

- `snapshot-editor edit-vmstate remove-regs <REGS>...
  --vmstate-path/-v <path> --output-path/-o <path>` admits decimal and
  `0x`/`0X` hexadecimal `u64` IDs, including space-delimited values supplied
  to one argument. The request is nonempty, duplicate-free, and entirely
  validated before either path is accessed.
- Bangbang intentionally does not accept Firecracker's arbitrary raw KVM
  vector. The finite 67-ID registry contains only reviewed aarch64 scalar
  state that maps to typed optional destination state. IDs and values are
  discarded before path access and never appear in output or diagnostics.
- Each admitted ID is removed from every vCPU when present. Success reports
  only each vCPU's removed/not-present counts and aggregate requested,
  removed, and not-present counts. The report is deterministic and
  value-free.
- The source is immutable. The output must be an absent distinct pathname in
  an authorized parent directory. The transaction writes a canonical owner-only
  `0600` staged file, verifies its decoded typed state, publishes without
  clobber, synchronizes the directory, and removes private staging. Durable
  success is exit 0; ordinary failure is 1; syntax is 2; committed uncertainty
  is 3; precommit SIGINT/SIGTERM cancellation is 130/143.

## Signed Full/Diff product evidence

The no-skip signed runner's targeted `production_bundle` selection is
self-contained: it builds the exact `aarch64-apple-darwin` snapshot-tools
artifacts, copies them into its private directory, separately ad-hoc signs
both tools, verifies both with `codesign --verify --strict`, and supplies the
editor path to the test.

For both MMIO and PCI, the ordinary production product creates a one-vCPU
native-v2 2.12 Full artifact and a nonempty sparse zero-root native-v2 2.13
Diff artifact. Each actual signed-editor case proves:

- two byte-identical executions of `version`, `vcpu-states`, and `vm-state`;
- exact schema, profile, version, transport, three-block graph, memory
  identity, and complete Diff base/layer/result relationship;
- exactly one DBGBVR0 value removal and a changed reviewed-debug fingerprint,
  with the whole vCPU and VM JSON equal after normalizing only
  `vcpus[*].debug.reviewed`;
- immutable original state and Full memory/Diff layer bytes and inode facts,
  a distinct canonical `0600` edited inode, and no editor/state/memory staging
  residue;
- retained descriptor adoption after every input pathname is replaced; and
- edited-state load with the unchanged memory/layer and drive set through the
  fixed production launcher and nested App Sandbox + Hypervisor worker,
  Paused-first publication, explicit resume, MMIO/PCI configuration, guest
  SYSTEM_OFF, and post-run artifact immutability.

## Compatibility and retained boundaries

The command names and arguments track pinned Firecracker v1.16.0, but the
artifact semantics are intentionally Bangbang-native. The tools reject valid
Firecracker bitcode, do not emit Firecracker Rust `Debug`, do not expose raw
KVM bytes, and do not claim Linux/KVM state compatibility. Inspection and
editing cover frozen native-v1 plus exact native-v2 2.3 through 2.13 documents;
the signed certification covers current Full 2.12 and Diff 2.13 products on
macOS Apple Silicon.

`corpus:snapshot-versioning` and
`semantic.snapshot:diff-dirty-tracking-and-memory-backends` remain
`audit-required` under
[#1543](https://github.com/seven332/bangbang/issues/1543). This contract does
not claim native-v2 Uffd, external paging/backend equivalence, Firecracker
snapshot bytes, arbitrary KVM-register editing, live-peer migration, or broad
cross-host portability, and it does not close #1490.
