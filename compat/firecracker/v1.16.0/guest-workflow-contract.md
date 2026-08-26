# macOS Guest Workflow Contract

This contract closes the #1796 slice of the pinned Firecracker v1.16.0 audit.
It binds one public, rootless, networkless Apple Silicon/HVF development
workflow to exact guest artifacts, guest-visible evidence, process ownership,
real signed execution, and two upstream corpus identities. The runtime authority
is [`guest-workflow-audit.json`](guest-workflow-audit.json); the operator source
is [the macOS guest workflow guide](../../../docs/macos-guest-workflow.md).

## Baseline and authority

- Firecracker source/API audit: v1.16.0 commit
  `d83d72b710361a10294480131377b1b00b163af8`.
- Download namespace: Firecracker CI v1.15 arm64. This namespace difference is
  intentional and does not change the source/API baseline.
- Public commands:
  `scripts/run-macos-guest-workflow.py api` and
  `scripts/run-macos-guest-workflow.py no-api`.
- Both modes use the exact manifest kernel, deterministic
  `guest-boot-initrd`, and pinned read-only squashfs root drive.
- Guest success requires `BANGBANG_ROOTFS_WORKFLOW_OK` plus VMM exit zero.
  `BANGBANG_ROOTFS_WORKFLOW_FAIL`, a missing marker, a nonzero/early exit, a
  timeout, or a socket invariant failure is terminal failure.

The initrd entry `/rootfs-poweroff-init` mounts the root drive, reads 401 bytes
from `/mnt/etc/os-release`, requires exactly 400 bytes, compares all bytes to the
manifest-owned SHA-256 identity, emits one fixed marker, and requests guest
poweroff on success and failure. This is an exact stable-content proof for the
pinned image, not a claim that an arbitrary distribution boots.

## Upstream corpus mapping

The source manifest owns two whole-document corpus records. A terminal corpus
record means the applicable macOS/HVF outcome and every non-applicable section
were audited here; it does not assert native execution of incompatible host
instructions.

| Capability | Owner | Disposition |
| --- | --- | --- |
| `corpus:getting-started` | #1796 | `implemented-and-verified` |
| `corpus:rootfs-and-kernel` | #1796 | `implemented-and-verified` |

### `docs/getting-started.md`

The pinned upstream getting-started document demonstrates Firecracker on a
Linux/KVM host. Its applicable lifecycle maps to Bangbang as follows:

| Upstream concern | Checked macOS outcome |
| --- | --- |
| obtain a kernel and root filesystem | fixed HTTPS name/size/SHA/provenance in the manifest; verified repairable cache; no checked-in bytes |
| start the VMM and configure a microVM | signed Bangbang process with exact API requests, or canonical config-file `--no-api` startup |
| boot and observe a guest | deterministic initrd reads exact content through the read-only root block device and emits the fixed serial marker |
| stop the guest/VMM | guest poweroff on both oracle outcomes; success additionally requires process exit zero |
| control-socket lifecycle | one private unique session; exact API socket responses and owned cleanup, or continuous no-API nonpublication |

Its `/dev/kvm`, root/sudo, Firecracker executable, dynamic-latest release,
Linux TAP/iptables, SSH, jailer, optional PCI recipe, and production deployment
steps do not execute as macOS aliases. The workflow uses HVF, needs no root,
publishes no guest network, and makes no production-containment claim.

### `docs/rootfs-and-kernel-setup.md`

The pinned upstream artifact document includes Linux kernel builds, root-mounted
or Docker-created ext4 images, Firecracker CI artifact construction, multiple
architectures, and a separate root/FreeBSD host flow. Bangbang consumes only the
fixed Firecracker CI arm64 kernel and squashfs bytes named by the manifest and
generates its own deterministic initrd. It validates source bytes before reuse
and validates exact guest-visible identity during the signed run.

The Linux build/container/mount recipe, FreeBSD host flow, arbitrary kernel or
distribution support, redistribution/licensing of downloaded bytes, artifact
authentication beyond fixed HTTPS plus SHA-256, and reproducible ext4 bytes are
not claimed. The optional rootless macOS ext4 recipe is separately
sidecar-verified and recipe-deterministic only; it is not the public smoke root
drive.

The checked current direct-test recipe is `direct-boot-v110`. It installs the
production-vmnet guest oracle as an exact mode-`0555` tracked input; historical
v109 sidecars are not accepted for those bytes. That selector is inert in both
public networkless workflow profiles and supplies no positive vmnet evidence.

## API and no-API evidence

API mode starts an unconfigured signed process on an owner-only Unix socket,
then requires the exact `204 No Content` response bytes for:

1. `PUT /machine-config`;
2. `PUT /boot-source`;
3. `PUT /drives/rootfs`; and
4. `PUT /actions` with `InstanceStart`.

No-API mode writes one canonical mode-0600 configuration containing the same
machine, boot source and root drive, starts with `--config-file --no-api`, and
continuously rejects publication of its reserved socket path. Both modes are
executed by the dedicated signed integration selection, not by a parallel
test-only implementation.

## Process, cache, and cleanup boundary

The command owns one process group and one 0700 unique session. Every artifact,
build, readiness, socket, request, guest, termination and thread join has a
checked wall-clock bound. stdout and stderr are drained concurrently; marker
matching crosses read boundaries while retained diagnostics are bounded.

On ordinary failure, signal or interruption, the command terminates the group,
escalates to kill after the bounded grace period, reaps the child, stops the
socket watcher, and removes only a session whose captured device/inode/owner/
mode still match. It never treats the shared `.tmp/guest-artifacts` cache as a
session child. A forced host crash may leave an isolated session; hostile-parent
traversal safety and system-wide garbage collection are explicit nonclaims.

## Checked evidence and nonclaims

The terminal gate is:

```text
cargo run -p bangbang-firecracker-capability-audit --locked -- validate --guest-workflow-final
```

It requires the canonical terminal audit, both implemented profiles, portable
failure evidence, both actual signed modes, this exact two-row scope, and the
ordered nonclaims. Ordinary delivery validation continues to permit unrelated
open #1348 work, while global `--final` remains strict.

The workflow does not claim byte-reproducible ext4, hostile-parent traversal
safety, artifact redistribution or authentication, arbitrary URL/profile
input, a production workflow, external guest networking, arbitrary distro or
FreeBSD guest support, or a crash-atomic image/sidecar pair.
