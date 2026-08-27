# bangbang

bangbang is a Rust virtual-machine monitor for macOS. Its local control plane
follows the Firecracker HTTP API shape over a Unix domain socket, while virtual
machines run on Apple's Hypervisor.framework rather than KVM.

The project targets macOS on Apple Silicon. Compatibility is defined by
observable API and process behavior for documented subsets; it does not imply
Firecracker binary, snapshot-file, Linux jailer-mechanism, seccomp, or KVM
compatibility. The observable macOS jailer aggregate is separately certified
with explicit platform limits and nonclaims. The
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
- [macOS Guest Workflow](docs/macos-guest-workflow.md) owns the public,
  rootless API and no-API guest boot commands, exact artifact identities,
  cleanup policy, troubleshooting, and workflow nonclaims.
- [Targeted Formal Verification](docs/formal-verification.md) owns the pinned
  Linux Kani setup, exact checked runner, five bounded proof records,
  assumptions, evidence interpretation, and nonclaims.
- [Specification Benchmark Observations](docs/specification-benchmarks.md) owns
  the strict signed Apple/HVF collector, report/comparison contract, real
  metrics-FIFO loss observation, optional network fixture, and interpretation
  nonclaims.
- [Firecracker v1.16.0 Capability Inventory](compat/firecracker/v1.16.0/README.md)
  owns the pinned structural scope, reviewed dispositions, and evidence rules.
- [Aggregate Jailer Contract](compat/firecracker/v1.16.0/jailer-aggregate-contract.md)
  owns the complete pinned grammar/operation mapping, macOS outcomes, terminal
  limits, evidence profiles, and exact inventory transition.
- [Multiprocess Isolation Contract](compat/firecracker/v1.16.0/multiprocess-isolation-contract.md)
  owns the 13-clause process/tenant mapping, failure-atomic and concurrent
  evidence, terminal identity composition, residuals, and nonclaims.
- [Host-Resource Authority Contract](compat/firecracker/v1.16.0/host-resource-authority-contract.md)
  owns the four-source obligation map, exact 17-role/five-access grant surface,
  fixed broker facets, operator/external boundaries, residuals, and nonclaims.
- [Jailer, Seccomp, and macOS Containment Contract](compat/firecracker/v1.16.0/jailer-seccomp-containment-contract.md)
  owns the final five-source, 46-clause containment composition, portable
  seccompiler boundary, exact Linux platform limits, external dependencies,
  residuals, and nonclaims.
- [Production Host Contract](compat/firecracker/v1.16.0/production-host-contract.md)
  owns the complete 31-clause production-host source accounting, terminal
  macOS/platform outcomes, operator boundaries, and exact #1378 handoff.
- [Entitlement-free vmnet Feasibility Contract](compat/firecracker/v1.16.0/vmnet-feasibility-contract.md)
  owns the no-Apple-authorization root-direct evidence boundary, exact dropped
  owner and repeated guest-connectivity gates, and the `383/0/2/33` handoff.
