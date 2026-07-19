# Firecracker v1.16.0 process contract

This document is the human-owned semantic audit for the process identities in
[`source-manifest.json`](source-manifest.json). The immutable baseline is
Firecracker v1.16.0 commit
`d83d72b710361a10294480131377b1b00b163af8`. The manifest proves the exact
identity set; this contract traces observable behavior to bangbang production
code and executable validation.

An argument leaf is terminal only when its process-facing behavior is present.
Recognizing a name or returning a stable unsupported error is not
implementation. An implemented argument that accepts a configuration or
resource delegates the contents to that capability family's records; the leaf
does not certify every possible device or configuration payload. Composite
records remain nonterminal when any behavior they aggregate is incomplete.

## Generic parser behavior

- Firecracker's `--help` and `-h` have precedence over every other token before
  the first standalone `--`; `--version` has the next precedence. Bangbang
  matches this and additionally retains its existing `-V` alias.
- The first standalone `--` ends option parsing. Firecracker's main process
  does not consume the retained extra `String` arguments, so bangbang ignores
  every following help, version, unknown, or positional token. Bangbang
  additionally splits its `OsString` input before UTF-8 conversion and thus
  ignores non-UTF-8 extras; pinned Firecracker collects `env::args()` first, so
  that robustness extension is not an upstream compatibility claim.
- Both implementations reject duplicate configured arguments. Bangbang accepts
  Firecracker's `--name value` spelling and additionally accepts
  `--name=value` for value-taking options. Value-less flags reject attached
  values.
- Argument parsing failures use exit code 153 and happen before fd-table work,
  signal setup, resource opening, readiness, or socket publication. Invalid
  logger configuration uses bad-configuration exit code 152.

Implementation is in
[`Args::parse_os` and `Args::parse`](../../../crates/bangbang/src/main.rs).
Focused validation is in the colocated parser tests and
[`executable_ignores_tokens_after_end_of_options_separator`](../../../crates/bangbang/tests/process_e2e.rs).

## Configured arguments

`I+V` means `implemented-and-verified`; `audit` means the record intentionally
remains `audit-required` for the named owner.

