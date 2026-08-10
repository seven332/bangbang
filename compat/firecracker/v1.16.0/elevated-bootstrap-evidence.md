# Elevated macOS Bootstrap Evidence

This contract records the #1373 direct-worker result, the #1884 inherited-root
follow-up, the #1885 no-chroot credential-transition result, the #1889 / #1891
target-owned runtime continuation results, and the #1893 post-transition guest
result plus the #1895 launcher-created API-listener result beneath #1371.
Together they test public chroot orderings, numeric
credential bootstrap, same-process continuation into the ordinary production
lifecycle, and real guest execution against Bangbang's mandatory worker
boundary. They do not add a public root mode, accept jailer uid/gid/chroot
options, or change a capability disposition by themselves.

## Exact test boundary

The evidence bundle keeps the production layout and identity split:

- the outer launcher is signed with Hardened Runtime and has neither App
  Sandbox nor Hypervisor entitlement;
- the separately signed nested worker has Hardened Runtime and exactly the App
  Sandbox plus Hypervisor entitlements;
- the launcher retains normal static bundle validation and performs the
  suspended/live worker identity checks whenever the child reaches them; and
- both launcher and worker probe entries are compiled only with the
  `elevated-bootstrap-probe` feature, while the representative target-runtime
  grant consumer additionally requires `grant-integration-probe`. A normal
  `--no-default-features` build is checked for absence of launcher-owned worker
  activation, ready/status records, credential and runtime modes/phases/steps,
  the `BBA1` continuation, `BBN1` session authority, guest evidence channel,
  grant activation, boundary vocabulary, and all evidence marker resources.
  The normal signed bundle dynamically rejects the historical, credential,
  runtime, guest, and grant-probe internal argv. The scan also excludes the
  dedicated `BBL1` listener handoff and both role-specific listener evidence
  markers from normal launcher and worker artifacts.

The root wrapper never invokes `sudo`. It requires exact real/effective
uid/gid zero, accepts explicit numeric target uid/gid values, runs with a
closed environment and fixed absolute tools, replaces inherited standard input
with `/dev/null`, and refuses missing root or HVF support rather than skipping.
Caller-provided elevation input therefore cannot be inherited by the launcher
or worker.
The wrapper is a manual capable-host certification artifact, not an ordinary
PR or CI gate. CI builds and inspects the isolated bundle and verifies its
fail-closed non-root boundary; deterministic tests cover the authority,
session, fault, and recovery contracts without elevation.
Historical roots remain root-owned mode-0700 children beneath the
fixed non-writable `/private/var/root` ancestry; runtime-continuation roots are
instead created with the exact selected target/no-drop ownership. The launcher walks that
ancestry with no-follow directory descriptors, binds the leaf device and inode
into a random nonce record, and transfers only the retained root descriptor as
new filesystem authority, at fixed worker fd 8. The existing fixed production
session and dormant broker endpoints remain closed-role socket descriptors;
the worker receives no host path and cannot reopen the root by name.

For the inherited-root branch, the wrapper copies exactly the complete signed
probe bundle plus the current host's Apple-signed `/usr/lib/dyld` into fixed
`/Bangbang.app` and `/usr/lib/dyld` locations inside a separate private root.
It admits exactly 22 manifest entries, normalizes and records each original
device/inode/owner/group/type/mode tuple, proves the loader bytes match the
host file, and revalidates the loader and complete nested code signatures. The
launcher independently validates the host-visible staged layout/profile and
loader through no-follow descriptors, prepares every spawn object, enters the
root with public `fchdir`/`chroot`/`chdir`, reattests `/`, and only then calls
public `posix_spawn` with the fixed in-root worker path.

In direct chroot modes, the signed worker verifies initial root identity, the
nonce-bound descriptor identity, and exact mode before `fchdir`, public
`chroot(2)`, or `chdir("/")`. If the inherited worker reaches application
entry, it must additionally attest that `/` and cwd are the same exact root,
observe the direct App Sandbox chroot retry denial, and create then destroy one
real HVF VM. Those historical modes never execute `setgroups`, `setgid`, or
`setuid`, and their output remains unchanged.

