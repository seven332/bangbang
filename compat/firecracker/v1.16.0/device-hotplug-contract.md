# Firecracker v1.16.0 runtime device hotplug contract

This ledger records the exact #1420 block, #1421 pmem, #1422 network, and
#1423 aggregate promotions inside the
broader pinned `docs/device-hotplug.md` corpus. Block promotes
`api-operation:PUT /drives/{drive_id}` and
`non-swagger-route:DELETE /drives/{drive_id}`. Pmem promotes
`api-operation:PUT /pmem/{id}`, `api-path:/pmem/{id}`, and
`non-swagger-route:DELETE /pmem/{id}` because its PUT, PATCH, and DELETE path is
now complete for the supported non-root profile. Network promotes
`api-operation:PUT /network-interfaces/{iface_id}` and
`non-swagger-route:DELETE /network-interfaces/{iface_id}`. The aggregate
`semantic.hotplug:runtime-device-manager`, `corpus:device-hotplug`, and
`semantic.transport:pci-msi-and-coexistence` records are
`implemented-and-verified` after #1423's pinned-source differential audit and
cross-device certification.

## Public boundary

- Pre-boot `PUT /drives/{drive_id}` retains its existing configuration
  behavior.
- After startup, `PUT` may attach a non-root file-backed drive only when the
  process was started with public all-virtio PCI (`--enable-pci`).
- Bodyless `DELETE /drives/{drive_id}` may remove only an existing non-root
  drive in that same PCI profile. A DELETE body still fails at parsing.
- After startup, `PUT /pmem/{id}` may attach a non-root, nonzero regular-file
  backing only in that PCI profile. Bodyless `DELETE /pmem/{id}` removes the
  matching live non-root endpoint; live `PATCH` retains its limiter-only
  behavior.
- After startup, `PUT /network-interfaces/{iface_id}` may attach one new
  validated network ID/MAC in that PCI profile. Bodyless
  `DELETE /network-interfaces/{iface_id}` removes the matching live endpoint;
  live `PATCH` continues to update only its limiter buckets.
- All runtime mutations are admitted in `Running` and `Paused` and enter the
  same bounded FIFO owner-thread command path as in-place updates. Snapshot
  quiescence and shutdown close ordinary command admission.
- Default all-virtio-MMIO sessions reject runtime PUT and DELETE before opening
  a proposed direct backing, reserving a contained grant, or changing public
  configuration. Root insertion/removal is nonmutating and rejected.

## Transaction and ownership

The retained PCI manager is the single live inventory for startup and runtime
block, pmem, and network endpoints. Insertion validates and reserves
configuration and metrics before the owner command. A published endpoint retains generation-bound
ownership for its device work gate, PCI identity and function, capability BAR,
shared MSI-X routes, MMIO dispatcher registration, event/interrupt admission,
backing, metrics entry, and configuration projection. The projection changes
only after publication succeeds. A normal publication failure unwinds every
provisional lease; incomplete publication cleanup instead closes command
admission and marks the worker terminal.

Pmem insertion additionally maps the already-open file on the owner thread,
allocates the first 2-MiB-aligned guest range outside DRAM, the full virtio-mem
reservation, and every live pmem range, creates one exact private HVF shadow,
and registers that shadow before endpoint publication. A failed endpoint
publication unregisters the unpublished shadow without flushing it. Failure to
undo that registration is terminal because the guest-memory inventory is no
longer trustworthy.

Removal first unpublishes MMIO and ECAM reachability, closes endpoint admission,
and drains already admitted work and messages while retaining exact leases. A
recoverable preparation failure republishes the same endpoint. Pmem removal
then flushes only its writable shadow to the exact backing and unregisters that
one range; an unmap failure retains the mapping for retry and restores endpoint
reachability. The endpoint commit boundary finally releases device state,
interrupt routes, BAR, PCI function, dispatcher registration, mapping, backing,
metrics generation, guest range, and configuration before capacity can be
reused. Incomplete insertion cleanup, failure to restore a preparation-stage
guest path or pmem mapping, or any failure after the irreversible boundary
marks the worker terminal instead of presenting partially live state.

Network insertion also prepares one independent process packet-I/O entry before
guest reachability. An initially empty or all-MMDS session can add an ID already
selected by immutable pre-boot MMDS config without opening vmnet; a mixed
startup session retains vmnet class for later entries. The pre-reserved provider
entry publishes immediately before its PCI endpoint, and configuration commits
only after both owners succeed. Network removal first prepares reversible PCI
teardown, takes the exact packet-I/O generation, explicitly stops vmnet when
present, and only then commits endpoint teardown. Failed provider cleanup
restores both owners when that is provable; an uncertain system vmnet stop or
any failed restoration/commit is terminal. Successful removal releases queue,
limiter deadline, scheduler, metrics, MMDS detour or vmnet, PCI, and live-config
ownership before the same identity and slot can be reused.

