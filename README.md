# bangbang

bangbang is a Rust VMM project for macOS hosts. It aims to keep the public
control plane compatible with the Firecracker HTTP API over a Unix domain
socket, while the VM backend is built on Apple's Hypervisor.framework.

The repository is still a scaffold. Use the documentation below as the source of
truth for detailed capability status, compatibility limits, security boundaries,
and test rules:

- [Firecracker Compatibility Scope](docs/firecracker-compatibility.md)
- [Firecracker Validation Matrix](docs/firecracker-validation-matrix.md)
- [Firecracker v1.16.0 Capability Inventory](compat/firecracker/v1.16.0/README.md)
- [Snapshot Feasibility](docs/snapshot-feasibility.md)
- [macOS Host Security Model](docs/security.md)
- [Testing Guide](docs/testing.md)
- [Pull Request Review Guidelines](docs/review-guidelines.md)

The reconciled Firecracker v1.16.0 remaining-device subset covers
virtio-balloon reporting and zero-safe best-effort Darwin discard, bounded
virtio-rng, targeted and rate-limited virtio-pmem flush, a block-granular
virtio-mem plug/unplug lifecycle, the no-interrupt aarch64 PL031 RTC,
DeviceTree VMGenID including native-v1 replacement notification, and startup
VMClock discovery. Optional PCI runtime attach/delete, ARM PVTime, pmem root or
direct file-backed mapping, optional-device snapshots, and mutable VMClock
restore remain explicit limits. Host discard never promises synchronous RSS or
footprint reduction. See the
[pinned remaining-device audit](docs/firecracker-compatibility.md#firecracker-v1160-remaining-device-audit)
for exact upstream sources and classifications.

## Layout

```text
crates/api        Firecracker-compatible API request and response surface
crates/runtime    Backend-neutral VM model, memory, MMIO, boot, and device helpers
crates/hvf        Hypervisor.framework backend and signed integration tests
crates/bangbang   VMM process entrypoint and startup CLI
crates/launcher   Production app bundle, nested-worker validation, and supervision
crates/session    Private launcher-worker protocol and runtime namespace ownership
tools/firecracker-capability-audit
                  Checked Firecracker source/capability inventory validator
```

On supported macOS Apple Silicon hosts, the public machine configuration accepts
`vcpu_count` from 1 through 32 and HVF startup admits the host-limited subset
`1..=min(32, host_max)`. Counts above the runtime host maximum fail before a
session is retained or the instance becomes `Running`. Public pause/resume uses
a topology-wide active-run barrier for every online vCPU. Guest PSCI `CPU_OFF`
and later `CPU_ON` re-entry reuse the fixed owner topology. PSCI
`CPU_SUSPEND32/64` provides KVM-style retained standby for an enabled,
guest-unmasked EL1 virtual timer: affinity remains `ON`, all three call
arguments are ignored, the timer PPI is made pending before `SUCCESS`, and
lifecycle cancellation rearms the same transaction without fabricating a
wake. Runtime discovery reports PSCI 1.0 and a minimal safe SMCCC 1.1 surface:
`PSCI_FEATURES` advertises only delivered calls, `SMCCC_ARCH_FEATURES` reports
only its mandatory VERSION/self results, and optional firmware services remain
unsupported. The FDT deliberately keeps Firecracker v1.15.1's
`arm,psci-0.2`/HVC binding. FDT idle-state discovery and SGI/SPI/direct IRQ/FIQ
wake are not exposed. Dynamic CPU topology, SMT, non-`None` CPU templates, and
cross-host CPU portability remain unsupported. The native-v1 snapshot profile
below remains restricted to exactly one vCPU.

Firecracker-shaped `PUT /cpu-config` input is fully syntax-validated. Empty
custom templates remain successful no-ops; non-empty KVM capability,
KVM vCPU-init feature, arm register modifier, and mixed requests are reduced to
category-only runtime actions and rejected with stable value-redacted arm64 HVF
faults. Deprecated non-`None` machine `cpu_template` names are likewise rejected
as Firecracker AWS/Linux CPU policies rather than HVF profiles. bangbang does
not retain or apply their raw masks, and accepted empty/`None` input adds no CPU
section to `GET /vm/config`.

The HVF runner currently exposes owner-thread capture building blocks for
general registers, plus ordered nontransactional restore of the same typed
X0-X30/PC/CPSR value, raw core system registers plus ordered nontransactional
restore of their typed SP_EL0/SP_EL1/ELR_EL1/SPSR_EL1 value, raw EL1 exception
registers plus ordered nontransactional restore of their typed
AFSR0/AFSR1/ESR/FAR/PAR/VBAR value, raw EL1 execution controls plus ordered
nontransactional restore of their typed ACTLR/CPACR value, raw thread-context
registers plus ordered nontransactional restore of their typed
TPIDR_EL0/TPIDRRO_EL0/TPIDR_EL1 value, raw EL1 translation registers plus
ordered nontransactional restore of their typed
SCTLR/TTBR0/TTBR1/TCR/MAIR/AMAIR/CONTEXTIDR value, baseline SIMD/FP registers
plus ordered nontransactional restore of their typed Q0-Q31/FPCR/FPSR value,
baseline and optional SVE/SME guest-visible processor identification metadata,
mutable SME PSTATE flags, raw SME system registers with redacted `Debug`,
conditional maximum-width streaming Z0-Z31 contents with
redacted `Debug`, conditional maximum-derived streaming P0-P15 predicates with
redacted `Debug`, conditional maximum-SVL-square ZA contents with redacted
`Debug`, conditional fixed-size SME2 ZT0 contents with redacted `Debug`, raw
system-context registers with redacted `Debug` plus ordered nontransactional
restore of their typed SCXTNUM_EL0/SCXTNUM_EL1 value, raw cache-selection plus
ordered nontransactional restore of its typed CSSELR_EL1 value,
hardware-breakpoint,
hardware-watchpoint, debug-control plus ordered nontransactional restore of its
typed MDCCINT_EL1/MDSCR_EL1 value, raw Hypervisor.framework debug-trap policy
plus ordered nontransactional restore of its complete two-Boolean value,
pointer-authentication key state with redacted `Debug` plus ordered
nontransactional restore of the complete APIA/APIB/APDA/APDB/APGA value, raw
physical and virtual timer state plus a separate debug-redacted normalized
timer value with ordered never-run restore, CPU-level IRQ/FIQ pending injection
levels plus ordered nontransactional restore of their complete typed value,
opaque GIC device state plus runner-owned pre-first-run reapply, and raw EL1
GIC ICC CPU-interface registers plus ordered pre-first-run restore of their nine
mutable values with derived RPR validation.
A native-v1 optional-state classifier fails closed for active SVE/SME and
enabled hardware breakpoint/watchpoint state. Prepared boot sessions can also
replace the 16-byte VMGenID buffer and retained metadata before first run, then
inject its edge-rising SPI after replacement. A separate no-handle query
exposes the maximum SME streaming vector length used for the Z-, P-, and
ZA-register allocations.

These primitives back a deliberately narrow public native-v1 snapshot path on
macOS Apple Silicon. `PUT /snapshot/create` supports only `Full` snapshots from
a paused VM with one vCPU, exactly one regular read-only root drive, default
serial, and no optional devices or MMDS. It writes a bounded kind-2
`BANGCMT\0` pair whose state file binds the complete memory image to an exact
five-component `BANGHVF\0` payload and nested `BANGDEV\0` device profile.

Create preflights both final namespaces, streams the paused aggregate capture
directly into an owner-only staging inode, and publishes memory durable first
and state last as the commit marker without replacing existing entries. A
successful request returns `204 No Content` and leaves the source paused and
usable. Failures clean private staging where safe; a late failure can leave a
typed memory-only orphan, and a state-directory sync failure after publication
is treated as committed but durability-uncertain.

`PUT /snapshot/load` accepts the matching committed pair only in a pristine
fresh process, except that logger and metrics configuration are allowed. It
supports a `File` memory backend (or the deprecated sole `mem_file_path` alias),
constructs a fresh HVF VM/GIC/vCPU, restores the exact local native state,
replaces and signals VMGenID, and first commits the session as `Paused`.
`resume_vm: true` then uses the ordinary resume path; otherwise resume later
with `PATCH /vm`. The external root backing must still match the captured
regular-file identity. Snapshot files and guest state are untrusted and
confidential, so keep artifacts and the API socket in operator-owned private
directories.

In the production bundle, contained describe/load inputs use exact read-only
file grants and create outputs use retained `SnapshotOutputDirectory` anchors
plus bounded UTF-8 child names. Load atomically adopts state, memory, and any
grant-tagged persisted root backing after bounded state preinspection; no tag is
reopened as a pathname. Create preserves the same anchor-relative no-clobber
transaction and repeated output-directory authority. Direct mode keeps ordinary
path behavior.

This is not Firecracker snapshot-file compatibility or a portable migration
format. `Diff`, UFFD, dirty tracking, clock adjustment, restore overrides,
writable or additional drives, optional devices, active SVE/SME/debug state,
EL2 GIC CPU-interface state, and cross-host portability remain unsupported.

## Process CLI

Run the VMM process skeleton and API server:

```sh
cargo run -p bangbang -- --api-sock /tmp/bangbang.socket --id demo-1
```

Supported value-taking options accept either `--name value` or `--name=value`.
Value-less flags, such as `--no-api`, do not accept an attached value.

- `--api-sock <PATH>` sets the Unix socket path. The default is
  `/tmp/bangbang.socket`.
- `--boot-timer` enables Firecracker-compatible guest boot-time logging. During
  startup, bangbang registers a pseudo-MMIO boot timer at Firecracker's aarch64
  boot timer address; a guest write of byte value `123` at offset `0` logs the
  elapsed wall and process CPU time when logger output is configured.
- `--config-file <PATH>` reads a Firecracker-shaped JSON configuration for the
  supported startup subset from a readable regular file up to 1 MiB, starts the
  VM, then serves the API socket unless `--no-api` is set.
- `--http-api-max-payload-size <BYTES>` sets the maximum accepted HTTP API
  request body size declared by `Content-Length`. The default is `51200` bytes;
  request-head bytes are bounded separately by the parser.
- `--id <ID>` records the microVM identifier. The default is
  `anonymous-instance`.
- `--start-time-us <MICROS>`, `--start-time-cpu-us <MICROS>`, and
  `--parent-cpu-time-us <MICROS>` accept Firecracker launcher timing values for
  session-initial, explicit `FlushMetrics`, 60-second Running/Paused periodic,
  and normal-terminal metrics output.
- `--metrics-path <PATH>` configures the same per-process metrics sink as
  `PUT /metrics` before the API socket is served.
- `--mmds-size-limit <BYTES>` sets the maximum serialized MMDS data-store size.
  When omitted, it inherits the HTTP API request-size limit, which defaults to
  `51200` bytes.
- `--log-path <PATH>`, `--level <LEVEL>`, `--module <MODULE>`,
  `--show-level`, and `--show-log-origin` configure the same per-process
  logger state as `PUT /logger` before the API socket is served. Implemented
  logger events use module paths `bangbang_runtime::api_server`,
  `bangbang_runtime::vmm_action`, and `bangbang_runtime::boot_timer`.
- `--no-api` requires `--config-file <PATH>`, starts from that configuration
  without publishing an API socket, and exits cleanly on `SIGINT` or `SIGTERM`.
- `--snapshot-version` prints the supported bangbang-native snapshot envelope
  version (`v1.0.0`) and exits before startup.
- `--describe-snapshot <PATH>` reads a bounded regular native state file,
  validates its complete envelope and CRC, prints its embedded version, and
  exits before startup. In contained mode an exact read-only
  `SnapshotDescribeInput` grant is inspected without reopening its tag. It does
  not accept Firecracker state files.
- `--help`, `-h`, `--version`, and `-V` are supported.

The API socket is an unauthenticated local control interface. bangbang restricts
the published socket inode to owner-only permissions; the parent directory is
still part of the access-control boundary, so use a private directory on
multi-user hosts.

Start with metrics and logger output configured:

```sh
cargo run -p bangbang -- \
  --api-sock /tmp/bangbang.socket \
  --id demo-1 \
  --metrics-path /tmp/bangbang.metrics \
  --log-path /tmp/bangbang.log \
  --level Info \
  --show-level
```

Start from a configuration file while keeping the API socket enabled:

```sh
cargo run -p bangbang -- \
  --api-sock /tmp/bangbang.socket \
  --config-file /tmp/bangbang-vm.json
```

Start from a configuration file without publishing an API socket:

```sh
cargo run -p bangbang -- \
  --config-file /tmp/bangbang-vm.json \
  --no-api
```

## Production macOS Bundle

The direct `cargo run -p bangbang` path above is intentionally uncontained: it
runs the VMM as the invoking user and relies on host filesystem permissions and
per-resource validation. The production entry point instead has a fixed
two-process topology:

```text
Bangbang.app                          dev.bangbang
├── Contents/MacOS/bangbang           unsandboxed launcher
└── Contents/Helpers/BangbangWorker.app
    └── Contents/MacOS/bangbang-worker  App Sandbox + Hypervisor worker
```

Build and exclusively publish it to an absent destination named
`Bangbang.app`:

```sh
scripts/build-production-bundle.sh --output /private/operator/Bangbang.app
```

Ad-hoc signing (`-`) is the local-validation default. A distribution build can
supply one identity for both separately signed code objects:

```sh
scripts/build-production-bundle.sh \
  --output /private/operator/Bangbang.app \
  --signing-identity "Developer ID Application: Example (TEAMID)"
```

The worker is signed first with exactly App Sandbox and Hypervisor
entitlements; the outer launcher is signed last without either entitlement.
Both use Hardened Runtime. Before every launch, the outer executable validates
the fixed bundle layout, nested signatures, identifiers, and required worker
entitlements. It then starts the fixed worker suspended with a default-close
descriptor policy: only open standard streams, one private lifecycle endpoint,
one private startup-grant endpoint, and one dormant private socket-broker
endpoint survive. The launcher validates the live worker code before resuming it and
again after the worker has used the endpoint and sent the bounded pre-session
greeting.

Each launch uses unnamed lifecycle stream, grant datagram, and socket-broker
socketpairs plus a random 256-bit session identity. Lifecycle protocol v2 has a 4-KiB frame limit,
exact per-direction sequence numbers, closed message variants, and monotonic
`prepared -> grants-accepted -> starting -> ready -> terminal` state. Even an
empty grant batch must be atomically acknowledged before `Proceed`. The launcher authenticates
the live worker PID, effective credentials, signature, identity, and exact
entitlements. The sandboxed worker verifies that the peer PID is its direct
parent and that effective credentials match; App Sandbox prevents the worker
from independently querying the launcher's code signature, so authentication is
deliberately asymmetric.

Before public argument or VM processing, the worker creates and locks a unique
mode-0700 empty namespace in its App Sandbox container. The launcher derives
that path independently and checks its exact name, owner, mode, device, inode,
emptiness, and live lock before authorizing startup. Graceful signals become one
session cancellation, readiness is reported only at the existing committed API
or no-API seams, and structured terminal status must match the reaped public
exit. Initial `Hello`, `Start`, and `Proceed` reads use absolute five-second
deadlines; cancellation and post-`Terminal`/EOF process-exit waits use a
five-second grace before owned-worker escalation. A surviving worker cleans
after launcher EOF; a surviving launcher cleans after worker exit; a later
worker performs bounded identity-checked recovery when both were killed.
Concurrent sessions retain independent identities, processes, namespaces,
grant registries, and API sockets.

The launcher recognizes one optional envelope only in argv position one:

```text
--bangbang-grant-manifest MANIFEST -- FIRECRACKER_ARGS...
```

Manifest v1 is bounded strict JSON with `version: 1` and a `grants` array. Each
grant has a 64-byte ASCII `id`, one closed `role`, exact `access`, and an
absolute UTF-8 `source` path. The launcher walks resource paths component by
component without following symlinks or accepting `.`/`..`, opens every
existing resource before spawn, rejects aliases and type/access conflicts, and
prepares the complete batch atomically. Regular-file roles transfer only an
identity-checked descriptor. The three create-children directory roles combine
an anchor descriptor with a bounded one-session implicit bookmark whose
resolved inode and active scope are revalidated in the worker.

The initial roles are startup config/metadata, kernel/initrd, repeatable
drive/pmem backing, logger/metrics/serial sinks, snapshot describe/state/memory
inputs, and API/vsock/snapshot-output directories. The exact access matrix and
hard limits are part of the closed protocol; unknown roles and operator-supplied
bookmark bytes are rejected. Grant delivery uses 1024-byte datagrams, bounded
bookmark fragmentation, SCM_RIGHTS, one five-second absolute deadline, and a
session-owned one-time typed registry. Closing the launcher's duplicate does
not revoke an already delivered descriptor; cleanup is cooperative ownership.

Production consumers now adopt read-only startup config, startup metadata,
kernel, initrd, snapshot describe/state/memory, and persisted snapshot-root
grants plus repeatable read-only/read-write block and pmem backing grants,
singleton write-only logger/metrics/serial sink grants, and repeatable snapshot
output-directory grants.
In authenticated contained mode the exact
case-sensitive private reference `bangbang-grant:<GrantId>` claims one matching
ID/role/access entry; malformed, missing, mismatched, or consumed claims fail
without pathname or singleton fallback. Direct mode treats the same bytes as an
ordinary pathname. Config and metadata read the transferred descriptor, while
explicit kernel/initrd references are claimed atomically when boot-source
configuration is applied, retained across API readiness, and consumed once by
boot loading without reopening the reference. Mixed boot sources claim only
their referenced members and leave ordinary members on deferred pathname
opening. Submitted boot references remain visible through the owner-authorized
VM configuration response but never appear in diagnostics. A
descriptor-consuming boot failure requires a fresh contained launch for
grant-backed retry because those roles are singleton.

Block and pmem `PUT` claims validate complete device state before consuming the
exact grant, retain the opened backing by device ID, and move it into startup
without reopening the tag. Access must match `is_read_only`/`read_only`.
Same-ID pre-boot `PUT` replaces the retained authority atomically; ordinary
paths preserve deferred opening. A path-changing live block `PATCH` may consume
one still-unused startup-batch drive grant and swaps the opened backing before
public configuration commits. Path-free block limiter and pmem limiter updates
retain the active backing. A grant consumed by startup or a live block swap is
one-time even if a later consumer step fails; retry requires a fresh same-ID
configuration with unused authority. Authorized configuration responses may
return submitted tags, while logs, faults, errors, and derived debug output stay
value-redacted.

Logger and metrics validate before claiming, normalize the transferred regular
file to append/nonblocking behavior without upgrading its kernel-enforced
write-only access, and retain the opened sink. A logger update without
`log_path` retains that sink and consumes no grant; metrics remains one-time
initialized. Serial retains a prepared output until startup, moves it into the
VM without reopening the reference, and requires successful reconfiguration
after a startup attempt consumes it. Clearing or replacing serial before start
drops the prepared output. Direct paths retain their existing create, FIFO-like,
and open-timing behavior.

Snapshot file inputs use the same exact `bangbang-grant:<GrantId>` grammar with
distinct read-only roles. Describe inspects a duplicate of its exact descriptor.
Load preinspects state without consuming it, discovers any persisted root grant,
then atomically takes all tagged state, memory, and read-only root backings and
finishes from those opened identities. Input authority is one-time after that
take. The persisted root identity includes file metadata such as `ctime`, so a
later rename or metadata-changing replacement is correctly rejected even when
it refers to the same inode.

Create outputs instead use
`bangbang-grant:<GrantId>/<SnapshotOutputChild>`. The child is one 1–255 byte
UTF-8 component, contains no NUL or `/`, and is neither `.` nor `..`. One
retained output grant can serve distinct state/memory children and later create
requests; distinct or mixed ordinary/granted directories are also supported.
Staging and exclusive final publication stay relative to the exact retained
anchors. App Sandbox authorization still requires the granted directory to
remain reachable at its authorized pathname; moving it after scope activation
can make descriptor-relative writes fail.

Each active granted staging inode gets one strict private identity record.
Normal publication or conclusive cleanup clears it; after worker death the
launcher removes only an exact current-user regular `0600`, single-link match
through its retained directory anchor and preserves a replacement. A hard death
between staging creation and record persistence, or simultaneous uncatchable
launcher/worker death, can still leave residue because Darwin has no
identity-conditional unlink primitive.

API and vsock directory consumers instead require the exact case-sensitive
reference `bangbang-grant:<GrantId>/<SocketChild>`. `SocketChild` is one 1–64
byte ASCII `[A-Za-z0-9._-]` component other than `.` or `..`; direct mode still
treats identical bytes as an ordinary path. The owner thread claims the exact
singleton directory role, retains its scope and anchor, and runs a short-lived
default-close instance of the signed worker that binds one fixed private
staging name. The worker receives the listener descriptor, records only its
role, safe child, and socket identity in its private namespace, and publishes
the socket exclusively to the requested child with fd-relative
`renameatx_np(RENAME_EXCL)`. Publication requires the namespace and granted
directory to share a filesystem. The binder is reaped before API readiness or
VM-start success; shutdown removes only an identity-matching socket. A
simultaneous uncatchable launcher and worker death can leave a stale external
socket name plus its private ownership record; automatic later recovery remains
limited to empty session namespaces.

The granted API listener is served directly and becomes ready only after
publication. `--no-api` claims no API directory. A granted vsock keeps the
published main listener plus directory authority through its VM lifetime.
Host-initiated traffic uses that supplied listener. Guest-initiated connections
activate the otherwise dormant per-session launcher broker once, then send only
monotonic `u32` host ports. The launcher is fixed to the retained vsock anchor
and safe child, connects only to relative `<SocketChild>_<port>` targets after
identity checks, and returns one validated connected stream descriptor. It
receives no guest payload, grant ID, path, bookmark, or general resource
selector. API-only, no-API, and direct-path sessions leave the broker dormant;
the worker still has exactly App Sandbox and Hypervisor entitlements and steady
state remains one launcher plus one worker.

General dynamic post-Ready brokerage, hard revocation, cross-filesystem socket
publication, vmnet provisioning, broader snapshot profiles, automatic restart
policy, Developer ID possession proof, launch-constraint policy, and
notarization workflow remain. The session namespace must be empty at the
`Prepared` gate. Authorized construction may transiently add one fixed
role-specific staging socket or one strict record per active snapshot artifact;
steady state retains no snapshot staging record and at most the two fixed socket
ownership records. Records never expose a path, descriptor, bookmark, grant ID,
payload, or session byte.
Same-identifier workers share one App Sandbox
container, so namespace locks and identity checks protect cooperative sessions
and replacements but do not isolate a malicious same-bundle sibling. See
[macOS Host Security Model](docs/security.md) for the precise trust boundary.

## API Examples

Query the instance info endpoint:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/
```

Example response:

```json
{"app_name":"bangbang","id":"demo-1","state":"Not started","vmm_version":"0.1.0"}
```

Query the accumulated VM configuration:

```sh
curl --unix-socket /tmp/bangbang.socket http://localhost/vm/config
```

Record a pre-boot boot source:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/boot-source \
  -H 'Content-Type: application/json' \
  -d '{"kernel_image_path":"/tmp/vmlinux","boot_args":"console=ttyS0 reboot=k panic=1"}'
```

Record a pre-boot drive:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/drives/rootfs \
  -H 'Content-Type: application/json' \
  -d '{"drive_id":"rootfs","path_on_host":"/tmp/rootfs.ext4","is_root_device":true,"is_read_only":true}'
```

Create a supported full native-v1 snapshot after the VM is paused:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PATCH http://localhost/vm \
  -H 'Content-Type: application/json' \
  -d '{"state":"Paused"}'

curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/snapshot/create \
  -H 'Content-Type: application/json' \
  -d '{"snapshot_type":"Full","snapshot_path":"/private/snapshot.state","mem_file_path":"/private/snapshot.memory"}'
```

Load that pair into a fresh `bangbang` process and leave it paused:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/snapshot/load \
  -H 'Content-Type: application/json' \
  -d '{"snapshot_path":"/private/snapshot.state","mem_backend":{"backend_path":"/private/snapshot.memory","backend_type":"File"},"resume_vm":false}'

curl --unix-socket /tmp/bangbang.socket \
  -X PATCH http://localhost/vm \
  -H 'Content-Type: application/json' \
  -d '{"state":"Resumed"}'
```

The destination must be pristine apart from optional logger/metrics setup, and
the captured read-only root backing must still satisfy the recorded identity.

Record a pre-boot network interface:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/network-interfaces/eth0 \
  -H 'Content-Type: application/json' \
  -d '{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"12:34:56:78:9a:bc","mtu":1500}'
```

After the VM starts, update individual RX/TX limiter buckets without resetting
omitted buckets:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PATCH http://localhost/network-interfaces/eth0 \
  -H 'Content-Type: application/json' \
  -d '{"iface_id":"eth0","rx_rate_limiter":{"bandwidth":{"size":1048576,"refill_time":100}}}'
```

Set a bucket's `size` or `refill_time` to `0` to disable only that bucket.

The configured `mtu` is advertised to the guest virtio-net device. Current
signed Network/MMDS scenarios select every configured interface in MMDS config,
so startup uses process-local MMDS-only packet I/O without opening vmnet; they
do not prove direct vmnet or external packet movement. Non-MMDS-only startup
conditionally uses the internal direct-vmnet foundation, which requires
Apple's restricted networking authorization plus operator-owned firewall,
routing/NAT, resource, and distribution policy. See the
[compatibility scope](docs/firecracker-compatibility.md#internal-network-interface-configuration),
[vmnet security boundary](docs/security.md#vmnet-host-policy-boundary), and
[testing guide](docs/testing.md) for the exact supported subset and exclusions.

Record a pre-boot vsock configuration:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/vsock \
  -H 'Content-Type: application/json' \
  -d '{"guest_cid":3,"uds_path":"./v.sock"}'
```

Virtio-vsock is an **implemented supported live virtio-MMIO/Unix-socket subset**.
Repeated valid pre-boot `PUT /vsock` requests replace the stored
configuration; post-start PUT is rejected without mutation, and there is no
PATCH, DELETE, runtime hotplug, or broader CID-routing contract. The live path
uses dynamic 64-KiB credit windows with wrapping counters, two-second
request/shutdown cleanup, up to 256 connections per direction, `EVENT_IDX`, and
process-local listener ownership with path/payload-redacted transport
diagnostics. Signed Apple Silicon tests verify at least 1 MiB in each direction
for both initiation paths plus two-stream isolation. Indirect descriptors are a
supported bangbang extension. Native-v1 snapshot UDS override, event-queue
`TRANSPORT_RESET`, and post-restore RX gating remain explicit exclusions; this
does not claim general performance, Firecracker artifact, or snapshot parity.

Configure metrics output before boot:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/metrics \
  -H 'Content-Type: application/json' \
  -d '{"metrics_path":"/tmp/bangbang.metrics"}'
```

Configuring the sink does not write before a VM session exists. The first
retained session causes one best-effort initial JSON line, regardless of
whether CLI, config-file, or API configuration supplied the sink. The same
process writes every 60 seconds in both `Running` and `Paused`, supports the
explicit runtime `FlushMetrics` action, and makes one best-effort
normal-terminal attempt while it still owns live diagnostics. Initial,
periodic, and terminal sink failures never replace the action, loop, or process
result; explicit `FlushMetrics` remains runtime-only and returns a configured
sink failure to its caller. Lines can include a `boot_run_loop_status` store
such as `running`, `paused`, `exited`, or `failed`. When startup timing CLI values are provided,
the same metrics output includes Firecracker-style
`api_server.process_startup_time_us` and
`api_server.process_startup_time_cpu_us` elapsed values. `--start-time-us` is
subtracted from the sampled monotonic clock, `--start-time-cpu-us` is
subtracted from the sampled process CPU clock, and `--parent-cpu-time-us`
contributes to the CPU value without being serialized as a separate field. If a
provided start timestamp is later than the sampled clock value, the elapsed
component saturates at zero. The current
Firecracker-shaped API request metrics subset also reports selected GET counters
under `get_api_requests`; parsed core
configuration, MMDS, observability, memory hotplug, pmem, and `/actions`
counters under `put_api_requests`; parser failures, including malformed bodies
and path/body ID mismatches, for those PUT endpoints with matching
Firecracker-style fields in the matching
`put_api_requests` count/fail counters; and selected PATCH counters including
memory hotplug and pmem under `patch_api_requests`, including parser failures
for those PATCH endpoints. bangbang also records
bangbang-specific `balloon_count` API request counters for parsed balloon GET,
PUT, and PATCH routes, plus `balloon_fails` counters for parsed balloon PUT and
PATCH failures and identifiable malformed balloon PUT/PATCH parser failures,
because Firecracker does not expose matching balloon API request metric fields.
Runtime metrics flushes can also include a top-level aggregate `block` object
and non-empty per-drive `block_{drive_id}` objects for implemented virtio-block
queue activity, read/write latency aggregates, backing update counters, and
failures; a top-level aggregate `pmem` object and non-empty per-device
`pmem_{id}` objects for implemented virtio-pmem queue activity and failures;
top-level aggregate `net` and non-empty per-interface
`net_{iface_id}` objects for implemented virtio-net RX/TX queue activity,
packet counts, byte counts, and failures; a top-level `mmds` object for
implemented guest MMDS packet detour and response queue activity; a top-level
`vsock` object for implemented virtio-vsock RX/TX queue activity, packet
counts, byte counts, connection cleanup counters, and classifiable queue/event
failures; a top-level `entropy` object with Firecracker-shaped counters for
implemented virtio-rng request, byte, host-randomness failure, and event-failure
activity; a
top-level `uart` object with Firecracker-shaped serial counters for implemented
TX writes, missed writes, output errors, and rate-limiter drops; a top-level
`signals` object with `sigpipe` counts for handled non-terminating `SIGPIPE`;
plus a top-level `balloon` object for implemented virtio-balloon activity and
failures. Balloon metrics distinguish inflate, free-page-hint, and free-page-
report discard attempts, bytes whose Darwin host-page interiors completed
zero/free advice, partial-edge bytes skipped to protect neighboring guest data,
and failed attempts. Reporting also exposes its requested byte total separately
from advised bytes, so accepted guest descriptors never imply that the host
reclaimed the complete range. Darwin discard is best effort and does not promise
a synchronous process-footprint reduction.

All implemented API, logger, signal, UART, and device counts, byte totals,
failures, errors, limiter activity, and block-latency `sum_us` are interval
increments. Startup timing, boot status, the latest lifecycle/snapshot action
latencies, and block-latency `min_us`, `max_us`, and `sample_count` are stores.
The typed baseline advances only after a complete successful write. A new or
lower producer generation emits its full current value; new, disappeared, and
reappearing keyed devices follow the same rule. Empty device families stay
sparse rather than appearing as fake all-zero Firecracker objects. An ambiguous
write error retains the old baseline, so a later success replays the interval
at least once. Every successfully completed line includes bangbang's extension
`vmm.metrics_flush_count: 1`.

Parsed deprecated HTTP API
usage is counted under `deprecated_api.deprecated_http_api_calls` for supported
deprecated machine `cpu_template`, MMDS V1 config, `vsock_id`, and snapshot-load
field forms.
After a metrics write failure, later successful output includes
`logger.missed_metrics_count`; failed API request/action/boot-timer logger
delivery appears in `logger.missed_log_count`; and denied boot-timer records
appear in `logger.rate_limited_log_count`. These are interval counters under the
same successful-baseline rule.

Configure logger output before boot:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/logger \
  -H 'Content-Type: application/json' \
  -d '{"log_path":"/tmp/bangbang.log","level":"Info","module":"bangbang_runtime","show_level":true,"show_log_origin":true}'
```

No logger sink is configured by default. A configured nonblocking file/FIFO
sink records successfully parsed API request method/path lines without request
bodies, plus successful `InstanceStart` and explicit `FlushMetrics` action
events. These host records are unrestricted by the guest limiter. `show_level` adds `level=Info`, and
`show_log_origin` adds the callsite as `origin=<file>:<line>`.
`module` filters these logger events by prefix against
`bangbang_runtime::api_server`, `bangbang_runtime::vmm_action`, or
`bangbang_runtime::boot_timer`.

When `--boot-timer` is enabled, its guest-triggered callsite admits an initial
burst of ten records, refills at one record per 500 ms across a five-second
budget, counts every denied record, and emits one unrestricted warning before
the next admitted boot-time record. Filtered or unconfigured records consume no
budget. Sink contention, poisoning, write, or flush failure is best effort:
`missed_log_count` changes, but the API, action, startup, or guest MMIO result
does not. Bangbang does not claim process-global panic/fatal durability,
rotation, syslog, journald, tracing, or remote telemetry.

Serial output is independently configured before boot with `PUT /serial`.
Omitting or clearing `serial_out_path` keeps TX in a bounded 64-KiB internal
buffer instead of stdout; a configured file/FIFO is opened nonblocking with
path-redacted errors. An optional token bucket drops exhausted bytes without
sleeping or failing the guest write and reports the drop count in `uart`
metrics. There is no public serial RX, stdin route, or streaming API. The
bangbang-native v1 profile captures default serial MMIO metadata/registers but
restores a fresh output buffer and does not capture a public path, buffered or
in-flight bytes, limiter state, or UART counters.

The exact field classes, failure semantics, and native-v1 boundary are in
[Firecracker Compatibility Scope](docs/firecracker-compatibility.md#firecracker-v1160-observability-contract).

Submit an `InstanceStart` action:

```sh
curl --unix-socket /tmp/bangbang.socket \
  -X PUT http://localhost/actions \
  -H 'Content-Type: application/json' \
  -d '{"action_type":"InstanceStart"}'
```

See [Firecracker Compatibility Scope](docs/firecracker-compatibility.md) for
the full endpoint matrix, implemented behavior, and deferred Firecracker
features. See [Firecracker Validation Matrix](docs/firecracker-validation-matrix.md)
for the support status and validation layer summary. The
[v1.16.0 capability inventory](compat/firecracker/v1.16.0/README.md) is the
mechanically checked scope authority for exhaustive compatibility work; its
initial `audit-required` entries do not make new runtime claims.

## Build And Test

Requires the latest stable Rust toolchain.

```sh
cargo fmt --all -- --check
cargo run -p bangbang-firecracker-capability-audit --locked -- validate
cargo check --workspace --all-targets --all-features --locked
cargo check -p bangbang-launcher --all-targets --all-features --locked --target aarch64-unknown-linux-musl
cargo test --workspace --all-targets --all-features --locked --exclude bangbang-hvf
cargo test -p bangbang-hvf --lib --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy -p bangbang-launcher --test production_bundle_e2e --all-features --locked --target aarch64-apple-darwin -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
```

Run signed HVF integration tests on macOS Apple Silicon:

```sh
scripts/run-integration-tests.sh
```

Run the integration-only App Sandbox boundary on its own:

```sh
scripts/run-integration-tests.sh --test app_sandbox
```

This target packages real test binaries as minimal app bundles, runs the full
HVF lifecycle suite with App Sandbox plus Hypervisor entitlements, and checks
that the real executable accepts an app-container API socket while rejecting
the default `/tmp` socket and outside configuration paths. It validates an
Apple containment building block, not a production sandboxed distribution.

Build and run the separately signed production launcher/worker boundary on its
own:

```sh
scripts/run-integration-tests.sh --test production_bundle
```

This target verifies exact identifiers, entitlements, Hardened Runtime, strict
static and live-worker validation, tamper rejection, the descriptor allowlist,
malformed-bootstrap rejection, container-only path denial and redaction,
structured API/no-API readiness and cancellation, worker-first/launcher-first
namespace cleanup, empty both-killed namespace recovery, concurrent-session
isolation, owned-socket
cleanup, mandatory empty-grant startup, typed read-only/write-only/directory
grants, mismatch rollback, grant-phase cancellation/deadline behavior,
grant-bearing crash/concurrency isolation, absence of the test exerciser from
the normal production build, exact external config/metadata/kernel/initrd
adoption by the normal worker, config-file and delayed API block/pmem adoption,
startup-CLI/config-file and delayed-API logger/metrics/serial adoption,
pathname-replacement identity, exact role/access and one-time failures,
read-only guest-write rejection, writable block persistence, pmem read/flush,
guest console output through the transferred serial descriptor, terminal
metrics, concurrent output-session isolation, preauthorized live block
replacement, limiter-only backing retention, redacted failure atomicity, and
granted native-v1 create/describe/state-memory-root restore, strict snapshot
staging cleanup after worker death, and real sandboxed HVF guests through
`SYSTEM_OFF`. It also proves an
outside-container client can use a granted API socket, and that a real guest
can complete deterministic bidirectional and half-close/EOF vsock traffic in
both initiation directions through the supplied granted listener and fixed
launcher broker, without changing the exact entitlements or leaving a helper
in steady state. Abrupt launcher-first and worker-first cases replace the
granted API pathname before death and prove both surviving cleanup owners
preserve the replacement while clearing the matching private namespace record.

Prepare the pinned Firecracker arm64 Linux kernel artifact used by guest boot
validation work:

```sh
scripts/fetch-firecracker-kernel.sh
```

Run only the minimal guest boot integration test on macOS Apple Silicon:

```sh
scripts/run-integration-tests.sh --test guest_boot
```

Hosted macOS CI may build and sign integration tests without executing HVF:

```sh
scripts/run-integration-tests.sh --allow-unsupported
```

See [Testing Guide](docs/testing.md) for test layering, signed integration-test
rules, guest boot artifact caching, and local verification expectations.

## Exit Status

- `0`: help or version completed successfully, the API server exited without
  error, or no-api mode handled `SIGINT`/`SIGTERM`.
- `152`: startup configuration failed before the process entered runtime,
  including config-file, metadata, logger-sink, and metrics-sink configuration
  failures. This matches Firecracker's bad-configuration exit
  code.
- `153`: startup argument parsing failed before process configuration began.
  This matches Firecracker's argument-parsing exit code.
- `148`, `149`, `150`, `151`, `154`, `156`, `157`: Firecracker-compatible
  fatal or restricted host signal exits for `SIGSYS`, `SIGBUS`, `SIGSEGV`,
  `SIGXFSZ`, `SIGXCPU`, `SIGHUP`, and `SIGILL`.
- `1`: process failure, including API socket bind, shutdown signal handling, API
  accept failures, or process-owned runtime failures.
