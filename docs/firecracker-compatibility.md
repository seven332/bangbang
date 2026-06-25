# Firecracker Compatibility Scope

This document describes bangbang's intended Firecracker compatibility scope. It
is a planning reference for future API, VMM, and backend work; it does not mean
the current scaffold implements all listed API behavior.

The current repository defines crate boundaries, endpoint names, a minimal
HTTP-over-Unix-socket API server for `GET /version`, a backend-neutral VM
trait, a minimal Hypervisor.framework VM create/destroy wrapper, and an initial
process startup argument model. There is no broader API request body model,
guest memory mapping, vCPU loop, or kernel loading yet.

## Firecracker Model Alignment

bangbang should follow Firecracker's process model: one `bangbang` process
manages one microVM. Future API work should keep the control plane outside the
guest execution fast path.

The intended public control plane is Firecracker-style HTTP over a Unix domain
socket. API requests should eventually map to explicit VMM actions and VM state
transitions, but this document only defines the initial scope.

## Process Startup CLI

The current `bangbang` executable parses only the first process-lifecycle
arguments and starts the first API socket surface. It binds a Unix socket and
serves `GET /version`, but does not load a configuration file or start a guest.

| Argument | Current behavior | Compatibility notes |
| --- | --- | --- |
| `--api-sock <PATH>` | binds the API Unix socket | Firecracker defaults to `/run/firecracker.socket`; bangbang defaults to `/tmp/bangbang.socket` because macOS does not normally provide `/run`. This is an intentional host-platform difference. |
| `--id <ID>` | parsed and stored | Defaults to Firecracker's `anonymous-instance`. IDs must be 1 to 64 bytes and contain only alphanumeric characters or `-`, matching the Firecracker `v1.16.0` validator policy. |
| `--help`, `-h` | prints help | Help describes the current API socket scope. |
| `--version`, `-V` | prints version | `-V` is retained from the existing bangbang scaffold. |
| `--config-file`, `--no-api` | rejected | Deferred until VM configuration models and no-API startup behavior exist. |
| seccomp, logger, metrics, snapshot, MMDS, boot timer, payload-size, and PCI process flags | rejected | These Firecracker options are Linux-specific, observability-related, or tied to later capability work. They must not be accepted as no-op compatibility shims. |

Only the Firecracker-style `--arg value` form is supported for the initial
startup arguments. The `--arg=value` form is rejected until a separate
compatibility decision expands the CLI parser.

CLI values are untrusted input. Current validation rejects invalid IDs, empty
socket paths, and socket paths containing control characters. API startup also
fails if the configured socket path already exists. Socket cleanup only removes
the socket inode created by the current process during normal shutdown; signal
handling and forced-termination cleanup are not implemented yet. Process CLI
parsing stays outside the future VM/vCPU fast path and should add only trivial
startup overhead. Error and status output avoid echoing path-like CLI values.

### Process Exit Status

The current executable uses a small process exit status contract:

| Exit status | Current meaning | Compatibility notes |
| --- | --- | --- |
| `0` | Help or version completed successfully, or the API server exited without error. | Matches Firecracker's success status. |
| `153` | Startup argument parsing or validation failed. | Matches Firecracker's `ArgParsing` exit code. |
| `1` | API socket bind or accept failure. | Used for non-argument process failures before more specific Firecracker-compatible process errors exist. Per-connection read/write errors do not terminate the API server. |

Firecracker also defines bad-configuration and signal-specific exit codes.
bangbang does not expose those until the corresponding configuration loading,
signal handling, API server, or VM runtime behavior exists.

## Compatibility Baseline

bangbang's first Firecracker compatibility baseline is the upstream
`firecracker-microvm/firecracker` `v1.16.0` release tag:

- tag: `v1.16.0`
- commit: `d83d72b710361a10294480131377b1b00b163af8`

