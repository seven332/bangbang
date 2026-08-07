# CPU-template Strip Contract

This contract owns the portable Firecracker v1.16.0 `cpu-template-helper
template strip` command, its normalized arm64 transformation, and its
multi-path publication boundary. Dump and verify remain governed by the
[CPU-template helper contract](cpu-template-helper-contract.md), while runtime
register admission and execution remain governed by the
[CPU-template contract](cpu-template-contract.md).

## Terminal certification

Issue [#1793](https://github.com/seven332/bangbang/issues/1793) promotes
exactly these three checked identities:

| Capability identity | Current disposition |
| --- | --- |
| `tool-argument:cpu-template-helper/template/strip/paths` | `implemented-and-verified` |
| `tool-argument:cpu-template-helper/template/strip/suffix` | `implemented-and-verified` |
| `tool-operation:cpu-template-helper/template/strip` | `implemented-and-verified` |

The scoped `validate --cpu-template-strip-final` gate requires the prior seven
dump/verify identities and these exact three rows to be terminal with exact
evidence. It also requires all eleven fingerprint, helper-corpus,
template-corpus, and aggregate CPU-template identities owned by #1794 and
#1795 to remain evidence-free `audit-required` handoffs.

## Command and input closure

The command preserves the pinned public spelling: `template strip` requires
`-p`/`--paths` with at least two path values, accepts no fixed maximum, and
uses `-s`/`--suffix` with `_stripped` as the default. An explicitly empty
suffix selects exact input replacement. Strip is portable host-side work: it
does not construct an HVF provider, VM, vCPU, guest memory, or run loop.

Every input is consumed in argument order through the same strict, 1 MiB,
UTF-8 custom-template decoder used by dump and verify. Each retained parent is
opened as a no-follow directory, and each basename is opened close-on-exec,
no-follow, nonblocking, and read-only. Descriptor metadata must identify a
regular file. Paths, contents, device/inode identities, register identities,
filters, and values are absent from diagnostics and custom `Debug` output.

The suffix cannot contain a platform path separator. Output naming follows
Firecracker's stem-plus-suffix-plus-optional-extension rule in the input's
same directory. The complete batch rejects repeated file identities, repeated
directory/basename entries, duplicate outputs, and nonempty-suffix output/input
collisions. Empty-suffix replacement additionally requires every input to be
its exact derived output and to have one link both during preparation and at
commit; this prevents an unlisted hard link from retaining the replaced
private input inode. Nonempty-suffix mode can read a multiply linked regular
input because it never mutates that inode.

## Normalized strip transformation

Strict decoding first normalizes every admitted U32, U64, and U128 modifier
into one identity-sorted map. For each identity present in every input, the
transform ORs the pairwise differences between each selected value and the
first selected value. A zero difference removes that identity from every
output. A nonzero difference intersects each original filter with the
difference mask and masks its value to the resulting filter. An identity
missing from any input remains unchanged in every input where it is present.

The transform therefore matches Firecracker's common-bit strip semantics
without widening native register values or inventing modifiers. Every result,
including an empty result or an all-`x` bitmap, is encoded in the one canonical
custom-template wire form and is proven to re-enter the strict decoder.

## Multi-path publication boundary

Before the first final-path mutation, the helper validates every retained
input and destination, probes the required atomic rename primitive in every
unique directory, encodes every output, creates every owner-only private
same-directory stage, writes and flushes every complete artifact, synchronizes
every stage, rechecks its identity, and synchronizes every unique directory.
An unsupported directory or any precommit failure cleans every independently
owned stage best effort and reports uncertainty if identity-safe cleanup or a
directory sync cannot be confirmed.

Nonempty-suffix publication uses an atomic `NOREPLACE` rename, so an existing
regular file, symlink, directory, or concurrent winner is preserved. Empty
suffix rechecks the retained descriptor, directory entry, exact identity, and
single-link count immediately before an atomic `EXCHANGE`; the displaced old
input stays at the private stage name until the complete batch has committed
and all unique directories have synchronized.

Final paths commit in input order. If one commit fails or becomes uncertain,
the helper attempts every earlier rollback in reverse order and every
remaining owned-stage cleanup even after an individual operation fails. It
removes only identities it owns, preserves unknown replacements, synchronizes
all affected directories, and distinguishes confirmed rollback from uncertain
state. A concurrent destination winner is never deleted. For empty-suffix
replacement, a captured racing replacement is exchanged out and restored when
its identity can be proven; an unprovable object remains untouched and makes
the result uncertain.

After a complete commit, every unique directory is synchronized before old
empty-suffix input inodes are identity-checked and removed from their private
stage names, followed by another directory sync. Failures distinguish
committed durability uncertainty from committed cleanup uncertainty. The
operation deliberately does not claim one global transaction or crash-atomic
rollback across multiple paths or filesystems: each final rename is atomic,
but a process or host crash can expose a committed prefix or retain private
stages. Operators must inspect an uncertainty result before retrying.

These controls provide bounded no-clobber or exact-replacement integrity; they
do not authenticate template contents or make a hostile ancestor directory
trustworthy. They also do not lock or serialize a different process that
already holds an input inode open for in-place writes. Operators must quiesce
input writers for the duration of strip, especially before empty-suffix
replacement.

## Diagnostics and evidence

Success is silent. Clap help and version retain stdout/exit 0, malformed
invocation emits one fixed stderr line/exit 2, and operational failures emit
one fixed category-only line/exit 1. No failure class retains path or template
values.

Portable unit and actual-process tests cover native-width common-bit results,
missing identities, empty and all-`x` canonical documents, arity and suffix
rules, duplicate/collision/link admission, provider independence, output
naming, silent success, and redaction. Fault injection covers every observed
pre-stage and split-commit boundary in both publication modes, racing winners,
identity replacement, reverse best-effort rollback, unknown-stage
preservation, durability uncertainty, and cleanup uncertainty across multiple
directories. The dedicated audit gate pins the exact three terminal identities
and the eleven retained later scopes.
