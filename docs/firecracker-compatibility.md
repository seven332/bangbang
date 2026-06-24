# Firecracker Compatibility Scope

This document describes bangbang's intended Firecracker compatibility scope. It
is a planning reference for future API, VMM, and backend work; it does not mean
the current scaffold implements the listed API behavior.

The current repository only defines crate boundaries, endpoint names, a
backend-neutral VM trait, and a minimal Hypervisor.framework VM create/destroy
wrapper. There is no API server, JSON request/response model, `--api-sock`
argument, guest memory mapping, vCPU loop, or kernel loading yet.

## Firecracker Model Alignment

bangbang should follow Firecracker's process model: one `bangbang` process
manages one microVM. Future API work should keep the control plane outside the
guest execution fast path.

The intended public control plane is Firecracker-style HTTP over a Unix domain
socket. API requests should eventually map to explicit VMM actions and VM state
transitions, but this document only defines the initial scope.

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

The current scaffold still implements no HTTP API behavior. The support levels
below describe compatibility targets for future API work:

- supported target: planned for the first boot-oriented API implementation
- planned later: expected to be compatible later, but outside the first tier
- deferred: blocked on a separate capability, device, or backend design
- intentionally unsupported: not part of the current macOS/HVF target without a
  later compatibility policy change

For request fields, rejected means the future API should fail the request once
JSON models exist. Ignored means accepted with no effect. No supported target
field is intentionally ignored. Deferred request fields should be rejected until
their capability is implemented. Unknown JSON fields should be rejected to match
Firecracker `v1.16.0` request models that deny unknown fields.

## Endpoint Compatibility Matrix

The first planned compatibility tier is the smallest boot-oriented API surface.
This matrix does not imply that the current scaffold implements the endpoints.

| Method | Endpoint | Support level | Scope notes |
| --- | --- | --- | --- |
| `GET` | `/` | supported target | Describe the microVM instance. |
| `GET` | `/version` | supported target | Report the VMM version with a Firecracker-shaped body. |
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
| `PUT /boot-source` | `boot_args` | optional | Kernel command line string; later work should define size and character validation. |
| `PUT /boot-source` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /machine-config` | `vcpu_count` | required | Firecracker bounds this to `1..=32`; HVF work must also account for host CPU and thread limits. |
| `PUT /machine-config` | `mem_size_mib` | required | Drives guest memory allocation and mapping; later work must cover bounds and startup performance. |
| `PUT /machine-config` | `smt` | rejected initially | Apple Silicon has no direct SMT setting for the initial HVF target. |
| `PUT /machine-config` | `cpu_template` | deferred | Firecracker CPU templates need a separate HVF compatibility design. |
| `PUT /machine-config` | `track_dirty_pages` | deferred | Snapshot support is outside the first tier. |
| `PUT /machine-config` | `huge_pages` | rejected initially | Linux hugetlbfs does not directly apply to the macOS target. |
| `PUT /machine-config` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /drives/{drive_id}` | path `drive_id` | required | Must be nonempty and contain only alphanumeric characters or `_`. |
| `PUT /drives/{drive_id}` | body `drive_id` | required | Must match the path `drive_id`. |
| `PUT /drives/{drive_id}` | `is_root_device` | required | Identifies whether this drive is the boot device. |
| `PUT /drives/{drive_id}` | `path_on_host` | required initially | Host path for the initial virtio-block target; future validation must cover access, file type, and path redaction in errors. |
| `PUT /drives/{drive_id}` | `is_read_only` | required initially | Required for the first virtio-block policy. |
| `PUT /drives/{drive_id}` | `partuuid` | optional | Only meaningful for root-device boot selection. |
| `PUT /drives/{drive_id}` | `cache_type` | deferred | Cache semantics need macOS-specific correctness and performance review. |
| `PUT /drives/{drive_id}` | `rate_limiter` | deferred | Tied to future block I/O performance work in #13. |
| `PUT /drives/{drive_id}` | `io_engine` | rejected initially | Firecracker's Linux I/O engine choices do not directly map to the first macOS target. |
| `PUT /drives/{drive_id}` | `socket` | deferred | Vhost-user-block is outside the first tier; future validation must cover socket path ownership and permissions. |
| `PUT /drives/{drive_id}` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |
| `PUT /actions` | `action_type=InstanceStart` | required initially | The only initial action target. |
| `PUT /actions` | `action_type=FlushMetrics` | deferred | Depends on logger and metrics support. |
| `PUT /actions` | `action_type=SendCtrlAltDel` | intentionally unsupported initially | Firecracker gates this on x86 keyboard behavior; the first target is Apple Silicon. |
| `PUT /actions` | unknown fields | rejected | Matches Firecracker's strict request model behavior. |

Future implementation PRs should derive unit or golden tests from these tables.
User documentation should keep the same support and field-status vocabulary when
API behavior ships. Security review must cover host paths, socket-like fields,
device identifiers, and error messages. Performance review must cover boot path
setup, memory size, and block device I/O when those surfaces are implemented.

## State and Response Scope

The initial tier is pre-boot oriented. Machine configuration, boot source, and
drive configuration are planned pre-boot operations, and `InstanceStart` is the
planned transition into guest execution. Runtime actions after start are outside
this initial tier.

The API should eventually use Firecracker-shaped success and error responses.
Exact status codes, response bodies, and unsupported-endpoint behavior are not
defined by this initial scope and should be specified before endpoint behavior
ships.

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