A release tag is the compatibility reference because it represents a published
Firecracker interface. Development branch commits can still inform
implementation research, but they must not redefine bangbang's compatibility
target without an explicit baseline update. A standalone pinned commit is
precise, but it should be tied to a release tag for this project so the baseline
is both reproducible and recognizable.

Use these upstream files and documents as sources of truth when comparing
Firecracker behavior:

- `src/firecracker/swagger/firecracker.yaml` for the published HTTP API surface
- `src/firecracker/src/api_server/parsed_request.rs` for method and path routing
- `src/vmm/src/rpc_interface.rs` for VMM actions and state-dependent behavior
- `docs/device-api.md` for endpoint, device, input, and output dependencies
- `docs/design.md` for process model, thread model, and threat-containment
  expectations

Unreviewed upstream drift in API routing, VMM actions, device behavior, or
published docs must not implicitly change bangbang's target. Future baseline
updates must be explicit pull requests that update this documentation and
describe API, state, documentation, security, performance, and test impact
before changing this reference.

## Support Level Vocabulary

The current scaffold implements only `GET /version` over HTTP on a Unix domain
socket. The support levels below describe compatibility targets for future API
work:

- supported target: planned for the first boot-oriented API implementation
- planned later: expected to be compatible later, but outside the first tier
- deferred: blocked on a separate capability, device, or backend design
- intentionally unsupported: not part of the current macOS/HVF target without a
  later compatibility policy change

For request fields, rejected means the future API should fail the request once
JSON models exist. Optional means the field may be omitted; for Firecracker
fields represented as nullable optional values, explicit `null` should be
treated like omission unless a row says otherwise. Ignored means accepted with
no effect. No supported target field is intentionally ignored. Deferred request
fields should be rejected until their capability is implemented. Some fields
have value-specific policy so Firecracker's explicit default values remain
accepted while feature-enabling values stay out of the first tier. Unknown JSON
fields should be rejected to match Firecracker `v1.16.0` request models that
deny unknown fields.

## Endpoint Compatibility Matrix

The first planned compatibility tier is the smallest boot-oriented API surface.
Rows marked as implemented describe current behavior; the rest describe planned
compatibility targets.

| Method | Endpoint | Support level | Scope notes |
| --- | --- | --- | --- |
| `GET` | `/` | supported target | Describe the microVM instance. |
| `GET` | `/version` | supported target; implemented first | Report the VMM version with a Firecracker-shaped body. |
| `GET` | `/vm/config` | supported target | Return the full VM configuration once configuration models exist. |
| `GET` | `/machine-config` | supported target | Return machine configuration and defaults. |
| `PUT` | `/machine-config` | supported target | Configure vCPU and memory settings before boot. |
| `PUT` | `/boot-source` | supported target | Configure the guest kernel, initrd, and boot arguments before boot. |
| `PUT` | `/drives/{drive_id}` | supported target | Configure initial virtio-block devices before boot. |
| `PUT` | `/actions` | supported target | Start the microVM with `InstanceStart`; other action values are outside the first tier. |
| `PUT` | `/actions` with `SendCtrlAltDel` | intentionally unsupported | Firecracker gates this action on x86 keyboard behavior; the first bangbang target is Apple Silicon. |
| `PUT` | `/logger`, `/metrics` | planned later | Tied to observability work in #17. |
| `PATCH` | `/machine-config` | deferred | Partial updates belong with later state and validation rules. |
| `PUT` | `/cpu-config` | deferred | Needs HVF CPU feature design with VM and boot work in #8 and #10. |
| `PUT` | `/network-interfaces/{iface_id}` | deferred | Tied to virtio network work in #14. |
| `PUT` | `/vsock` | deferred | Tied to virtio vsock work in #15. |
| `GET`, `PUT`, `PATCH` | `/mmds` | deferred | Tied to MMDS work in #16. |
| `PUT` | `/mmds/config` | deferred | Tied to MMDS work in #16. |
| `PUT` | `/snapshot/create`, `/snapshot/load` | deferred | Tied to snapshot and restore work in #19. |
| `GET`, `PUT`, `PATCH` | `/balloon` | deferred | Requires balloon device and runtime update design. |
| `GET`, `PATCH` | `/balloon/statistics` | deferred | Requires balloon statistics design. |
| `PATCH` | `/balloon/hinting/start`, `/balloon/hinting/stop` | deferred | Requires balloon free-page hinting design. |
| `GET` | `/balloon/hinting/status` | deferred | Requires balloon free-page hinting design. |
| `PUT`, `PATCH` | `/pmem/{id}` | deferred | Requires a separate pmem device design. |
| `PUT` | `/entropy`, `/serial` | deferred | Requires separate device and macOS/HVF design work. |
| `GET`, `PUT`, `PATCH` | `/hotplug/memory` | deferred | Requires memory hotplug device and runtime update design. |
| `PATCH` | `/vm` | deferred | Pause and resume state rules belong with #29 and VMM action work. |
| `PATCH` | `/drives/{drive_id}`, `/network-interfaces/{iface_id}` | deferred | Hotplug and runtime update behavior belongs with the relevant device issues. |
| `DELETE` | `/drives/{drive_id}`, `/pmem/{id}`, `/network-interfaces/{iface_id}` | deferred | Firecracker routes these hot-unplug requests in `parsed_request.rs`, but they are not in the `v1.16.0` swagger surface; support needs an explicit compatibility decision. |