The product profile preflights one 512-KiB BAR and dispatcher identity for
every one of the 31 endpoint slots. It reserves exact routes for configured
fixed endpoints plus the maximum three MSI-X routes used by any supported
runtime class for every remaining slot. Startup and runtime endpoints allocate
from the same bounded pools. Removal tests prove that a slot, BAR, vector set,
dispatcher identity, metrics entry, device ID, and—where applicable—pmem guest
range can be reused only after the old endpoint commits teardown.

## Contained authority

Direct mode opens a runtime block or pmem backing on the API thread before
submitting the owner command. Contained mode never treats a grant tag as an
ambient path. It may reserve only an exact, unused, initial-manifest
`drive-backing` or `pmem-backing` grant with access matching the requested
read-only flag. Preparation duplicates the already-open file for the candidate
while retaining the original grant in a typed rollback claim. Any validation,
preparation, admission, mapping, or publication failure restores that original
authority; successful publication consumes it exactly once. Successful removal
closes the active duplicate but does not recreate consumed launcher authority.

Network packet I/O has no path grant. Direct mode admits the existing supported
host/shared/bridged vmnet forms. Contained runtime preparation evaluates the
selected entry class on the owner thread: MMDS-only entries consume no vmnet
authority, while a vmnet entry must match the granted mode/bridge and fit the
actual live-vmnet count. A denied request starts no backend, publishes no PCI or
metrics state, and leaves configuration unchanged. The normal networkless
production profile therefore supports the all-MMDS hotplug proof without adding
the restricted vmnet entitlement while rejecting a non-MMDS runtime candidate.

Paths, grant tags, and rejected IDs are omitted from API-facing transaction
errors. Configuration readback remains authorized and therefore reports the
committed direct path or grant tag.

## Guest coordination and evidence

Linux does not receive a platform hotplug notification from this macOS PCI
host, so the operator contract requires a guest PCI rescan after PUT and guest
sysfs removal before DELETE. Separate signed direct-executable and production
App-Sandbox bundle tests run two complete rounds for block, pmem, and network:

1. start a PCI guest with a permanent control drive;
2. PUT an initially absent endpoint;
3. rescan PCI and verify a host seed by guest read;
4. overwrite it, read it back, and issue block fsync or pmem flush;
5. remove the PCI function through guest sysfs;
6. pause, DELETE, and PUT a replacement using the released capacity;
7. resume and repeat guest rescan/I/O/fsync/sysfs removal;
8. DELETE the replacement and stop cleanly.

The pmem guest also reads its namespace resource and PCI BDF before both rounds
and refuses success unless the second insertion reuses both exact values. Host
assertions verify each queue-driven flush reached only the corresponding first
or second backing before DELETE.

The network guest begins with one MMDS-selected interface, removes its startup
PCI function, and lets the host DELETE that endpoint before both runtime rounds.
Each round rescans for the configured MAC and modern virtio-net identity,
requires the original BDF, brings the interface up, completes a real MMDS
request, and removes it through sysfs before host DELETE. The second PUT occurs
while Paused. The production case uses the exact networkless worker signature,
also proves a non-MMDS bridged request is denied without mutation, and completes
both MMDS rounds without vmnet authority.

The contained case additionally replaces every source pathname after launcher
grant preparation, injects a failed access claim, proves the exact grant is
still usable, and confirms guest writes reached only the launcher-opened inodes.
Focused unit tests cover configuration projection, bounded metrics generations,
grant rollback, independent packet-I/O start/stop/take/restore, actual-vmnet
authority counting, publication cleanup, dynamic map/take/restore, failed map
and unmap isolation, work/message draining, guest-path republish, terminal
commit handling, snapshot/shutdown admission conflicts, paused FIFO ordering,
default-MMIO rejection, and exact capacity and range reuse.

The #1423 aggregate tests additionally give block, pmem, and network the same
string ID in one public-PCI session, reject same-type duplicates and a second
network identity with the live MAC, mutate the three classes in different
Running/Paused orders across two rounds, and verify the complete live
configuration after each boundary. A concurrent test submits all three classes
through cloned handles and proves exactly-once serialization on the one paused
owner. Shared accounting tests compose fixed and dynamic classes to exactly 31
endpoints, reopen one slot by removing any runtime class, and prove the mixed
demand fits the fixed-plus-worst-case-runtime MSI-X reservation. These focused
tests compose with the signed product all-class startup and separate direct and
contained runtime verticals; they do not replace real guest or containment
evidence.

## Explicit exclusions

These slices do not implement root block/pmem insertion or removal, vhost-user
block, async/io_uring, automatic guest PCI notification, PCI state in native-v1
snapshots, direct file-backed HVF pmem mapping, pmem dirty tracking, externally
certified vmnet connectivity, or Firecracker's KVM eventfd, timerfd, and
interrupt-controller implementation identities. PCI snapshot profiles remain
rejected before artifact mutation. Apple-approved production vmnet credentials
and real external connectivity remain #1351/#1378 work.
