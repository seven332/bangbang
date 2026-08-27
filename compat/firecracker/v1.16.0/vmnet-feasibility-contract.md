# Entitlement-free vmnet feasibility contract

This contract records the #1930 feasibility decision for the pinned
Firecracker v1.16.0 network-setup corpus and Bangbang's aggregate vmnet
semantic. It changes only these two records:

- `corpus:network-setup`
- `semantic.network:virtio-net-vmnet-policy-and-connectivity`

Both move from `audit-required` to `missing-platform-feasible`. The exact
inventory becomes `383 implemented / 0 audit-required / 2
missing-platform-feasible / 33 proven-platform-impossible`. Neither capability
is implemented by this transition, and #1378 remains their delivery owner.

## Reviewed contract and platform boundary

The upstream authority is `docs/network-setup.md` from Firecracker v1.16.0 at
commit `c161b6661d4362a49d1978e0cafc5e7a6e5cebf6`. Its TAP, routing, guest
configuration, multi-guest allocation, and cleanup requirements remain the
compatibility contract; Bangbang maps the applicable host data path to Apple's
vmnet API.

Apple's public [vmnet API](https://developer.apple.com/documentation/vmnet)
defines host/shared network modes and complete Ethernet packet I/O. Apple's
public [`com.apple.vm.networking`
entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com_apple_vm_networking)
describes restricted authority for using vmnet without root escalation. #1930
assumes that entitlement, an approved provisioning profile, and a corresponding
signing identity are absent. Explicit operator-authorized root execution is the
feasibility boundary.

Root-direct execution is test evidence only. It is not a supported production
topology and does not replace the #1378 design: an unprivileged launcher, a
minimal one-shot privileged provider, one per-interface owner that starts vmnet
and irreversibly drops privilege, and a contained HVF worker using a remote
packet provider.

## Preparation and authority split

Preparation runs as the ordinary user:

```text
scripts/prepare-elevated-vmnet-evidence.sh --output /absolute/absent/directory
```

It fetches and verifies the pinned kernel/rootfs, builds the static aarch64
guest oracle and exact Rust harnesses, builds and ad-hoc signs the direct
Hypervisor-entitled Bangbang executable, and builds the separately ad-hoc-signed
entitlement-free `bangbang-vmnet-provider`. It validates the closed artifact
shape and runs both ordinary-user negative controls. The v111 rootfs recipe is
a distinct identity; v110 and the optional Apple-authorized production
certifier retain their existing meaning and digest.

The run wrapper requires that its caller already holds exact root authority:

```text
scripts/run-elevated-vmnet-evidence.sh \
  --prepared /absolute/prepared/directory \
  --target-uid DECIMAL_UID \
  --target-gid DECIMAL_GID
```

The wrapper never invokes `sudo`, builds, downloads, signs, discovers accounts
or credentials, or inherits standard input. A caller may use its own
authorization mechanism, but no password or authorization token is a repository
input, artifact, log field, or child-process descriptor.

The prepared directory and every member have an exact owner, type, link count,
mode, size limit, digest, sidecar, signature, and entitlement policy. The root
runner copies that immutable shape into one private root-owned stage. Under a
closed environment it invokes only four distinct positive prebuilt test names:
provider data lifecycle, provider cancellation, direct dropped owner, and direct
guest. The provider data and guest names each run twice for clean-repeat proof.

## Exact evidence result

On the supported Apple Silicon host, without Apple vmnet authorization, the
checked workflow requires all of the following categorical outcomes:

| Gate | Required result |
| --- | --- |
| Ordinary-user negative control | The identical ad-hoc-signed direct binary reaches the public `vmnet:shared` start, receives exact HTTP 400 with a fixed vmnet denial category, then exits without its API socket or process group. |
| Minimal provider broker/owner | A root-owned, single-link, non-writable provider image starts one suspended exact-image owner through default-close descriptors. The owner starts shared vmnet while root, irreversibly drops and re-attests the configured ordinary uid/gid, then completes provider-v1 Hello/readiness/read/write/stop/shutdown, correlated control Stop/reap, control cancellation, and clean repeat without residue. The provider carries no entitlement. |
| Dropped owner | A separate exact-root process starts the real Rust `SystemVmnetInterfaceBackend`, validates bounded realized parameters, clears supplementary groups, irreversibly changes real/effective uid and gid, proves it cannot regain root, then completes callback enable, fixed 60-byte experimental-frame write, bounded read, stop, and residue checks. |
| Direct guest | The public Unix HTTP API configures the exact kernel, read-only v111 rootfs, read-only schema-v2 control sector, serial sink, and one `vmnet:shared` interface. A real HVF guest validates DHCP offer/request/ack, derives the host endpoint only from the accepted DHCP router, and completes an exact nonce-bound request and response. |
| Repeat and cleanup | The guest gate succeeds twice in separate VMM processes. Each process stops normally, and every harness-owned API socket, process group, temporary file, listener, and interface owner is gone. |

The successful fixed output is:

```text
platform: macos=<version> sdk=<version> arch=arm64 hvf=supported root=exact apple-vmnet=absent
bangbang elevated vmnet proof: denial=passed provider=passed provider-cancel=passed provider-repeat=passed dropped-owner=passed guest=passed repeat=passed cleanup=passed
```

Tracked evidence and normal output never contain artifact paths, account names,
uid/gid values, PIDs, interface names, MAC addresses, IP addresses, routes,
ports, nonces, packets, framework output, or raw process errors. Guest serial
records are fixed phase/result markers only. Process observation is restricted
to exact harness-owned children and process groups and never reads arguments.

## Guest oracle boundary

`direct-boot-v111` installs one static, stripped, interpreter-free aarch64 Linux
ELF built from `scripts/guest/elevated_vmnet_certification.rs`. A no-std syscall
entry point is required because the direct init environment cannot reliably
start the distribution Python or static-std runtime before normal userspace
initialization.

The oracle accepts exactly one 512-byte schema-v2 control sector containing a
mode, bounded port, nonzero nonce, reserved zero bytes, and SHA-256 digest. It
discovers exactly one canonical non-loopback Linux interface, binds DHCP to
that interface, accepts only a strict coherent lease, applies the address and
default route with bounded child commands, completes one exact TCP exchange,
and reverses route, address, and link state. All socket, command, DHCP, TCP, and
interface waits are bounded. Serial writes are split into at most 16 bytes to
match the emulated UART boundary.

The v111 oracle does not accept a host address from the control sector. The TCP
address is the router from the accepted DHCP lease, so the host cannot substitute
an unrelated endpoint after lease validation.

## Inventory handoff and nonclaims

The checked `vmnet-feasibility-audit.json` pins the source identity, two public
Apple references, authorization boundary, three evidence profiles, two exact
disposition transitions, unrelated-inventory digest, and nonclaims. Default
delivery validation requires that authority and the exact `383/0/2/33`
partition. Global `validate --final` must still fail, and it must identify only
the same two `missing-platform-feasible` records.

The later [`provider-v1` contract](../../../docs/vmnet-provider-protocol.md)
freezes the bounded wire, role state, and descriptor ownership for the split
topology. #1934 adds the minimal broker/owner process foundation and extends
this exact-host workflow with its real provider data/cancellation/repeat proof.
That successor changes neither disposition. #1936 then adds the credential-free
contained grant, remote-only route, client pumps, and process-registry adapter;
elevated launcher/provider assembly, a real guest through the remote provider,
and product certification remain separate work.

#1930 does not claim any of the following:

- a root direct VMM as production;
- an Apple-authorized vmnet path;
- the privileged provider protocol, broker, or per-interface executable;
- the sandbox-worker remote provider;
- production service/crash reclamation or concurrent-session certification;
- implementation of either capability or completion of #1378, #1375, #1351,
  or #1348.

These are the historical nonclaims of the #1930 inventory transition. #1934
subsequently implements the privileged provider protocol/broker foundation, but
does not retroactively turn #1930 into production evidence. #1936 subsequently
implements the sandbox-worker remote provider adapter, but likewise supplies no
elevated product assembly or positive connectivity evidence.