## Initial Field Handling Policy

Field policy is based on Firecracker `v1.16.0` schemas and parser behavior. The
future API should use these tables as golden/API test input once JSON models
exist.

| Endpoint | Field | Handling | Notes |
| --- | --- | --- | --- |
| `PUT /boot-source` | `kernel_image_path` | required | Host path to the kernel image; future validation must check access without leaking sensitive path details. |
| `PUT /boot-source` | `initrd_path` | optional | Host path to an initrd; future validation follows the kernel path policy. |
| `PUT /boot-source` | `boot_args` | optional | Firecracker uses its default kernel command line when omitted; later work should define size and character validation. |
| `PUT /boot-source` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /machine-config` | `vcpu_count` | required | Firecracker bounds this to `1..=32`; HVF work must also account for host CPU and thread limits. |
| `PUT /machine-config` | `mem_size_mib` | required | Drives guest memory allocation and mapping; later work must cover bounds and startup performance. |
| `PUT /machine-config` | `smt` | optional when `false`; rejected when `true` | Firecracker defaults this to `false` and rejects `true` on aarch64; the initial HVF target should accept explicit no-SMT config without exposing SMT control. |
| `PUT /machine-config` | `cpu_template` | optional when omitted, `null`, or `None`; deferred for non-`None` templates | Explicit `None` matches Firecracker's deprecated default; non-default CPU templates need a separate HVF compatibility design. |
| `PUT /machine-config` | `track_dirty_pages` | optional when `false`; deferred when `true` | Explicit `false` matches Firecracker's default; enabling dirty tracking belongs with snapshot support. |
| `PUT /machine-config` | `huge_pages` | optional when `None`; rejected for `2M` | Explicit `None` matches Firecracker's default; Linux hugetlbfs does not directly apply to the macOS target. |
| `PUT /machine-config` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /drives/{drive_id}` | path `drive_id` | required | Must be nonempty and contain only alphanumeric characters or `_`. |
| `PUT /drives/{drive_id}` | body `drive_id` | required | Must match the path `drive_id`. |
| `PUT /drives/{drive_id}` | `is_root_device` | required | Identifies whether this drive is the boot device. |
| `PUT /drives/{drive_id}` | `path_on_host` | required initially | Host path for the initial virtio-block target; future validation must cover access, file type, and path redaction in errors. |
| `PUT /drives/{drive_id}` | `is_read_only` | optional | Firecracker defaults omitted virtio-block drives to read-write; future validation should keep write intent clear in errors and user documentation. |
| `PUT /drives/{drive_id}` | `partuuid` | optional | Only meaningful for root-device boot selection. |
| `PUT /drives/{drive_id}` | `cache_type` | optional when `Unsafe`; deferred when `Writeback` | Explicit `Unsafe` matches Firecracker's default and avoids advertising guest flush support; `Writeback` needs macOS-specific correctness and performance review. |
| `PUT /drives/{drive_id}` | `rate_limiter` | optional when absent or `null`; deferred when configured | Non-null rate limiting is tied to future block I/O performance work in #13. |
| `PUT /drives/{drive_id}` | `io_engine` | optional when `Sync`; rejected when `Async` | Explicit `Sync` matches Firecracker's default; `Async` is tied to Linux io_uring and does not directly map to the first macOS target. |
| `PUT /drives/{drive_id}` | `socket` | optional when absent or `null`; deferred when set | Vhost-user-block is outside the first tier; future validation must cover socket path ownership and permissions. |
| `PUT /drives/{drive_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /actions` | `action_type=InstanceStart` | required initially | The only initial action target. |
| `PUT /actions` | `action_type=FlushMetrics` | deferred | Depends on logger and metrics support. |
| `PUT /actions` | `action_type=SendCtrlAltDel` | intentionally unsupported | Firecracker gates this on x86 keyboard behavior; the first target is Apple Silicon. |
| `PUT /actions` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |

