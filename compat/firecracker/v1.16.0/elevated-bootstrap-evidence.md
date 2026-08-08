# Elevated macOS Bootstrap Evidence

This contract records the #1373 direct-worker result and the #1884
inherited-root follow-up beneath #1371. Together they test two public orderings
for combining Firecracker's chroot-first model with Bangbang's mandatory
production worker boundary. They do not add a public root mode, accept jailer
uid/gid/chroot options, or change a capability disposition by themselves.

## Exact test boundary

The evidence bundle keeps the production layout and identity split:

- the outer launcher is signed with Hardened Runtime and has neither App
  Sandbox nor Hypervisor entitlement;
- the separately signed nested worker has Hardened Runtime and exactly the App
  Sandbox plus Hypervisor entitlements;
- the launcher retains normal static bundle validation and performs the
  suspended/live worker identity checks whenever the child reaches them; and
- both launcher and worker probe entries are compiled only with the
  `elevated-bootstrap-probe` feature. A normal `--no-default-features` build is
  checked for absence of the launcher-owned worker activation, ready and status
  records, and the marker resource, and the normal signed bundle rejects the
  internal launcher argv.

The root wrapper never invokes `sudo`. It requires exact real/effective
uid/gid zero, accepts explicit numeric target uid/gid values, runs with a
closed environment and fixed absolute tools, replaces inherited standard input
with `/dev/null`, and refuses missing root or HVF support rather than skipping.
Caller-provided elevation input therefore cannot be inherited by the launcher
or worker. The wrapper creates only root-owned mode-0700 children beneath the
fixed non-writable `/private/var/root` ancestry. The launcher walks that
ancestry with no-follow directory descriptors, binds the leaf device and inode
into a random nonce record, and transfers only the retained root descriptor as
new filesystem authority, at fixed worker fd 8. The existing fixed production
session and dormant broker endpoints remain closed-role socket descriptors;
the worker receives no host path and cannot reopen the root by name.

For the inherited-root branch, the wrapper copies exactly the complete signed
probe bundle plus the current host's Apple-signed `/usr/lib/dyld` into fixed
`/Bangbang.app` and `/usr/lib/dyld` locations inside a separate private root.
It admits exactly 20 manifest entries, normalizes and records each original
device/inode/owner/group/type/mode tuple, proves the loader bytes match the
host file, and revalidates the loader and complete nested code signatures. The
launcher independently validates the host-visible staged layout/profile and
loader through no-follow descriptors, prepares every spawn object, enters the
root with public `fchdir`/`chroot`/`chdir`, reattests `/`, and only then calls
public `posix_spawn` with the fixed in-root worker path.

In direct modes, the signed worker verifies initial root identity, the
nonce-bound descriptor identity, and exact mode before `fchdir`, public
`chroot(2)`, or `chdir("/")`. If the inherited worker reaches application
entry, it must additionally attest that `/` and cwd are the same exact root,
observe the direct App Sandbox chroot retry denial, and create then destroy one
real HVF VM. No mode executes `setgroups`, `setgid`, or `setuid`. The launcher
reports only fixed mode, stage, and error categories. Target IDs, paths,
nonces, device/inode values, descriptor numbers, signing values, account data,
and environment values are never reported.

## Capable-host result

The final evidence shape was executed on 2026-08-08 on Apple Silicon with
macOS 26.5.2, macOS SDK 26.5, and `kern.hv_support=1`, under explicit operator
root authority. The complete matrix produced these results:

| Process boundary | Mode | Result |
| --- | --- | --- |
| signed outer launcher, no App Sandbox | root control | `chroot` and `chdir("/")` succeeded |
| signed nested App Sandbox + Hypervisor worker | explicit ordinary target, repeated three times | exact root fd and `fchdir` succeeded; `chroot` returned permission denied |
| same worker | uid/gid zero no-drop | exact root fd and `fchdir` succeeded; `chroot` returned permission denied |
| same worker | high unmapped numeric uid/gid syscall case | exact root fd and `fchdir` succeeded; `chroot` returned permission denied |
| two concurrent direct launcher/worker pairs | distinct exact roots and the same explicit target class | both returned the same chroot-stage permission denial |
| same signed App Sandbox + Hypervisor worker, unchrooted | HVF control | exact worker validation and real HVF create/destroy succeeded |
| launcher enters staged root before spawn | inherited-root, repeated three times | static bundle/loader validation and launcher root entry succeeded; `posix_spawn` returned success, but the worker exited before the earliest Ready record (`worker-bootstrap` / `other`) |
| two concurrent complete inherited roots | distinct staged bundle/loader trees | both returned the same `worker-bootstrap` / `other` blocker |

Focused negative cases reject a group-writable root and a symlink root. The
staged manifest also rejects a writable, missing, symlinked, or inode-replaced
loader, a missing nested worker, and an unexpected entry before worker
activation. Every successful, rejected, repeated, and concurrent case leaves
no root or workspace residue. Nonempty-root cleanup preflights all 20 ledger
entries before removing any, then uses only reverse exact `unlink`/`rmdir`;
root cleanup rechecks device, inode, owner, group, and mode and refuses to
remove a replacement.

The inherited result is not a missing-HVF or generally invalid-signature
result: the same exact unchrooted signed worker completed real HVF
create/destroy in the same wrapper run. The inherited branch nevertheless
never reached application entry, root attestation, the direct-chroot sandbox
control, or HVF. No guest, credential transition, lifecycle/API, or typed
resource consumption was attempted.

After those results were established, the checked harness remained at the
evidence boundary: it contains no credential-changing product path. If a
future platform permits the direct sandboxed `chroot`, that branch reports
`unexpected-continuation` before later work. If an inherited worker reaches
entry, it must pass exact root, sandbox-denial, and HVF checks before success.
Either changed result forces fresh implementation research and Challenge.
Connected-socket credentials never cross a credential change in these measured
branches; the harness leaves the normal peer verifier unchanged and makes no
dynamic-versus-snapshot credential claim for #1374.

## Supported conclusion and nonclaims

On the measured public macOS/SDK boundary, a running mandatory App Sandbox
worker cannot directly enter a caller-selected chroot, even with exact root
identity and an already-opened validated directory descriptor. The successful
unsandboxed control distinguishes that result from missing root authority, an
invalid root, or an unavailable `chroot(2)` primitive.

Pre-spawn inheritance is materially different and #1884 measured it rather
than dismissing it. Staging the complete signed bundle and current host dyld
was sufficient for `posix_spawn` to return success after launcher chroot, so
the former assumption that root entry necessarily removes all executable and
loader lookup was too broad. It was not sufficient for the spawned image to
reach the first application record. Because no authenticated worker code ran,
the evidence cannot distinguish a dyld/shared-cache bootstrap dependency from
another pre-entry platform launch dependency and cannot claim inherited App
Sandbox or HVF execution.

This is a reproducible blocker for the exact public staged-bundle/current-dyld
shape, with successful direct-root and unchrooted-HVF controls. It is evidence
for #1371's fresh Challenge, not an inventory disposition by this PR. Removing
App Sandbox, installing a privileged daemon/setuid helper, using private
Seatbelt APIs, or switching to a different VMM process changes the challenged
product/security model; any credible public in-root dependency alternative
must instead be evaluated explicitly by the parent.

This does not prove that numeric `setgroups`/`setgid`/`setuid` alone are
unavailable on macOS, that no larger fixed in-root Darwin dependency set can
bootstrap, or that inherited-root success would supply full resources,
lifecycle, API, guest boot, daemon/crash, grant, or credential semantics. It
does not claim Linux mount/PID/user namespace parity, certify notarized
distribution, or generalize beyond the recorded OS/SDK behavior. A changed
root content or future public platform requires rerunning the wrapper and a
fresh ID-by-ID Challenge.

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
result: inherited-root-worker=blocked stage=worker-bootstrap error=other controls=success cleanup=exact
```