| Argument | Pinned Firecracker contract | Bangbang process outcome and equivalence | Owner / disposition | Production and validation evidence |
| --- | --- | --- | --- | --- |
| `--api-sock <PATH>` | One value; default `/run/firecracker.socket`; bind the API Unix socket. | Binds one owner-only Unix socket. The macOS host equivalent defaults to `/tmp/bangbang.socket`; an explicit path is exact. Existing paths are not removed or clobbered. | process / I+V | [`StartupConfig`, `run`](../../../crates/bangbang/src/main.rs); [`executable_serves_api_and_shuts_down_cleanly`, socket conflict and concurrent-owner tests](../../../crates/bangbang/tests/process_e2e.rs) |
| `--boot-timer` | Flag; enable the guest boot-timer device/log event. | Enables the aarch64 Firecracker boot-timer MMIO device and routes its event through the configured logger. | process-observability / I+V | [`StartupConfig::boot_timer`](../../../crates/bangbang/src/main.rs); [`executable_accepts_boot_timer_flag`](../../../crates/bangbang/tests/process_e2e.rs) and signed guest boot-timer coverage in [`executable_hvf_e2e.rs`](../../../crates/bangbang/tests/executable_hvf_e2e.rs) |
| `--config-file <PATH>` | One value; load the JSON configuration; required by `--no-api`. | Reads one bounded regular UTF-8 JSON file, applies the supported Firecracker-shaped sections in their defined order, and starts before API publication or no-API readiness. In contained mode, an exact `bangbang-grant:<GrantId>` claims the singleton read-only startup-config descriptor once; malformed, missing, mismatched, or consumed claims fail closed without path fallback. Drive and pmem sections may independently claim repeatable exact-ID backing grants with access derived from each validated device, retain them across configuration application, and move them into startup without reopening tags. Logger/metrics/serial sections may claim their singleton exact-ID write-only sinks after validation; logger/metrics retain adopted sinks while serial moves its prepared output once into startup. Direct mode treats every such text as a pathname. Section semantics remain owned by their capability records. | process / I+V | [`config_file_actions_with_authority`, `run`](../../../crates/bangbang/src/main.rs); config-file process and signed startup cases in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs), [`executable_hvf_e2e.rs`](../../../crates/bangbang/tests/executable_hvf_e2e.rs), and the external no-API production-bundle guests with singleton, repeatable, and output resources in [`production_bundle_e2e.rs`](../../../crates/launcher/tests/production_bundle_e2e.rs) |
| `--describe-snapshot <PATH>` | One value; early command that prints the provided Firecracker state file's data-format version. | Validates and describes the supported bangbang native-v1 envelope, prints `v1.0.0`, and explicitly rejects incompatible Firecracker state files. In contained mode an exact `bangbang-grant:<GrantId>` claims one `SnapshotDescribeInput` read-only descriptor and never reopens the tag; direct mode keeps pathname behavior. | snapshot process / I+V (#1368) | Bounded native inspection in [`describe_snapshot_with_authority`](../../../crates/bangbang/src/main.rs), direct process evidence in [`executable_reports_native_snapshot_versions_before_socket_publication`](../../../crates/bangbang/tests/process_e2e.rs), and external granted-file evidence in [`normal_bundle_adopts_snapshot_grants_for_create_describe_and_restore`](../../../crates/launcher/tests/production_bundle_e2e.rs) |
| `--enable-pci` | Flag; enable Firecracker PCIe support for every configured virtio device. | Implemented on macOS arm64 with the HVF/GIC-MSI symbols required by the product path. Exact flag syntax selects one immutable all-virtio transport: balloon, block, network, pmem, vsock, entropy, and virtio-mem use deterministic modern PCI functions, while platform devices remain MMIO. Default startup remains all-virtio-MMIO. Unsupported hosts fail before API/no-API readiness; complete slot/BAR/vector capacity fails before `Running`. PCI profiles explicitly fail native-v1 create/load. #1420 and #1421 use this immutable profile for transactional Running/Paused non-root block and pmem PUT/DELETE with manual guest coordination; pmem includes dynamic shadow mapping, targeted flush/unmap, and exact range reuse. Runtime network mutation and PCI snapshots remain deferred. | process PCI / I+V (#1419, #1420, #1421) | Parser and pre-readiness probe in [`Args::parse` and `run`](../../../crates/bangbang/src/main.rs), all-virtio assembly and retained runtime manager in [`startup.rs`](../../../crates/hvf/src/startup.rs), dynamic mappings in [`memory.rs`](../../../crates/hvf/src/memory.rs), exact/attached/duplicate and process-preflight coverage in [`main.rs`](../../../crates/bangbang/src/main.rs) and [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs), signed all-class discovery/MSI-X/I/O in [`signed_executable_runs_all_startup_virtio_devices_over_product_pci`](../../../crates/bangbang/tests/executable_hvf_e2e.rs), and direct/contained two-round block and pmem hotplug gates in [`executable_hvf_e2e.rs`](../../../crates/bangbang/tests/executable_hvf_e2e.rs) and [`production_bundle_e2e.rs`](../../../crates/launcher/tests/production_bundle_e2e.rs) |
| `--http-api-max-payload-size <BYTES>` | One `usize`; default 51,200; zero is valid. | Same default and complete non-negative `usize` domain. A zero limit permits bodyless requests and returns 413 for every nonempty body. Request-head bytes have a separate safety bound. | process / I+V | [`parse_http_api_max_payload_size`](../../../crates/bangbang/src/main.rs); zero/max unit cases and [`executable_zero_http_payload_limit_allows_bodyless_requests_only`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--id <ID>` | One value; default `anonymous-instance`; 1–64 UTF-8 bytes; each character is `-` or Unicode alphanumeric. | Exact validation and default. The accepted value is returned unchanged by `GET /`; punctuation, symbols, empty, and overlong values fail before readiness. | process / I+V | [`validate_instance_id`](../../../crates/bangbang/src/main.rs); byte-boundary unit cases plus Unicode identity and invalid/no-socket cases in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--level <LEVEL>` | One value; configure logger level. | Configures the process logger before readiness; supported Firecracker-shaped levels are documented, and invalid input uses exit 152. | observability / I+V | [`LoggerConfigInput` parsing](../../../crates/bangbang/src/main.rs); [`executable_applies_startup_logger_arguments` and `executable_rejects_invalid_logger_level_as_bad_configuration`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--log-path <PATH>` | One value; configure logger output file or FIFO. | Opens the process logger sink before readiness with redacted failures and duplicate-sink protection. In contained mode an exact logger-sink reference adopts the singleton write-only regular-file descriptor with append/nonblocking status and no path reopen; direct mode retains file/FIFO creation behavior. Producer breadth stays with observability records. | observability / I+V | logger startup in [`run`](../../../crates/bangbang/src/main.rs); [`executable_applies_startup_logger_arguments`](../../../crates/bangbang/tests/process_e2e.rs), signed observability cases in [`executable_hvf_e2e.rs`](../../../crates/bangbang/tests/executable_hvf_e2e.rs), and startup-CLI grant proof in [`production_bundle_e2e.rs`](../../../crates/launcher/tests/production_bundle_e2e.rs) |
| `--metadata <PATH>` | One value; initialize MMDS from JSON before startup. | Reads a bounded regular UTF-8 JSON object and initializes the process-local MMDS store before API/no-API readiness under the effective MMDS limit. In contained mode, an exact `bangbang-grant:<GrantId>` claims the singleton read-only startup-metadata descriptor once with the same fail-closed rules; direct mode retains pathname behavior. Guest MMDS transport remains owned by MMDS/network records. | process-MMDS / I+V | [`metadata_content_input_with_authority`, `run`](../../../crates/bangbang/src/main.rs); API and no-API metadata cases in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) plus external metadata verification in [`production_bundle_e2e.rs`](../../../crates/launcher/tests/production_bundle_e2e.rs) |
| `--metrics-path <PATH>` | One value; configure metrics output. | Configures the per-process metrics sink before readiness with redacted errors. In contained mode an exact metrics-sink reference adopts the singleton write-only regular-file descriptor with append/nonblocking status and no path reopen; duplicate initialization rejects before another claim. Direct mode retains file/FIFO creation behavior. Producer breadth stays with observability records. | observability / I+V | metrics startup in [`run`](../../../crates/bangbang/src/main.rs); startup metrics and observability cases in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) plus startup-CLI grant proof in [`production_bundle_e2e.rs`](../../../crates/launcher/tests/production_bundle_e2e.rs) |
| `--mmds-size-limit <BYTES>` | One `usize`; omitted value inherits the effective HTTP limit; zero is valid. | Exact inheritance and complete non-negative `usize` domain. A zero limit permits startup and rejects every serialized object through the MMDS data-store-limit path. | process-MMDS / I+V | [`StartupConfig::effective_mmds_size_limit`](../../../crates/bangbang/src/main.rs); zero/max unit cases and [`executable_zero_mmds_limit_rejects_every_serialized_object`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--module <MODULE>` | One value; configure logger module filtering. | Applies Firecracker-style module-prefix filtering to implemented process logger events before readiness. Producer breadth stays with observability records. | observability / I+V | logger argument handling in [`Args::parse`](../../../crates/bangbang/src/main.rs); [`executable_applies_startup_logger_arguments`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--no-api` | Flag; requires `--config-file`; start and run without an API socket. | Enforces the prerequisite, applies the same supported config path, publishes only no-API readiness, and owns no socket. Clean signals and guest terminal outcomes end the process. | process / I+V | [`run_without_api`](../../../crates/bangbang/src/main.rs); no-API failure/readiness/guest-outcome cases in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) and [`executable_hvf_e2e.rs`](../../../crates/bangbang/tests/executable_hvf_e2e.rs) |
| `--no-seccomp` | Flag; conflicts with `--seccomp-filter`; replace Firecracker's default `vmm`/`api`/`vcpu` Linux filters with empty programs. | Rejected with a fixed name before configuration-file access, VMM/backend construction, readiness, or API socket publication. Direct bangbang already has no Linux filter, so accepting this as a no-op would falsely report the upstream default-to-empty transition. | process / proven-platform-impossible (#1384) | Fixed first-name behavior in [`Args::parse`](../../../crates/bangbang/src/main.rs), full exact/attached/duplicate/conflict unit coverage, and process no-output/no-socket evidence in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--parent-cpu-time-us <MICROS>` | One `u64`; optional; zero through `u64::MAX`. | Exact input domain; contributes to emitted startup CPU diagnostics when `--start-time-cpu-us` is present. | process-observability / I+V | [`StartupTimeConfig`](../../../crates/bangbang/src/main.rs); startup-time unit/process and metrics cases in [`main.rs`](../../../crates/bangbang/src/main.rs) and [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--seccomp-filter <PATH>` | One value; conflicts with `--no-seccomp`; load a bounded bitcode map and install its `vmm`/`api`/`vcpu` classic-BPF programs on Linux threads. | Missing, separated, attached, duplicate, and conflicting forms all return the first fixed name before consuming or opening a path, configuration-file access, VMM/backend construction, readiness, or socket publication. macOS has no public per-thread Linux seccomp installer. | process / proven-platform-impossible (#1384) | Fixed redacted behavior in [`Args::parse`](../../../crates/bangbang/src/main.rs), complete unit matrix there, and exact exit/stderr/no-output/no-socket process proof in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--show-level` | Flag; include logger level. | Enables the level field for implemented process logger events. | observability / I+V | logger configuration in [`Args::parse`](../../../crates/bangbang/src/main.rs); [`executable_applies_startup_logger_arguments`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--show-log-origin` | Flag; include logger callsite origin. | Enables the origin field for implemented process logger events. | observability / I+V | logger configuration in [`Args::parse`](../../../crates/bangbang/src/main.rs); [`executable_applies_startup_logger_arguments`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--snapshot-version` | Flag; early command that prints Firecracker's supported snapshot data-format version. | Early command exists, but prints bangbang native-v1 (`v1.0.0`), not Firecracker's state-artifact format version. | snapshot wave under #1348 / audit | Native-only implementation in [`run`](../../../crates/bangbang/src/main.rs) and native snapshot process tests in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--start-time-cpu-us <MICROS>` | One `u64`; optional; zero through `u64::MAX`. | Exact input domain; reports sampled process CPU time relative to the supplied value and optional parent time. | process-observability / I+V | [`StartupTimeConfig`](../../../crates/bangbang/src/main.rs); startup-time unit/process and metrics cases in [`main.rs`](../../../crates/bangbang/src/main.rs) and [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--start-time-us <MICROS>` | One `u64`; optional; zero through `u64::MAX`. | Exact input domain; reports sampled monotonic startup time relative to the supplied value, saturating at zero. | process-observability / I+V | [`StartupTimeConfig`](../../../crates/bangbang/src/main.rs); startup-time unit/process and metrics cases in [`main.rs`](../../../crates/bangbang/src/main.rs) and [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) |
| `--version` | Flag; early command that prints the running product version. | Prints `bangbang <package-version>` and exits before resource setup. Product branding/version is intentionally bangbang's; the early-command behavior is equivalent. `-V` is an extension. | process / I+V | [`run`](../../../crates/bangbang/src/main.rs); version, alias, precedence, and no-socket cases in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) |

## Platform-excluded seccomp inputs

Firecracker v1.16's default, empty, and custom paths all produce a per-thread
Linux classic-BPF installation contract: nonempty programs first set
`PR_SET_NO_NEW_PRIVS` and then use `seccomp(SECCOMP_SET_MODE_FILTER)`. The
current public macOS SDK and XNU syscall surface expose no `seccomp` operation.
App Sandbox is a fixed signed resource boundary, Endpoint Security is privileged
event monitoring, and private Seatbelt policy is unsupported; none can load the
caller's `vmm`/`api`/`vcpu` return actions. Offline `seccompiler-bin` artifact
creation remains implemented, but compiling or deserializing a map without
installing it is not runtime equivalence.

Both executable names are therefore terminal `proven-platform-impossible`
records, not accepted no-ops. The parser reads only the fixed option name,
returns on its first occurrence, never opens the supplied filter path, and
prints only `unsupported Firecracker argument: --NAME`. Unit and real process
tests cover exact, attached, missing, separated, duplicate, and both conflict
orders while proving empty stdout, the argument exit code, no readiness, and no
API socket.

## Composite process semantics

| Inventory record | Audited result | Disposition and evidence |
| --- | --- | --- |
| `semantic.process:cli-config-readiness-and-api-socket` | Argument parsing precedes process setup. API-only startup publishes one owner-only socket after successful setup; config-file API startup publishes it only after the VM starts; no-API startup never creates it. Failed setup reports no readiness and cleans any owned socket. Concurrent processes have independent controller, MMDS, observability, socket, signal, and VM state. | I+V; production ownership in [`run`, `run_with_api`, and `run_without_api`](../../../crates/bangbang/src/main.rs), with API/config/no-API, failure, conflict, and concurrent-owner coverage in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs) and signed startup coverage in [`executable_hvf_e2e.rs`](../../../crates/bangbang/tests/executable_hvf_e2e.rs). |
| `semantic.process:instance-identity-and-version-output` | Unicode instance identity and product help/version output are implemented. The record also owns snapshot version/description output, whose artifact semantics are native-only. | audit; snapshot wave under #1348. Partial behavior is documented but cannot terminally certify the composite. |
| `semantic.process:signals-exits-fd-and-cleanup` | SIGINT/SIGTERM request clean shutdown; SIGPIPE is nonfatal and counted; Firecracker fatal signals map to stable exit classes. Best-effort fd-table preallocation never clobbers an inherited target descriptor. Normal/error/guest terminal paths join the owned worker, stop schedulers, close resources, and unlink only the socket inode they own. | I+V; production logic and focused unit tests in [`main.rs`](../../../crates/bangbang/src/main.rs), process signal/socket/cleanup cases in [`process_e2e.rs`](../../../crates/bangbang/tests/process_e2e.rs), and signed repeatable lifecycle cases in [`executable_hvf_e2e.rs`](../../../crates/bangbang/tests/executable_hvf_e2e.rs). |
| `tool-operation:firecracker/run` | The executable entrypoint has 19 implemented arguments and two terminal platform-excluded seccomp arguments, but the aggregate still includes PCI and native-only snapshot-version leaves. | audit; remains nonterminal until those independent argument leaves and aggregate ordering reach terminal outcomes. |
| `corpus:design` | The pinned whole file includes process model, isolation, API, device, guest, resource, and Linux mechanism claims. Runtime seccomp now has a terminal macOS conclusion. | audit; broader lifecycle, device, resource, and architecture claims remain owned across later #1348 waves. |
| `corpus:getting-started` | The pinned whole file includes executable, jailer, KVM/Linux host, configuration, boot, and device claims. Its runtime seccomp references now have terminal macOS conclusions. | audit; setup, artifacts, operator workflow, deployment, and other claims remain owned across later #1348 waves. |

## Terminal record set through #1384

Exactly 23 of the 29 process-family records are terminal. Twenty-one are
`implemented-and-verified`: the 19 `I+V` argument rows and the two `I+V`
semantic rows above. `--no-seccomp` and `--seccomp-filter` are the two
`proven-platform-impossible` records. Six remain `audit-required`: two argument
rows, one snapshot-containing semantic record, the aggregate run operation, and
the two broad corpus records.

The repository validates overlay structure and tracked references with
`cargo run -p bangbang-firecracker-capability-audit --locked -- validate` and
validates the exact pinned identity set with `compare --firecracker <checkout>`.
The final parent gate intentionally continues to fail while these and other
families contain nonterminal records.

## Offline seccompiler public tool

The separate `seccompiler-bin` executable implements the five pinned public
seccompiler arguments and the aggregate compile operation; it does not alter
the 29 main-process records above.

- `-t`/`--target-arch` requires `x86_64` or `aarch64`; `-i`/`--input-file`
  requires one policy path; and `-o`/`--output-file` defaults to
  `seccomp_binary_filter.out`. Short options accept attached values. Missing,
  duplicate, positional, unknown, and invalid-UTF-8 invocations emit one fixed
  value-redacted diagnostic and exit 2. Help and the bangbang-branded
  Firecracker-format version exit 0.
- `-b`/`--basic` retains Firecracker v1.16's deprecated behavior of dropping
  argument conditions and rule-level distinctions. `--split-output` treats the
  selected output basename only as a parent selector and writes exactly
  `vmm.bpf`, `api.bpf`, and `vcpu.bpf`; otherwise the selected path receives one
  bitcode 0.6.9 map that Firecracker deserializes as
  `HashMap<String, Vec<u64>>`.
- Runtime input, compilation, serialization, and publication failures exit 1
  with static value-redacted categories. The input is one no-follow,
  nonblocking, regular UTF-8 file capped at 1 MiB. Normal output is checked
  against Firecracker's 100,000-byte consumer limit before filesystem mutation.
- Output publication retains one no-follow directory descriptor, accepts only
  absent or regular final entries, stages synced owner-only complete files,
  requires no-replace/exchange rename support, and identity-checks publication,
  rollback, and cleanup. Observed failures before complete publication restore
  prior entries when identities still match; uncertain rollback and
  post-commit durability/cleanup use distinct errors. Three visible split names
  are not falsely described as one crash-atomic POSIX transaction.

Implementation is in [`src/bin.rs`](../../../tools/seccompiler/src/bin.rs),
[`src/tool.rs`](../../../tools/seccompiler/src/tool.rs), and
[`src/artifact.rs`](../../../tools/seccompiler/src/artifact.rs). Process
validation is in [`tests/cli.rs`](../../../tools/seccompiler/tests/cli.rs),
with independent classic-BPF semantics in
[`tests/semantics.rs`](../../../tools/seccompiler/tests/semantics.rs). The
pinned documentation's install-helper prose maps to the Linux VMM filter
consumer, not this offline tool. #1384 terminally classifies that complete
runtime corpus and its two executable inputs as public-macOS platform
exclusions without expanding the offline tool into an installer.