Future implementation PRs should derive unit or golden tests from these tables.
User documentation should keep the same support and field-status vocabulary when
API behavior ships. Security review must cover host paths, socket-like fields,
device identifiers, and error messages. Performance review must cover boot path
setup, memory size, and block device I/O when those surfaces are implemented.

## API State and Response Policy

The current scaffold implements the first HTTP API behavior for `GET /version`.
The policy below is the compatibility target for future request parsing, VMM
action mapping, state validation, and golden API tests.

### Initial API State Model

The first API implementation should model the same broad stages as Firecracker:

- pre-boot: configuration requests are accepted and stored before guest
  execution starts
- starting: `PUT /actions` with `InstanceStart` validates the accumulated
  configuration, starts guest execution, and transitions the process out of
  pre-boot state on success
- runtime: the microVM is running; pre-boot-only configuration requests should
  fail with a Firecracker-shaped unsupported-state error
- paused/resumed: deferred until `/vm` state update work defines pause and
  resume behavior

### Initial Operation State Matrix

| Operation | Pre-boot behavior | Runtime behavior | Notes |
| --- | --- | --- | --- |
| `GET /` | supported target; `200` JSON | supported target; `200` JSON | Response state should reflect the current microVM state. |
| `GET /version` | implemented; `200` JSON | implemented; `200` JSON | Body uses Firecracker's `firecracker_version` field shape. |
| `GET /vm/config` | supported target; `200` JSON | supported target; `200` JSON | Returns the accumulated or active VM configuration once models exist. |
| `GET /machine-config` | supported target; `200` JSON | supported target; `200` JSON | Returns machine configuration and defaulted values. |
| `PUT /machine-config` | supported target; `204` empty response on success | unsupported after start; `400` `fault_message` | Pre-boot-only configuration. |
| `PUT /boot-source` | supported target; `204` empty response on success | unsupported after start; `400` `fault_message` | Host path errors must avoid leaking sensitive path details. |
| `PUT /drives/{drive_id}` | supported target; `204` empty response on success | unsupported after start; `400` `fault_message` | Runtime hotplug remains deferred. |
| `PUT /actions` with `InstanceStart` | supported target; `204` empty response on successful transition | unsupported after start; `400` `fault_message` | Startup validation failures should also use `400` `fault_message`. |
| `PUT /actions` with `FlushMetrics` | unsupported before start; `400` `fault_message` | deferred until metrics support exists; future success should use `204` empty response | Firecracker treats this as runtime-only; tied to observability work. |
| `PUT /actions` with `SendCtrlAltDel` | intentionally unsupported; `400` `fault_message` | intentionally unsupported; `400` `fault_message` | Firecracker rejects this on aarch64; bangbang's first target is Apple Silicon. |
| Non-initial endpoints from the endpoint matrix | `400` `fault_message` until their capability exists | `400` `fault_message` until their capability exists | Covers planned later and deferred endpoints; a later capability PR may define more specific state behavior. |
| Unknown endpoint or invalid method/path | `400` `fault_message` | `400` `fault_message` | Matches Firecracker's parser-level invalid path or method handling. |

