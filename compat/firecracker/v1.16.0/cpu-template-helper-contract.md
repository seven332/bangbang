# CPU-template Helper Foundation Contract

This contract owns the portable format, selection, provider, input, and
publication boundary for Firecracker v1.16.0 CPU-template dump and verify
work. The runtime register allowlist, application order, readback, boot
precedence, and platform exclusions remain owned by the
[CPU-template contract](cpu-template-contract.md).

## Delivery status

Issue [#1861](https://github.com/seven332/bangbang/issues/1861) supplies a
library-only `bangbang-cpu-template-helper` foundation. It deliberately has no
binary target, command-line parser, Hypervisor.framework dependency, or
production effective-profile provider. Tests may supply a fake provider, but
production code cannot report a successful dump or verification without a
caller-provided implementation of the provider contract.

Issue [#1862](https://github.com/seven332/bangbang/issues/1862) owns the public
executable, real signed HVF provider, command behavior, and end-to-end
promotion gate. Until that slice is merged, these seven checked identities
remain exactly `audit-required`:

| Capability identity | Current disposition |
| --- | --- |
| `tool-argument:cpu-template-helper/template/dump/config` | `audit-required` |
| `tool-argument:cpu-template-helper/template/dump/output` | `audit-required` |
| `tool-argument:cpu-template-helper/template/dump/template` | `audit-required` |
| `tool-argument:cpu-template-helper/template/verify/config` | `audit-required` |
| `tool-argument:cpu-template-helper/template/verify/template` | `audit-required` |
| `tool-operation:cpu-template-helper/template/dump` | `audit-required` |
| `tool-operation:cpu-template-helper/template/verify` | `audit-required` |

## Configuration and selection

The API crate owns one duplicate-safe complete config-document parser used by
both VMM startup and the helper foundation. A complete document must retain
the existing top-level object, known-section, required `boot-source`, strict
section parsing, and production ordering rules. Parsing does not open paths
named by boot, drive, logger, metrics, snapshot, or device sections.

Helper preparation projects only machine and CPU requests through the real
backend-neutral `VmmController`. Every section must still parse, and an invalid
earlier machine or CPU request fails even when a later explicit template would
replace its selection. Within a valid document, the config CPU selection
follows machine static-template selection. An explicit template document is
applied last and therefore has final precedence.

Transport mapping is intentionally narrow and tested against the production
API-server mapping. The helper does not duplicate controller validation or
invent a separate machine/CPU state machine.

## Descriptor and provider authority

The runtime crate owns the exact sorted 80-entry arm64 descriptor census used
by both accepted custom-template decoding and future effective capture. Each
descriptor binds a Firecracker/KVM-shaped compatibility identity to one typed
U32, U64, or U128 runtime target, its admitted filter, boot disposition, and
availability class. The serialized identity is a compatibility token; it does
not imply a KVM handle or KVM execution mechanism on macOS.

An effective provider returns one topology-common profile in descriptor order.
Every entry carries its identity explicitly as well as an exact-width
`Available` value or `Unavailable` status. Construction rejects count, order,
identity, width, and baseline-availability drift, including swaps between
same-width registers. All baseline entries must be available. Only the
descriptor-marked macOS 15 ACTLR and macOS 15.2 ZFR0/SMFR0 entries may be
unavailable.

Dump captures one profile and includes every available `Retained` entry. It
omits X0, PC, and PSTATE because normal boot setup supersedes those applied
values. Each emitted modifier uses the descriptor's complete admitted filter:
full width for ordinary entries and only ACTLR.EnTSO bit 1 for ACTLR. Optional
unavailable entries are omitted.

Verify requires a selected nonempty custom template, captures once at the
application checkpoint, and compares each requested entry as
`(effective & filter) == value`. A missing descriptor, unavailable requested
entry, or mismatch fails without exposing the identity, mask, expected value,
or effective value. Extra profile entries do not affect the result. The future
HVF provider must obtain a real common result for every configured vCPU; this
library does not synthesize that platform evidence.

## Template document

Input accepts value-equivalent spellings admitted by the strict `/cpu-config`
parser, including insignificant JSON whitespace and object-field order. It
rejects duplicate keys or targets, aliases, identity/width mismatches, values
outside filters or widths, unsupported KVM capability or vCPU-feature
categories, forbidden registers, and every other input that cannot become one
closed runtime custom template.

Canonical output is UTF-8 JSON with these fixed properties:

- fields appear as `kvm_capabilities`, `reg_modifiers`, and `vcpu_features`;
- the first and last arrays are empty;
- modifiers are sorted by ascending compatibility identity;
- addresses are `0x` plus 16 lowercase hexadecimal digits;
- bitmaps are `0b` plus exactly 32, 64, or 128 `0`, `1`, or `x` characters;
- serde's two-space pretty layout is used; and
- exactly one newline terminates the document.

Both accepted inputs and encoded outputs are capped at 1 MiB. Canonical output
must re-enter the same strict API and runtime closure without loss.

## Input and publication boundary

Path input is opened read-only with close-on-exec, no-follow, and nonblocking
flags. Descriptor metadata must identify a regular file. Size is checked both
before and during the bounded read, and the complete bytes must be UTF-8.
Failures expose only a fixed category; paths and contents are not retained in
`Debug` or `Display`.

Output publication is absent-only. The parent is opened as a no-follow
directory, and an existing regular file, symlink, directory, or concurrent
winner is a collision whose bytes are preserved. A private same-directory
stage is exclusively created with mode `0600`, written completely, flushed,
file-synchronized, and checked against its captured device/inode identity.
The only commit point is an exclusive `NOREPLACE` rename.

Before commit, cleanup removes only a stage whose current identity still
matches the captured identity, then synchronizes the directory. An identity
change or cleanup/sync failure returns `PrecommitCleanupUncertain` and never
removes the unknown object. After rename, the final path is not rolled back.
A final-identity failure returns `CommittedStateUncertain`; a directory-sync
failure returns `CommittedDurabilityUncertain`. Both postcommit classes mean a
complete rename may already be visible and must not be reported as a safe
precommit failure.

No-clobber and synchronization protect publication integrity and failure
classification; they do not authenticate template contents or make a hostile
ancestor path trustworthy. Operators remain responsible for the authority and
permissions of the selected directory.

## Diagnostics, exit classes, and evidence

All library errors and custom `Debug` implementations are bounded and omit
paths, config values, register identities, filters, target values, effective
values, and provider internals. The reserved public exit classes are 0 for
success, 1 for an operational failure, and 2 for invocation failure. #1862
owns the exact mapping from public commands and arguments to those classes.

Portable tests cover parser sharing, production projection parity, descriptor
census and decoder closure, canonical round trips, selection precedence,
identity-bound profiles, optional availability, filtered verification,
bounded no-follow input, collision preservation, short writes, synchronization
faults, identity replacement, cleanup uncertainty, and postcommit uncertainty.
The audit guard also proves that the package remains library-only and that the
seven public capability identities remain nonterminal. Real provider and
process evidence must run through the signed integration wrapper in #1862.