The separate no-chroot credential modes reuse the same static, suspended, and
live code validation, root descriptor, closed environment, fixed lifecycle
stream, and fixed grant datagram. A nonce-bound three-message datagram barrier
proves endpoint possession before the first transition. The worker transitions
first, reports its self-state and initial/worker-only observations, then the
launcher transitions and responds; the worker sends the final after-both
observation before both processes exit. The external root wrapper retains only
bounded supervision and exact cleanup.

The three runtime-continuation modes perform that same credential exchange but
do not respawn either endpoint. The launcher sends one fixed, canonical,
nonce-bound `BBA1` acknowledgment only after the credential transcript is
complete and both the lifecycle stream and grant datagram are live and empty.
The same launcher and worker PIDs then continue over those same transports.
After exact target/no-drop and live-worker reattestation, the permanently
transitioned launcher generates the lifecycle session, creates its random
mode-0700 child beneath the exact target-owned root, and opens three independent
session descriptions. It sends one fixed canonical `BBN1` record plus exactly
one transfer descriptor and consumes the sender alias. `BBN1` binds the
bootstrap mode, nonce, target, root identity, nonzero lifecycle session, and
session identity; all flags and reserved bytes are closed.

The worker receives, validates, adopts, and exclusively locks that descriptor
before it emits ordinary lifecycle `Hello`. The later `Start` must carry the
same session before policy installation and descriptor-relative entry;
`Prepared` must report the same inode, and the launcher must observe the live
lock through a separately opened description before grants begin. The existing
read-only, write-only, and create-children grant batch then commits atomically,
the target performs the complete allowed/denied workload, and ordinary
`Proceed`/`Starting`/terminal supervision and reap-before-cleanup complete.
Feature-only closed fault points cover acknowledgment, session creation/open,
authority send/receive/validation, lock, enter, `Prepared`, grant, proceed, and
terminal boundaries without changing lifecycle v5 or normal bundle behavior.

The six #1893 guest modes append API and no-API workloads for mapped,
retained-root, and SDK-maximum unmapped identities without changing that
prefix. The same
authenticated grant datagram carries two closed late reattestation barriers
before the first guest resource claim and before HVF creation, followed by
value-free HVF-created and guest-shutdown reports. Immutable kernel, initrd,
rootfs, and no-API configuration artifacts are prepared and verified as the
ordinary user, sealed into the evidence bundle, and only reverified and opened
by exact identity under root. The root wrapper never downloads, repairs, or
builds guest inputs.

No-API consumes the canonical config and boot/output grants through the normal
startup path. #1893 API uses the normal contained API directory grant and
signed descriptor-relative binder, while the transitioned launcher owns the
bounded HTTP state machine. #1895 preserves that historical result and replaces
only the feature-only listener producer: after the worker consumes the exact
claim, it sends a canonical zero-right `BBL1` request; the transitioned launcher
enters the retained exact anchor, binds fixed final `evidence-api.sock`, and
sends one canonical acknowledgment plus exactly one listener descriptor. A
complete send releases the launcher descriptor alias while retaining exact
post-reap cleanup metadata. The worker must validate and adopt the listener,
sync the existing socket ownership record, and install its exact cleanup guard
before API readiness. Guest success requires the exact HTTP responses, serial
oracle, guest poweroff, ordered logger evidence, bounded metrics, lifecycle
terminal, replacement-safe authority cleanup, and zero residue; launcher
output alone is not sufficient.

Each runtime case uses a mode-0700 root and a separate bounded workspace with a
root-owned traversal-only ancestry and exact target-owned fixtures. A root-owned
ledger binds the workspace, its input, output, create-children directory,
outside-denial sentinel, and manifest by identity and metadata. Cleanup only
removes the exact unchanged objects and independent final scans cover roots,
workspaces, sockets, launchers, and workers.