### Response Policy

| Case | HTTP status | Body policy |
| --- | --- | --- |
| Successful data response | `200 OK` | JSON body with Firecracker-shaped field names. |
| Successful empty response | `204 No Content` | Empty body. |
| Invalid path, invalid method, invalid JSON, unknown field, invalid field, unsupported endpoint, or unsupported state | `400 Bad Request` | JSON object with `fault_message`. |
| Startup, configuration, or VMM action failure | `400 Bad Request` | JSON object with `fault_message`; exact strings can be refined with the implementation. |
| MMDS payload-limit failure | deferred | Firecracker uses `413 Payload Too Large`; define this with MMDS support. |

Future API work should use `fault_message` consistently where Firecracker does.
Exact message strings should be covered by golden tests once the API parser and
VMM action model exist, but this document only defines the initial status/body
shape.

The initial API implementation uses Firecracker's default `51200` byte HTTP
request payload limit. The `--http-api-max-payload-size` process argument
remains rejected until configurable payload limits are introduced explicitly.
Invalid path and method errors use the Firecracker `fault_message` body shape
but intentionally avoid echoing path-like request values.

API request bodies, path identifiers, and host resource paths are untrusted
input. Future implementations must validate them before mutating VMM state and
redact sensitive host path details from error messages. API parsing and response
serialization must stay outside the VM and vCPU fast path; expensive startup,
memory, or device work belongs in explicit VMM actions where it can be measured
and tested.

## Non-Initial Firecracker Features

The following Firecracker features are outside the first compatibility tier.
Their eventual support level should follow the endpoint matrix:

- networking and `network-interfaces`
- vsock
- snapshots
- MMDS
- balloon devices and balloon statistics
- pmem
- entropy device configuration
- serial customization
- metrics and logger configuration
- memory hotplug
- pause and resume VM state updates
- PATCH and DELETE hotplug/update behavior

Non-initial features should be introduced through narrower capability work that
covers behavior, validation, documentation, security, and performance together.

## macOS and HVF Differences

Firecracker targets Linux/KVM. bangbang targets macOS with Apple's
Hypervisor.framework. Some Firecracker host mechanisms therefore need explicit
macOS design work instead of direct implementation:

- KVM-specific VM and vCPU operations need HVF equivalents rather than direct
  KVM ioctl usage.
- Linux seccomp, jailer, cgroups, and namespaces do not directly apply.
- Linux TAP-based networking needs a macOS-specific design.
- Snapshot and device behavior may differ when backed by HVF.

The initial compatibility scope should document these differences without
pretending they are solved.

## Validation Expectations

Every future compatibility change should choose validation appropriate to its
surface:

- unit tests for parsing, configuration, and state transitions
- golden tests for Firecracker-shaped API responses once the API exists
- platform-gated tests for HVF behavior
- boot smoke tests once kernel loading and vCPU execution exist

## Security and Performance Scope

Security review should cover host paths, Unix sockets, FFI boundaries, guest
memory, device I/O, and untrusted API or guest input as those surfaces are
introduced. Performance review should cover startup path, memory mapping, vCPU
run loops, and device I/O when those areas change.

Detailed security and performance analysis belongs with the capability work that
introduces or changes the relevant surface.
