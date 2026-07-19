# Firecracker v1.16.0 runtime device hotplug contract

This ledger records the exact #1420 block and #1421 pmem promotions inside the
broader pinned `docs/device-hotplug.md` corpus. Block promotes
`api-operation:PUT /drives/{drive_id}` and
`non-swagger-route:DELETE /drives/{drive_id}`. Pmem promotes
`api-operation:PUT /pmem/{id}`, `api-path:/pmem/{id}`, and
`non-swagger-route:DELETE /pmem/{id}` because its PUT, PATCH, and DELETE path is
now complete for the supported non-root profile. The aggregate
`semantic.hotplug:runtime-device-manager` and `corpus:device-hotplug` records
remain `audit-required` until the independent network slice finishes.

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
- All runtime mutations are admitted in `Running` and `Paused` and enter the
  same bounded FIFO owner-thread command path as in-place updates. Snapshot
  quiescence and shutdown close ordinary command admission.
- Default all-virtio-MMIO sessions reject runtime PUT and DELETE before opening
  a proposed direct backing, reserving a contained grant, or changing public
  configuration. Root insertion/removal is nonmutating and rejected.

## Transaction and ownership

The retained PCI manager is the single live inventory for startup and runtime
block and pmem endpoints. Insertion validates and reserves configuration and
metrics before the owner command. A published endpoint retains generation-bound
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

The product profile reserves one 512-KiB BAR and the maximum three MSI-X routes
for every one of the 31 endpoint slots. Startup and runtime endpoints allocate
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

Paths, grant tags, and rejected IDs are omitted from API-facing transaction
errors. Configuration readback remains authorized and therefore reports the
committed direct path or grant tag.

## Guest coordination and evidence

Linux does not receive a platform hotplug notification from this macOS PCI
host, so the operator contract requires a guest PCI rescan after PUT and guest
sysfs removal before DELETE. Separate signed direct-executable and production
App-Sandbox bundle tests run two complete rounds for block and pmem:

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

The contained case additionally replaces every source pathname after launcher
grant preparation, injects a failed access claim, proves the exact grant is
still usable, and confirms guest writes reached only the launcher-opened inodes.
Focused unit tests cover configuration projection, bounded metrics generations,
grant rollback, publication cleanup, dynamic map/take/restore, failed map and
unmap isolation, work/message draining, guest-path republish, terminal commit
handling, paused FIFO ordering, default-MMIO rejection, and exact capacity and
range reuse.

## Explicit exclusions

These slices do not implement root block/pmem insertion or removal, runtime
network mutation, vhost-user block, async/io_uring, automatic guest PCI
notification, PCI state in native-v1 snapshots, direct file-backed HVF pmem
mapping, pmem dirty tracking, or Firecracker's KVM eventfd, timerfd, and
interrupt-controller implementation identities. PCI snapshot profiles remain
rejected before artifact mutation.
