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

## Initial Compatibility Tier

The first planned compatibility tier is the smallest boot-oriented API surface:

| Method | Endpoint | Planned purpose |
| --- | --- | --- |
| `GET` | `/` | Describe the microVM instance. |
| `GET` | `/version` | Report the VMM version. |
| `GET` | `/vm/config` | Return the full VM configuration. |
| `PUT` | `/machine-config` | Configure vCPU and memory settings before boot. |
| `PUT` | `/boot-source` | Configure the guest kernel and boot arguments before boot. |
| `PUT` | `/drives/{drive_id}` | Configure block devices before boot. |
| `PUT` | `/actions` | Start the microVM with `InstanceStart`. |

Until the API server and VMM action model exist, these endpoints are
compatibility targets rather than implemented behavior.

## State and Response Scope

The initial tier is pre-boot oriented. Machine configuration, boot source, and
drive configuration are planned pre-boot operations, and `InstanceStart` is the
planned transition into guest execution. Runtime actions after start are outside
this initial tier.

The API should eventually use Firecracker-shaped success and error responses.
Exact status codes, response bodies, and unsupported-endpoint behavior are not
defined by this initial scope and should be specified before endpoint behavior
ships.

## Deferred Firecracker Features

The following Firecracker features are intentionally deferred from the initial
compatibility tier:

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
- pause and resume actions
- PATCH and DELETE hotplug/update behavior

Deferred features should be introduced through narrower capability work that
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

Security review should cover host paths, Unix sockets, FFI boundaries, guest
memory, device I/O, and untrusted API or guest input as those surfaces are
introduced. Performance review should cover startup path, memory mapping, vCPU
run loops, and device I/O when those areas change.