For a nonzero target, each endpoint executes public `setgroups(0, null)`,
`setgid`, and `setuid` in that order. Darwin's legacy `getgroups` surface reports
the current effective gid as the sole access-list entry after supplementary
groups are cleared, so the checked postcondition is `effective-only`, not a
literal zero-length list. Real/effective ids must equal the target, and attempts
to restore uid zero, gid zero, or a root group must all return permission denied
before any later work. Retained-root performs no credential mutation and is
reported as no-drop. The SDK-maximum unmapped numeric class uses no account or
home lookup and is measured by the same numeric postconditions and runtime
gates as the mapped class.

Stream `getpeereid` and `LOCAL_PEERCRED`, live `LOCAL_PEERPID`, datagram
`LOCAL_PEERCRED`, and opaque `LOCAL_PEERTOKEN` comparison remain separate
fields. Raw IDs, paths, PIDs, tokens, nonces, descriptor numbers, signatures,
account data, and environment values never enter records, diagnostics, or the
checked result.

## Capable-host result

The final evidence shape was executed on 2026-08-10 on Apple Silicon with
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
| unsandboxed signed launcher credential control | mapped ordinary target and SDK-maximum unmapped target | ordered transition, `effective-only` postcondition, and all three restoration denials completed |
| same launcher control | uid/gid-zero retained-root | initial credentials/groups remained exact with no mutating credential call |
| fixed signed launcher + App Sandbox/Hypervisor worker, no chroot | mapped ordinary target, repeated three times | worker-first and launcher-second transitions both completed irreversibly |
| same fixed pair | uid/gid-zero retained-root, repeated three times | both endpoints retained exact root without claiming privilege separation |
| same fixed pair | SDK-maximum unmapped numeric target | both endpoint syscalls, postconditions, and restoration denials completed independently of account-backed runtime feasibility |
| two concurrent fixed credential pairs | distinct exact roots/workspaces | both ordinary-target transitions completed with byte-identical bounded results |
| same fixed pair continued in place | mapped ordinary target, repeated three times | credential exchange and `BBA1` completed; the launcher created and published the target-owned session through `BBN1`; worker adoption, independent lock proof, exact grants, terminal lifecycle, and cleanup completed |
| same fixed pair continued in place | uid/gid-zero retained-root, repeated three times | the identical authority/grant/lifecycle/cleanup path completed with explicit no-drop semantics |
| same fixed pair continued in place | SDK-maximum unmapped numeric target, repeated three times | both live code/profile revalidations and the complete authority/grant/lifecycle/cleanup path completed without an account lookup or identity-check bypass |
| two concurrent runtime-continuation pairs | distinct exact roots and workspaces | both mapped-target pairs completed with distinct sessions and byte-identical bounded result lines |
| same fixed pair, real no-API guest | mapped ordinary target, retained-root no-drop, and SDK-maximum unmapped target, each repeated three times | both late reattestations, exact boot/output grants, real HVF creation, the fixed guest oracle, guest-requested poweroff, ordinary terminal ownership, and cleanup completed |
| two concurrent real no-API guests | distinct mapped-target roots, sessions, artifacts, outputs, launchers, and workers | both completed independently with no API socket publication |
| #1893 ordinary-binder contained API guest | mapped ordinary and SDK-maximum unmapped targets, each repeated three times | stopped at `api-socket-publication` / `other`: App Sandbox reinitialization rejected the post-transition binder before application entry |
| #1893 ordinary-binder contained API guest | uid/gid-zero retained-root, repeated three times | stopped at the same value-free API boundary: the reinitialized App Sandbox denied creation of the descriptor-rooted staging socket |
| two concurrent #1893 ordinary-binder API attempts | distinct mapped-target roots, sessions, and API authorities | both returned the same `api-socket-publication` boundary and cleaned exactly |
| #1895 launcher-created final listener | mapped ordinary, uid/gid-zero retained-root, and SDK-maximum unmapped targets, each repeated three times | the request, direct final bind, exactly-one-descriptor transfer, and launcher alias release completed; the signed worker then stopped in the closed receive/adoption interval reported as `api-listener-adoption` / `other` before readiness |
| two concurrent #1895 launcher-created-listener attempts | distinct mapped-target roots, sessions, anchors, sockets, outputs, launchers, and workers | both returned the same `api-listener-adoption` boundary and cleaned exactly |

