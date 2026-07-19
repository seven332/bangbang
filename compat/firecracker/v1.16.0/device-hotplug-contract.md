# Firecracker v1.16.0 runtime block hotplug contract

This ledger records the exact #1420 promotion inside the broader pinned
`docs/device-hotplug.md` corpus. It promotes only
`api-operation:PUT /drives/{drive_id}` and
`non-swagger-route:DELETE /drives/{drive_id}`. The aggregate
`semantic.hotplug:runtime-device-manager` and `corpus:device-hotplug` records
remain `audit-required` until the independent pmem and network slices finish.

## Public boundary

- Pre-boot `PUT /drives/{drive_id}` retains its existing configuration
  behavior.
- After startup, `PUT` may attach a non-root file-backed drive only when the
  process was started with public all-virtio PCI (`--enable-pci`).
- Bodyless `DELETE /drives/{drive_id}` may remove only an existing non-root
  drive in that same PCI profile. A DELETE body still fails at parsing.
- Both operations are admitted in `Running` and `Paused` and enter the same
  bounded FIFO owner-thread command path as in-place drive updates. Snapshot
  quiescence and shutdown close ordinary command admission.
- Default all-virtio-MMIO sessions reject runtime PUT and DELETE before opening
  a proposed backing or changing public configuration.

## Transaction and ownership

The retained PCI manager is the single live inventory for startup and runtime
block endpoints. Insertion validates and reserves configuration and metrics,
prepares the backing outside the vCPU owner, then publishes the endpoint on the
owner thread. A published endpoint retains generation-bound ownership for its
device work gate, PCI identity and function, capability BAR, shared MSI-X
routes, MMIO dispatcher registration, event/interrupt admission, backing,
metrics entry, and configuration projection. The projection changes only after
publication succeeds. A normal publication failure unwinds every provisional
lease; incomplete publication cleanup instead closes command admission and
marks the worker terminal.

Removal first unpublishes MMIO and ECAM reachability, closes admission, and
drains already admitted work and messages while retaining the exact leases. A
recoverable preparation failure republishes the same endpoint. The commit
boundary then releases device state, interrupt routes, BAR, PCI function,
dispatcher registration, backing, metrics generation, and configuration in
that order before the capacity can be reused. Incomplete insertion cleanup,
failure to restore a preparation-stage guest path, or any failure after that
irreversible boundary marks the worker terminal instead of presenting partially
live state.

The product profile reserves one 512-KiB BAR and the maximum three MSI-X routes
for every one of the 31 endpoint slots. Startup and runtime endpoints allocate
from the same bounded pools. Removal tests prove that a slot, BAR, vector set,
dispatcher identity, metrics entry, and drive ID can be reused only after the
old endpoint commits teardown.

## Contained authority

Direct mode opens a runtime backing on the API thread before submitting the
owner command. Contained mode never treats the grant tag as an ambient path. It
may reserve only an exact, unused, initial-manifest `drive-backing` grant with
access matching `is_read_only`. Preparation duplicates the already-open file
for the candidate while retaining the original grant in a typed rollback
claim. Any validation, preparation, admission, or publication failure restores
that original authority; successful publication consumes it exactly once.
Successful removal closes the active duplicate but does not recreate consumed
launcher authority.

Paths, grant tags, and rejected IDs are omitted from API-facing transaction
errors. Configuration readback remains authorized and therefore reports the
committed direct path or grant tag.

## Guest coordination and evidence

Linux does not receive a platform hotplug notification from this macOS PCI
host, so the operator contract requires a guest PCI rescan after PUT and guest
sysfs removal before DELETE. The signed direct executable and production
App-Sandbox bundle tests each run two complete rounds:

1. start a PCI guest with a permanent control drive;
2. PUT an initially absent block endpoint;
3. rescan PCI and verify a host seed by guest read;
4. overwrite it, read it back, and fsync;
5. remove the PCI function through guest sysfs;
6. pause, DELETE, and PUT a replacement using the released capacity;
7. resume and repeat guest rescan/I/O/fsync/sysfs removal;
8. DELETE the replacement and stop cleanly.

The contained case additionally replaces every source pathname after launcher
grant preparation, injects a failed access claim, proves the exact grant is
still usable, and confirms guest writes reached only the launcher-opened inodes.
Focused unit tests cover configuration projection, bounded metrics generations,
grant rollback, publication cleanup, work/message draining, guest-path
republish, terminal commit handling, paused FIFO ordering, default-MMIO
rejection, and exact capacity reuse.

## Explicit exclusions

This slice does not implement root-drive insertion/removal, runtime pmem or
network mutation, vhost-user block, async/io_uring, automatic guest PCI
notification, PCI state in native-v1 snapshots, or Firecracker's KVM eventfd,
timerfd, and interrupt-controller implementation identities. PCI snapshot
profiles remain rejected before artifact mutation.
