# macOS Guest Workflow

Bangbang provides one checked development/demo workflow for booting a pinned
arm64 Linux guest on Apple Silicon with Hypervisor.framework. It is rootless
and networkless, builds and signs Bangbang locally, verifies its guest inputs,
checks exact content through the guest block path, observes guest-requested
poweroff, and cleans its private process/socket session.

Run either public mode from the repository root:

```sh
scripts/run-macos-guest-workflow.py api
scripts/run-macos-guest-workflow.py no-api
```

Both commands prepare the same fixed artifacts and prove the same guest result.
`api` configures the VM through Bangbang's HTTP-over-Unix-socket API. `no-api`
writes the same configuration as canonical JSON and starts with
`--config-file --no-api` without publishing an API socket.

## Prerequisites

The workflow requires:

- an Apple Silicon Mac with Hypervisor.framework enabled;
- Python 3.9 or newer;
- the latest stable Rust toolchain with the `aarch64-apple-darwin` target;
- Xcode command-line signing support (`codesign`); and
- HTTPS access to the pinned Firecracker CI objects only when the verified
  local cache is empty or invalid.

It does not require root, `/dev/kvm`, Docker, a guest network, TAP, iptables,
SSH, vmnet credentials, a production certificate, or a checked-out sibling
Firecracker tree.

The command fails before download/build/session mutation when the host is not
Darwin arm64, HVF is unavailable or disabled, or a required tool is absent.

## Exact artifact authority

The compatibility source/API baseline is Firecracker v1.16.0 commit
`d83d72b710361a10294480131377b1b00b163af8`. Firecracker does not publish a
matching v1.16 CI namespace for this recipe, so the guest bytes intentionally
come from the separately pinned Firecracker CI v1.15 arm64 namespace. That
artifact namespace does not change which source/API release Bangbang audits.

| Input | Provenance | Size | SHA-256 |
| --- | --- | ---: | --- |
| `vmlinux-6.1.155` | Firecracker CI v1.15 arm64 | 17,111,552 | `e3544b10603acbf3db492cb52e000d22ba202cb4b63b9add027565683e11c591` |
| `ubuntu-24.04.squashfs` | Firecracker CI v1.15 arm64 Ubuntu 24.04 | 105,332,736 | `0efb6a3ff2982baa6ca7e3d940966516ba7ddd2df5deb3e6c2161d369a15d608` |
| `guest-boot-initrd` | `scripts/build-guest-boot-initrd.py` | 54,272 | `1057079b072452a762396113867ebc5afa699a0b5c3121e28970ecadd4ba11d0` |

The complete machine-readable authority, URLs and policies live in
[`guest-workflow-audit.json`](../compat/firecracker/v1.16.0/guest-workflow-audit.json).
Bangbang does not vendor, redistribute, relicense or authenticate the downloaded
kernel/rootfs bytes; it downloads the fixed HTTPS objects and requires exact
size plus SHA-256 before use.

## What each run proves

Both profiles configure one vCPU, 256 MiB RAM, the pinned kernel/initrd, and the
pinned squashfs as a read-only root block device. The submitted kernel command
line is:

```text
console=ttyS0 reboot=k panic=1 quiet loglevel=1 rdinit=/rootfs-poweroff-init
```

The MMIO VMM adds its established `pci=off` transport argument. The dedicated
initrd entry mounts devtmpfs and `/dev/vda` as read-only squashfs, then reads 401
bytes from `/mnt/etc/os-release`. It requires the read to return exactly the
pinned 400-byte file and compares every byte. The expected file SHA-256 is
`3e5851448bae5b36f351becde037a8b13b77307279f484eda808f8177d9a4293`.

On a match the guest writes:

```text
BANGBANG_ROOTFS_WORKFLOW_OK
```

On any mount/open/read/length/content failure it writes:

```text
BANGBANG_ROOTFS_WORKFLOW_FAIL
```

Both paths ask the virtual platform to power off. The host command reports
success only when it observes the success marker, never observes the failure
marker, receives VMM exit status zero, and verifies the mode-specific socket
invariant. A marker without clean exit, or clean exit without the marker, fails.

### API mode

`scripts/run-macos-guest-workflow.py api` starts an unconfigured signed VMM in
the private session, waits for the owner-only socket and readiness line, then
sends machine config, boot source, root drive, and `InstanceStart`. Every
request uses a fresh bounded Unix connection and must return exactly:

```text
HTTP/1.1 204 No Content
Content-Length: 0
Connection: close
```

The command finally requires Bangbang to remove its owned socket before the
session is removed.

### No-API mode

`scripts/run-macos-guest-workflow.py no-api` writes one mode-0600 canonical
Firecracker-shaped JSON document under the private session and starts the same
signed executable with `--config-file ... --no-api`. A watcher runs for the
whole child lifetime and fails if the reserved API socket path appears even
briefly.

## Cache and output policy

Verified shared artifacts are retained under `.tmp/guest-artifacts/`:

- downloaded kernel/rootfs entries are reused only after exact size/SHA
  validation and are announced/repaired under a nonblocking advisory lock;
- the generated initrd is reused only when byte-identical to its manifest
  identity; and
- workflow success, failure, timeout or signal never removes these shared
  caches.

Each invocation separately creates one random mode-0700 `bbgw.*` directory
under the platform `TMPDIR`. Its signed VMM, optional private config and API
socket are the only workflow-owned ephemeral objects. The command captures the
session's device, inode, owner and mode and refuses recursive cleanup if that
identity changes. It never follows a session symlink during cleanup.

All artifact, build, readiness, request, guest, termination and output-thread
waits use the fixed manifest bounds. On ordinary error, `SIGINT` or `SIGTERM`,
the command terminates the owned process group, escalates to kill after the
grace period, reaps it, stops the watcher, and removes the still-owned session.
A host crash or `SIGKILL` can leave an isolated `bbgw.*` directory; inspect and
remove such residue according to local operator policy.

## Optional rootless ext4 preparation

The public smoke deliberately boots the pinned read-only squashfs. If a local
test needs ext4, install the Homebrew tools and run:

```sh
brew install squashfs e2fsprogs
scripts/fetch-firecracker-rootfs.sh --format ext4
```

The recipe uses no root mount. Reuse requires a matching `.bangbang.json`
sidecar binding the source identity, requested size, recipe inputs, bounded tool
versions, output size/SHA and a successful `e2fsck -fn`. Tool versions and ext4
metadata can change bytes, so this is recipe-deterministic—not a byte-identical
or crash-atomic image/sidecar claim. The ext4 output is not substituted into the
two public workflow profiles.

## Troubleshooting

- `the macOS guest workflow requires Apple Silicon`: run on Darwin arm64; cross
  compilation is not executable HVF evidence.
- `Hypervisor.framework is not supported` or `disabled`: verify host policy and
  `sysctl -n kern.hv_support` (or `kern.hv.supported`). The command does not
  silently skip.
- `required tool is unavailable`: install the named stable Rust/signing tool;
  do not bypass signing.
- artifact failures: preserve the diagnostic and retry. The checked cache
  policy can repair an invalid regular manifest-owned entry, but rejects
  symlinks/nonregular collisions and concurrent lock ownership.
- build/sign failures: run `cargo check --workspace --all-targets
  --all-features --locked`, confirm the arm64 target and `codesign`, then retry.
- readiness, HTTP, marker or exit timeout: retain the VMM output shown by the
  command. A success marker alone must not be treated as success.
- `private API socket path is too long`: choose a normal short macOS `TMPDIR`;
  the public command does not accept an alternate socket argument.
- cleanup warning: the command intentionally refuses to delete a replaced
  session root. Inspect the isolated path using local ownership policy.

For portable failure tests and the real signed selection, see
[Testing Guide](testing.md#macos-guest-workflow). For exact compatibility
mapping and nonclaims, see the
[checked contract](../compat/firecracker/v1.16.0/guest-workflow-contract.md).

## Explicit nonclaims

This workflow is not a general image manager, arbitrary VM/device CLI,
persistent supervisor, production launcher, App Sandbox containment path,
networking setup, performance recipe, artifact distribution service, or
artifact-authentication system. It does not claim arbitrary distro or FreeBSD
guest boot, private Apple APIs, restricted vmnet credentials, Linux/KVM,
`/dev/kvm`, TAP/iptables, jailer/seccomp/cgroups/namespaces, root-mounted image
construction, hostile-parent traversal safety, byte-reproducible ext4, or a
crash-atomic image/sidecar pair.
