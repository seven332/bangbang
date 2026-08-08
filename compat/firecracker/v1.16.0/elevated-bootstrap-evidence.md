# Elevated macOS Bootstrap Evidence

This contract records the #1373 evidence result for the direct-root operator
branch beneath #1371. It tests whether Firecracker's chroot-first credential
transition can coexist with Bangbang's mandatory production worker boundary;
it does not add a public root mode, accept jailer uid/gid/chroot options, or
change a capability disposition by itself.

## Exact test boundary

The evidence bundle keeps the production layout and identity split:

- the outer launcher is signed with Hardened Runtime and has neither App
  Sandbox nor Hypervisor entitlement;
- the separately signed nested worker has Hardened Runtime and exactly the App
  Sandbox plus Hypervisor entitlements;
- the launcher performs the normal static bundle and suspended/live worker
  identity checks before authorizing the probe; and
- both launcher and worker probe entries are compiled only with the
  `elevated-bootstrap-probe` feature. A normal `--no-default-features` build is
  checked for absence of the launcher-owned worker activation, ready and status
  records, and the marker resource, and the normal signed bundle rejects the
  internal launcher argv.

The root wrapper never invokes `sudo`. It requires exact real/effective
uid/gid zero, accepts explicit numeric target uid/gid values, runs with a
closed environment and fixed absolute tools, and refuses missing root or HVF
support rather than skipping. It creates only root-owned mode-0700 children
beneath the fixed non-writable `/private/var/root` ancestry. The launcher walks
that ancestry with no-follow directory descriptors, binds the leaf device and
inode into a random nonce record, and transfers only the retained root
descriptor as new filesystem authority, at fixed worker fd 8. The existing
fixed production session and dormant broker endpoints remain closed-role
socket descriptors; the worker receives no host path and cannot reopen the
root by name.

The signed worker verifies initial root identity, the nonce-bound descriptor
identity, and exact mode before `fchdir`, public `chroot(2)`, `chdir("/")`,
`setgroups`, `setgid`, or `setuid`. The launcher reports only fixed mode, stage,
and error categories. Target IDs, paths, nonce, device/inode values, descriptor
numbers, signing values, account data, and environment values are never
reported.

## Capable-host result

The final evidence shape was executed on 2026-08-08 on Apple Silicon with
macOS 26.5.2, macOS SDK 26.5, and `kern.hv_support=1`, under explicit operator
root authority. The same private directory and public syscall produced these
results:

| Process boundary | Mode | Result |
| --- | --- | --- |
| signed outer launcher, no App Sandbox | root control | `chroot` and `chdir("/")` succeeded |
| signed nested App Sandbox + Hypervisor worker | explicit ordinary target, repeated three times | exact root fd and `fchdir` succeeded; `chroot` returned permission denied |
| same worker | uid/gid zero no-drop | exact root fd and `fchdir` succeeded; `chroot` returned permission denied |
| same worker | high unmapped numeric uid/gid syscall case | exact root fd and `fchdir` succeeded; `chroot` returned permission denied |
| two concurrent launcher/worker pairs | distinct exact roots and the same explicit target class | both returned the same chroot-stage permission denial |

Focused negative cases also reject a group-writable leaf and a symlink leaf
before worker activation. Every successful, rejected, and concurrent case
leaves the exact private roots empty; cleanup rechecks device, inode, owner,
group, and mode before `rmdir`, and refuses to remove a replacement.

The host had real HVF support and the rejected worker carried the real
Hypervisor entitlement, but no guest was started: mandatory App Sandbox denied
the earlier public chroot syscall before credential transition, lifecycle/API,
typed resource consumption, or HVF construction could begin. Treating later
steps as passing through a mock or a separate invocation would not change that
ordered combination result.

After that result was established, the checked harness was deliberately kept
at the evidence boundary: it contains no dormant credential-changing product
path. If a future platform permits the sandboxed `chroot`, the worker reports
`unexpected-continuation` and exits before `setgroups`, `setgid`, `setuid`,
lifecycle, API, or HVF work. That failure forces a fresh implementation and
Challenge instead of silently treating an unproved continuation as success.
Connected-socket credentials therefore never cross a credential change in the
measured branch; the harness authenticates the initial root peer by exact PID
and effective IDs plus the random nonce, leaves the normal peer verifier
unchanged, and makes no dynamic-versus-snapshot credential claim for #1374.

## Supported conclusion and nonclaims

On the measured public macOS/SDK boundary, a mandatory App Sandbox worker
cannot enter a caller-selected chroot, even when it starts with exact root
identity and receives an already-opened, validated directory descriptor. The
successful unsandboxed control distinguishes this result from missing host
root authority, an invalid root, or an unavailable `chroot(2)` primitive.

Moving `chroot` to an unsandboxed helper cannot change another process's root;
performing it before loading the worker removes the production bundle and
dynamic-loader path needed to spawn that worker; removing App Sandbox,
installing a privileged daemon/setuid helper, or using private Seatbelt APIs
changes the challenged product/security model. Therefore the exact
Firecracker chroot-plus-mandatory-containment branch has a reproducible public
platform blocker and should be dispositioned through #1371's fresh Challenge.

This does not prove that numeric `setgroups`/`setgid`/`setuid` alone are
unavailable on macOS, does not claim Linux mount/PID/user namespace parity,
does not certify notarized distribution, and does not generalize beyond the
recorded OS/SDK behavior. A future public platform change requires rerunning
the wrapper and a fresh ID-by-ID Challenge.

## Reproduction

Build the statically isolated evidence bundle as an ordinary user:

```sh
scripts/build-elevated-bootstrap-probe.sh \
  --output /absolute/absent/path/Bangbang.app
```

Capture target values before elevation, then explicitly run the root wrapper:

```sh
target_uid="$(id -u)"
target_gid="$(id -g)"
sudo /usr/bin/env -i HOME=/var/root PATH=/usr/bin:/bin \
  /bin/bash "$PWD/scripts/run-elevated-bootstrap-probe.sh" \
  --bundle /absolute/absent/path/Bangbang.app \
  --target-uid "$target_uid" \
  --target-gid "$target_gid"
```

The required terminal summary is value-free:

```text
result: app-sandbox-chroot=permission-denied control=success cleanup=exact
```