- [Production vmnet certification runner](docs/testing.md#production-vmnet-certification-foundation)
  owns the private config, retained fixture, guest DHCP/TCP oracle, two-package
  inspection, descriptor-grant assembly, fixed 21-case production matrix, and
  redacted result. It remains the optional Apple-authorized production matrix;
  the entitlement-free feasibility workflow does not substitute for it.
- [Wave 7 Aggregate Authority](compat/firecracker/v1.16.0/wave7-aggregate-audit.json)
  machine-checks the terminal design, device API, release, public-tool, and
  virtio-MMIO closure while retaining every external handoff.
- [Wave 8 Platform-Feasible Authority](compat/firecracker/v1.16.0/wave8-certification-contract.md)
  certifies the final seven-domain interaction matrix, all 21 unordered pairs,
  the historical 30-record platform-exclusion review, the exact uid/gid and
  configurable-chroot platform-limit successors, and the retained external
  evidence boundary.
- [Firecracker-shaped Developer Tracing](compat/firecracker/v1.16.0/tracing-contract.md)
  owns the opt-in feature, exact production scope set, record/privacy envelope,
  delivery policy, and explicit nonclaims.
- [Snapshot Feasibility](docs/snapshot-feasibility.md) owns bangbang-native
  snapshot formats, version behavior, capture/restore semantics, and nonclaims.
- [Wave 6 Snapshot Certification](compat/firecracker/v1.16.0/snapshot-wave6-contract.md)
  owns the exact 70-record load, artifact, device, tool, time/identity, and
  bounded-portability evidence ledger. Later #1491/Wave 8 composition is
  terminal; caller-owned credentialed vmnet evidence remains external under
  #1378.
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

The Wave 8 contract explains the scoped gate, its historical 377/8/3/30 phase,
the exact 377/6/3/32 uid/gid successor, the 377/5/3/33 configurable-chroot
successor, the 379/3/3/33 aggregate-jailer successor, the 380/3/2/33
multiprocess-isolation successor, the 381/3/1/33 host-resource-authority
successor, the exact 382/3/0/33 containment successor, the 383/2/0/33
production-host successor, and the current exact 383/0/2/33 entitlement-free
vmnet-feasibility successor. See the
[Testing Guide](docs/testing.md#entitlement-free-vmnet-feasibility) for the
real-host command and the checked authority for the two #1378 feasible
handoffs.

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
                  Signed dump/verify and portable Firecracker-shaped strip helper
tools/seccompiler Firecracker-compatible offline seccompiler artifact tool
tools/snapshot-tools
                  Firecracker-shaped native memory rebase, inspection, and reviewed editing
compat/firecracker/v1.16.0
                  Pinned v1.16.0 manifest, overlay, and closure ledgers
```

## Quick Start

On an Apple Silicon Mac, use the latest stable Rust toolchain and run either
checked guest workflow from the repository root:

```sh
scripts/run-macos-guest-workflow.py api
scripts/run-macos-guest-workflow.py no-api
```

Both modes prepare and verify the exact pinned arm64 kernel, deterministic
initrd and read-only Ubuntu squashfs, build and sign Bangbang for HVF, validate
the guest-visible rootfs identity, observe guest-requested poweroff, and clean
their private session. The first configures the VM through HTTP over an
owner-only Unix socket; the second starts the equivalent canonical config with
`--no-api` and proves that no socket appears. See the
[macOS Guest Workflow](docs/macos-guest-workflow.md) for prerequisites,
artifact identities, cache behavior and troubleshooting.

For control-plane exploration without booting a guest, start the direct,
uncontained API process:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

In another terminal, query its local version endpoint:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/version
```

Run `cargo run -p bangbang -- --help` for the accepted process arguments. The
canonical option semantics, API state model, endpoint behavior, and exit status
are in the [compatibility document](docs/firecracker-compatibility.md#process-startup-cli).
The CPU-template helper must be signed for Hypervisor.framework before it can
inspect an effective profile:

```sh
cargo build -p bangbang-cpu-template-helper --bin cpu-template-helper --locked
scripts/sign-hvf-binary.sh target/debug/cpu-template-helper /tmp/cpu-template-helper
/tmp/cpu-template-helper template dump --config /path/to/config.json --output /path/to/cpu_config.json
/tmp/cpu-template-helper template verify --config /path/to/config.json --template /path/to/template.json
/tmp/cpu-template-helper fingerprint dump --config /path/to/config.json --output /path/to/fingerprint.json
target/debug/cpu-template-helper template strip --paths /path/to/first.json /path/to/second.json
target/debug/cpu-template-helper fingerprint compare --prev /path/to/previous.json --curr /path/to/current.json
```

Dump output defaults to a new `cpu_config.json` and never replaces an existing
path. Fingerprint dump defaults to a new `fingerprint.json`, records a closed
platform-tagged macOS host/kernel envelope plus the same effective guest CPU
state, and is diagnostic change-awareness evidence rather than host,
migration, or snapshot-portability authority. Verify requires a selected
nonempty custom template. Strip is portable
and needs no HVF signature; it defaults to sibling `_stripped` outputs, while
`--suffix ''` atomically replaces exact single-link inputs. Its multiple paths
are not one global or crash-atomic transaction. Fingerprint compare is also
portable and unsigned: it strictly reads two platform-matched artifacts,
defaults to every applicable fact, succeeds silently when selected state is
equal, and writes one bounded canonical JSON difference to stderr with exit 1.
The exact command, format,
redaction, and platform boundaries are in
[CPU-template dump and verify](docs/firecracker-compatibility.md#cpu-template-dump-and-verify-helper),
[CPU-template strip](docs/firecracker-compatibility.md#cpu-template-strip),
[CPU-template fingerprint dump](docs/firecracker-compatibility.md#cpu-template-fingerprint-dump),
and [CPU-template fingerprint compare](docs/firecracker-compatibility.md#cpu-template-fingerprint-compare).
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

The terminal tracing, targeted formal verification, CPU-template dump/verify,
portable CPU-template strip,
platform-tagged CPU-fingerprint dump, deterministic fingerprint compare,
aggregate CPU-template workflow,
69-field API/process,
231-field device, ten-scenario aggregate metrics, multiprocess isolation,
host-resource authority, and jailer/seccomp containment
scopes have separate
fail-closed certification gates:

```sh
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --tracing-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --formal-verification-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --cpu-template-helper-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --cpu-template-strip-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --cpu-template-fingerprint-dump-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --cpu-template-fingerprint-compare-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --cpu-template-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-process-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-device-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --metrics-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --multiprocess-isolation-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --host-resource-authority-final
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --jailer-seccomp-containment-final
```

On Linux with the exact pinned Kani setup, compile, inventory, and execute all
five manifest-owned proofs with `python3 scripts/run-kani.py`. The command and
bounded interpretation are owned by the
[formal-verification guide](docs/formal-verification.md); normal macOS builds
do not install or invoke Kani.

The CPU-template aggregate gate promotes exactly its two corpus rows and CPU
semantic after validating the five-operation producer ledger, implemented and
platform-impossible foundations, signed artifact composition, runtime
selection/application/boot behavior, native-v1 no-template boundary, and the
bounded heterogeneous-fleet workflow. It does not infer distinct-host safety,
artifact authenticity, migration safety, or snapshot portability.

The metrics aggregate gate promotes exactly the metrics corpus and lifecycle
semantic. Its checked ledger covers initial, real 60-second, explicit, terminal,
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