Focused negative cases reject a group-writable root and a symlink root. The
staged manifest also rejects a writable, missing, symlinked, or inode-replaced
loader, a missing nested worker, and an unexpected entry before worker
activation. Every successful, blocked, rejected, repeated, and concurrent case
leaves no process, root, workspace, or named-socket residue. The launcher-first
death case removes only the pre-recorded empty target session after both signed
endpoints are gone; an identity replacement is preserved and fails the proof.
Nonempty-root cleanup preflights all 27 ledger entries before removing any,
then uses only reverse exact `unlink`/`rmdir`;
root cleanup rechecks device, inode, owner, group, and mode and refuses to
remove a replacement.

Across the completed nonzero transitions, both stream credential surfaces kept
their documented connection-time root snapshot while live stream PID remained
exact. Connected-datagram `LOCAL_PEERCRED` returned the bounded unsupported
class; `LOCAL_PEERTOKEN` changed after a credential transition and remained
unchanged for retained-root; datagram `LOCAL_PEERPID` remained exact after the
nonce-bound possession barrier. No datagram `getpeereid` claim is made.

The inherited result is not a missing-HVF or generally invalid-signature
result: the same exact unchrooted signed worker completed real HVF
create/destroy in the same wrapper run. The inherited branch nevertheless
never reached application entry, root attestation, the direct-chroot sandbox
control, or HVF. The runtime continuation separately reached both signed
application endpoints, completed the exact acknowledgment, and reached the
private pre-`Hello` `BBN1` adoption gate for all three runtime identity classes.
Each class then completed exact `Start`/`Prepared` binding, representative typed
grant commitment and consumption, `Proceed`/`Starting`/terminal ownership, and
exact session recovery/cleanup. #1893 then kept that exact prefix and measured
real guests. All three no-API identity classes completed late resource/HVF
reattestation, exact grant-backed guest startup, real HVF creation, the closed
guest oracle, guest-requested poweroff, terminal ownership, and cleanup. All
three API classes stopped earlier at `api-socket-publication`: mapped and
unmapped binder images failed during App Sandbox container initialization,
while retained-root reached the binder but App Sandbox denied creation of its
descriptor-rooted staging socket. No API configuration request or API-started
guest ran after that boundary. #1895 then removed only that binder/publication
step from the evidence shape. In every identity class the transitioned launcher
accepted the fixed request, directly bound the final target-owned listener,
sent its one descriptor, and released its alias. The already-running signed App
Sandbox worker stopped in the closed receive/adoption interval reported as
`api-listener-adoption` / `other` before durable ownership, readiness, HTTP
configuration, or HVF. Two concurrent mapped attempts returned the same
boundary. This result establishes the producer and transfer prefix, but does
not identify the exact failing operation or kernel/sandbox sub-cause inside
that closed interval. Daemon/crash convergence remains unmeasured.

After those results were established, the checked harness remained at the
evidence boundary: it contains no credential-changing product path. If a
future platform permits the direct sandboxed `chroot`, that branch reports
`unexpected-continuation` before later work. If an inherited worker reaches
entry, it must pass exact root, sandbox-denial, and HVF checks before success.
Either changed result forces fresh implementation research and Challenge. The
harness leaves the normal peer verifier and public launch policy unchanged.
All reachable runtime and no-API guest fault cases reached their exact closed
boundary, including the launcher-create/open and authority-send paths, worker
receive/validate/lock/enter/`Prepared` exits, grant/proceed/terminal paths, late
reattestation, HVF creation, guest execution/oracle/poweroff, endpoint death,
and cleanup. #1895 additionally reaches deterministic listener request, bind,
transfer, and adoption faults; API configuration and later faults remain
ineligible because the measured adoption boundary occurs first. Exact object
state and the final residue scan remained clean. #1891 is the separately
challenged launcher-created-session authority result; it is not a hidden
fallback in #1889. #1885, #1889, #1891, #1893, and #1895 are evidence results; product
uid/gid behavior still requires the parent-selected continuation.

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

