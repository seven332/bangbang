# bangbang

bangbang is a Rust virtual-machine monitor for macOS. Its local control plane
follows the Firecracker HTTP API shape over a Unix domain socket, while virtual
machines run on Apple's Hypervisor.framework rather than KVM.

The project targets macOS on Apple Silicon. Compatibility is defined by
observable API and process behavior for documented subsets; it does not imply
Firecracker binary, snapshot-file, jailer, seccomp, or KVM compatibility. The
current bangbang-native ceiling is `v2.13.0`: `Full` emits an exact `v2.12.0`
state plus complete memory image, while `Diff` emits an exact `v2.13.0` state
plus differential layer. The loader retains the exact older native profiles
documented in the snapshot guide.

## Documentation

Each detailed subject has one primary document:

- [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) owns
  public API and CLI behavior, field policy, platform limits, and compatibility
  rationale.
- [Firecracker Validation Matrix](docs/firecracker-validation-matrix.md) is the
  compact current-status and evidence index.
- [Firecracker v1.16.0 Capability Inventory](compat/firecracker/v1.16.0/README.md)
  owns the pinned structural scope, reviewed dispositions, and evidence rules.
- [Firecracker-shaped Developer Tracing](compat/firecracker/v1.16.0/tracing-contract.md)
  owns the opt-in feature, exact production scope set, record/privacy envelope,
  delivery policy, and explicit nonclaims.
- [Snapshot Feasibility](docs/snapshot-feasibility.md) owns bangbang-native
  snapshot formats, version behavior, capture/restore semantics, and nonclaims.
- [Wave 6 Snapshot Certification](compat/firecracker/v1.16.0/snapshot-wave6-contract.md)
  owns the exact 70-record load, artifact, device, tool, time/identity, and
  bounded-portability evidence ledger; only its two external network
  aggregates remain nonterminal for #1378/#1491.
- [`bangbang-pager-v1` Protocol](docs/snapshot-pager-protocol.md) owns the pager
  wire and lifecycle contract.
- [macOS Host Security Model](docs/security.md) owns authority, containment,
  isolation, signing, and threat boundaries.
- [Testing Guide](docs/testing.md) owns test layers and the complete validation
  command set.
- [Pull Request Review Guidelines](docs/review-guidelines.md) owns review
  expectations.

Versioned files under `compat/firecracker/v1.16.0/` are audit ledgers. They
record pinned upstream identities, dispositions, exclusions, and evidence; the
human-readable documents above explain current behavior.

## Workspace Layout

```text
crates/api        Firecracker-shaped API request and response surface
crates/runtime    Backend-neutral VM model and device/runtime foundations
crates/hvf        Hypervisor.framework backend and signed integration tests
crates/bangbang   VMM process, API server, and startup CLI
crates/launcher   Production app bundle, nested worker, and supervision
crates/pager      bangbang-pager-v1 protocol and VMM-side client
crates/session    Private launcher/worker lifecycle and grant protocols
crates/vhost-user Portable vhost-user frontend protocol foundations
tools/firecracker-capability-audit
                  Checked Firecracker source/capability inventory validator
tools/cpu-template-helper
                  Library-only portable CPU-template helper foundation
tools/seccompiler Firecracker-compatible offline seccompiler artifact tool
tools/snapshot-tools
                  Firecracker-shaped native memory rebase, inspection, and reviewed editing
compat/firecracker/v1.16.0
                  Pinned v1.16.0 manifest, overlay, and closure ledgers
```

## Quick Start

Use the latest stable Rust toolchain. Start the direct, uncontained VMM process
and API server:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

In another terminal, verify the local API:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/version
```

Configure and start a minimal guest after replacing `/tmp/vmlinux` with a
supported arm64 Linux kernel:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/machine-config \
  -H 'Content-Type: application/json' \
  -d '{"vcpu_count":1,"mem_size_mib":128}'

curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/boot-source \
  -H 'Content-Type: application/json' \
  -d '{"kernel_image_path":"/tmp/vmlinux","boot_args":"console=ttyS0 reboot=k panic=1"}'

curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/actions \
  -H 'Content-Type: application/json' \
  -d '{"action_type":"InstanceStart"}'
```

Run `cargo run -p bangbang -- --help` for the accepted process arguments. The
canonical option semantics, API state model, endpoint behavior, and exit status
are in the [compatibility document](docs/firecracker-compatibility.md#process-startup-cli).
The snapshot rebase, deterministic state-inspection, and reviewed register-edit
surfaces are in [Snapshot Rebase Tools](docs/firecracker-compatibility.md#snapshot-rebase-tools)
and [Snapshot State Inspection and Reviewed Editing](docs/firecracker-compatibility.md#snapshot-state-inspection-and-reviewed-editing).

## Production macOS Bundle

The direct command above runs with the invoking user's ambient authority. The
production entry point instead assembles a fixed outer launcher and a separately
signed nested App Sandbox + Hypervisor worker:

```sh
scripts/build-production-bundle.sh --output /private/operator/Bangbang.app
```

The command publishes only to an absent destination. Signing profiles,
entitlements, startup grants, vmnet policy, crash ownership, and bundle
validation are defined in the
[security model](docs/security.md#production-bundle-and-signed-worker-boundary).
Run real HVF-backed verification through the wrapper documented in the
[testing guide](docs/testing.md#running-tests).

## Development

Two fast checks cover formatting-independent inventory and workspace type
consistency:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
cargo check --workspace --all-targets --all-features --locked
```

The ordinary audit also validates the exact 231-field device producer
authority for #1789. The completed #1838–#1846 slices contribute 212
implemented device records, two source-neutral records, and 17 terminal
platform-zero records. The latter are the two immutable-MAC fields plus six
arm64-retained i8042 and nine PIO/KVM-clock fields. All 231 records are
terminal and map to ten implemented shared device profiles. The dedicated
device gate verifies that exact profile set, its resolvable evidence, the
212/2/17 field census, and the terminal #1790 lifecycle handoff.

The terminal tracing, 69-field API/process, 231-field device, and ten-scenario
aggregate metrics scopes have separate fail-closed certification gates:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --tracing-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-process-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-device-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-final
```

The aggregate gate promotes exactly the metrics corpus and lifecycle semantic.
Its checked ledger covers initial, real 60-second, explicit, terminal,
backpressure, retry, configured cardinality, snapshot-destination freshness,
hotplug/reuse, and process isolation behavior without claiming durable or
exactly-once output.

Developer tracing is compile-time opt-in (`--features tracing`) and remains
absent from default builds. VMM scopes use the configured logger Trace
level/module filter; snapshot tools additionally require `BANGBANG_TRACE=*` or
a matching module prefix. Run `scripts/report-tracing-overhead.sh` for the
descriptive release binary-size and scope-cost report.

When the pinned sibling checkout is available at `../firecracker`, verify its
source identities and anchors with:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- compare --firecracker ../firecracker
```

Before opening or updating a pull request, run the complete command set in
[Running Tests](docs/testing.md#running-tests). Real Hypervisor.framework tests
must use `scripts/run-integration-tests.sh` so their binaries are signed.