The measured public credential primitives are available in the exact signed
no-chroot process shape: mapped ordinary and SDK-maximum unmapped transitions
both completed, while zero remained an explicit no-drop class. This is not yet
a Firecracker uid/gid implementation. Same-process continuation is viable
through exact acknowledgment, launcher-created session authority, worker
adoption, the representative grant workload, and ordinary terminal cleanup for
mapped, retained-root, and SDK-maximum unmapped numeric identities on the
measured host. This establishes that narrow feature-only process/resource
composition. It now also establishes real no-API guest/HVF execution after the
transition for all three identities. #1893 records the ordinary contained API
binder block before publication. #1895 proves that the already-transitioned
launcher can instead bind the fixed final listener and transfer exactly one
descriptor, but the same worker then stops in the closed receive/adoption
interval before durable ownership or readiness. It therefore establishes
neither API configuration nor an API-started guest and does not establish the
exact cause within that interval. Any next producer, helper, signing, sandbox,
or topology alternative requires a fresh parent Challenge; this evidence does
not weaken App Sandbox, use a private extension API, or treat a different
topology as equivalent. Daemon/
crash convergence, public policy, and aggregate jailer behavior remain
unmeasured. It also does not prove that a larger fixed in-root Darwin dependency
set or a materially different security model cannot work; nor does it claim
Linux mount/PID/user namespace parity, notarized distribution, or behavior
beyond the recorded OS/SDK. A changed platform result requires rerunning the
wrapper and a fresh ID-by-ID Challenge.

## Reproduction

Build the statically isolated evidence bundle as an ordinary user at an absent
path whose ancestry is traversable by every selected numeric identity. A fresh
directory beneath `/private/tmp` is the recommended capable-host location:

```sh
scripts/build-elevated-bootstrap-probe.sh \
  --output /private/tmp/bangbang-evidence/Bangbang.app
```

Capture target values before elevation, then explicitly run the root wrapper
through `sudo` (substitute the captured numeric values and absolute paths):

```sh
/usr/bin/sudo -- /bin/bash /absolute/repo/scripts/run-elevated-bootstrap-probe.sh \
  --bundle /private/tmp/bangbang-evidence/Bangbang.app \
  --target-uid 501 \
  --target-gid 20
```

The required terminal summary remains value-free and includes credential and
runtime results, observations, residue, and nonclaim classes:

```text
result: inherited-root-worker=blocked stage=worker-bootstrap error=other credential-ordinary=complete credential-retained-root=complete-no-drop credential-unmapped=complete runtime-mapped=complete runtime-retained-root=complete-no-drop runtime-unmapped=complete authority=consumed lock=independent grants=committed lifecycle=terminal controls=complete cleanup=exact
guest-matrix: api-mapped=blocked-listener-adoption api-retained-root=blocked-listener-adoption-no-drop api-unmapped=blocked-listener-adoption no-api-mapped=complete no-api-retained-root=complete-no-drop no-api-unmapped=complete repeats=three concurrency=no-api-complete-api-isolated-blocked faults=no-api-reachable-api-through-adoption deaths=no-api-worker-first-launcher-first tamper=rejected-both-workloads adoption-replacement=no-api-preopened-api-rejected-at-grant cleanup=exact
observations: stream-eid=snapshot stream-cred=snapshot stream-pid=exact datagram-cred=unsupported datagram-token=changed-or-unchanged datagram-pid=exact
residue: roots=zero workspaces=zero sockets=zero launchers=zero workers=zero
nonclaims: daemon-crash=unmeasured public-policy=unchanged chroot=unresolved aggregate-jailer=nonterminal
```
