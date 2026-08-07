# CPU-template fingerprint dump contract

This document is the checked terminal contract for #1866, the first #1794
delivery slice. It covers the Firecracker v1.16.0 `cpu-template-helper
fingerprint dump` command surface, Bangbang's platform-tagged document, public
macOS host-fact substitution, real effective guest capture, and failure-aware
publication. Compare/filter behavior remains #1867; corpus and aggregate
certification remain #1795.

## Pinned upstream boundary

The clean sibling source authority is Firecracker v1.16.0 commit
`d83d72b710361a10294480131377b1b00b163af8`. The relevant upstream files are:

- `src/cpu-template-helper/src/main.rs`;
- `src/cpu-template-helper/src/fingerprint/mod.rs`;
- `src/cpu-template-helper/src/fingerprint/dump.rs`; and
- `docs/cpu_templates/cpu-template-helper.md`.

Upstream accepts optional `-c/--config`, optional `-t/--template`, and
`-o/--output` defaulting to `fingerprint.json`. It emits an unversioned object
with helper version, kernel release, Linux arm64 sysfs revision, two Linux DMI
strings, and effective guest CPU configuration. Its host reads are required and
its direct output write has no repository-owned size, strict-reparse,
no-clobber, or durability contract.

The upstream documentation gives the artifact change-awareness authority. A
difference may be inconsequential. A fingerprint does not authenticate a host,
prove that a template is valid, authorize migration, or prove snapshot
portability.

## Public command

Bangbang exposes exactly:

```text
cpu-template-helper fingerprint dump \
  [--config|-c PATH] \
  [--template|-t PATH] \
  [--output|-o PATH]
```

The output default is `fingerprint.json`. Config and template files use the
same bounded, no-follow, regular-file, UTF-8 input and production-controller
projection as `template dump`. Every complete configuration section still
parses. A valid explicit template is applied last and replaces a configuration
CPU selection.

Success is silent and exits 0. Help/version use stdout and exit 0. Malformed
invocation writes one fixed line and exits 2. Input, host, effective-state,
document, and publication failures write one bounded category-only line and
exit 1.

## Version-1 document

The artifact is a Bangbang format because Apple facts cannot truthfully occupy
Linux sysfs/DMI fields. Its fixed top-level order is:

1. `schema_version` — numeric `1` only;
2. `producer` — fixed `name`, variable canonical `version`, and fixed
   `firecracker_compatibility`;
3. `kernel` — operating-system name, release, and machine;
4. `host` — exactly one closed platform variant; and
5. `guest_cpu_config` — the existing normalized Firecracker-shaped custom CPU
   template.

The fixed producer name is `bangbang-cpu-template-helper`, and compatibility is
`1.16.0`. Encoding records the package version. Decoding accepts any canonical
SemVer of at most 64 UTF-8 bytes so later compare can report helper upgrades;
parsing and reformatting the SemVer must produce the identical string.

Every kernel/platform fact is nonempty UTF-8 of at most 255 bytes, has no NUL
or control character, and has no leading or trailing Unicode whitespace.
Persisted decoding validates and never trims. A source adapter may remove only
an interface-defined transport terminator before constructing the fact. The
whole artifact is at most 1 MiB.

All document, producer, kernel, host, and nested modifier objects reject unknown
and duplicate fields. Unsupported schema, producer name, or compatibility
identity fails. The decoder accepts insignificant JSON whitespace and object
formatting, but not non-normalized values or semantic aliases. Canonical
encoding uses declaration order, serde's two-space pretty layout, normalized
nested CPU bytes, lowercase fixed-width numbers, and exactly one final newline.

The dump path constructs canonical bytes in memory, strictly decodes them,
re-encodes the result, and requires exact byte equality before publication.

## Closed host variants

### macOS

The `macos` variant has four fixed fields in this order:

```json
{
  "platform": "macos",
  "product": "Mac product value or null",
  "target": "Apple target value or null",
  "cpu_family": "0x00000000 or null"
}
```

Common kernel provenance must be exact `Darwin` and `arm64`. The provider reads
only:

- `uname(3)` system name, release, and machine;
- public `sysctlbyname(3)` selector `hw.product`;
- public `sysctlbyname(3)` selector `hw.target`; and
- public exact-width `uint32_t` selector `hw.cpufamily`.

The node name/hostname and verbose build string are discarded. Old deprecated
`hw.machine`/`hw.model`, private registry properties, brand strings without the
reviewed SDK authority, frequency, serial number, host ID/UUID, and other
identity sources are never queried.

`uname` fields occupy a 256-byte C array including termination. A sysctl string
is capped at 256 raw bytes including exactly one terminal NUL and no interior
NUL. Invalid UTF-8, empty strings, width/length changes, malformed termination,
or non-normalized results fail. `hw.cpufamily` must return exactly four bytes
and is rendered as `0x` plus eight lowercase hexadecimal digits. The value is
an opaque marketing identity and never a feature-ordering inference.

An `ENOENT` or `EINVAL` result for one reviewed optional sysctl selector becomes
that always-present field's JSON `null`. Every other system error fails. Missing
`uname` facts, malformed available facts, or a platform/kernel mismatch cannot
be represented as unavailability.

### Linux

The closed `linux` variant contains required normalized
`microcode_version`, `bios_version`, and `bios_revision` strings with their
pinned upstream meanings: arm64 revision sysfs and Linux DMI data. Common
kernel provenance must be exact `Linux` and `aarch64`.

#1866 defines, canonically encodes, strictly decodes, and mutation-tests this
variant so #1867 receives one closed authority. The production Bangbang helper
does not emit it on macOS and the non-macOS host provider returns explicit
unsupported instead of fabricating either variant.

## Effective guest state and ordering

The host provider completes once before a disposable HVF inspection begins.
The existing signed provider then creates the selected ordered topology,
applies the selected static/custom state through the production all-vCPU path,
captures the exact 80-entry identity/width/availability profile, tears down the
topology, and returns only values common to every vCPU.

The nested guest document retains every available descriptor marked
`Retained`, uses each descriptor's complete allowed filter, and omits boot-owned
X0, PC, and PSTATE. Optional ACTLR/ZFR0/SMFR0 absence remains explicit in the
provider and causes omission from a default dump. No second CPU-template format
exists.

Default selection and explicit machine `None` produce real signed capture.
Firecracker's AWS/Linux T2CL, T2A, and V1N1 static policies have no
identity-preserving Apple Silicon/HVF source model and fail before publication.
A valid explicit custom document retains final precedence and can replace a
pending configuration selection before inspection.

Host failure prevents effective capture. Effective failure prevents encoding.
Host capture, effective capture, teardown, canonical encode, strict reparse,
and byte-equality validation all finish before the output publisher is called.

## Input, publication, and diagnostics

The existing one-artifact publisher owns the output boundary. It rejects an
existing file, symlink, directory, or concurrent winner. It stages a complete
owner-mode `0600` file in the no-follow parent directory, flushes and
synchronizes it, checks device/inode identity, and commits only through atomic
`NOREPLACE`. The final directory is synchronized. Precommit and postcommit
uncertainty retain their existing explicit classifications.

An input error, unsupported target, host read error, unsigned or failed HVF
provider, static-policy rejection, encoding/reparse failure, collision, or
precommit publication failure never replaces the final path. An unknown
private stage is not removed. Diagnostics and custom `Debug` output omit paths,
host fact values, register identities, filters, guest values, selectors' raw
results, and provider internals.

The artifact itself is intentionally fingerprinting material requested by the
invoking user and written with that user's direct filesystem authority. It is
not emitted by the App Sandbox worker or launcher. Owner-only mode limits
accidental disclosure but does not authenticate contents or make hostile
ancestor directories trustworthy.

## Verification

Portable tests prove macOS present/null and Linux golden bytes, schema/version/
SemVer/platform/field/value/nested mutations, exact fact and artifact bounds,
ordinary JSON whitespace, canonical repeats, value-redacted `Debug`, raw C
termination/UTF-8 behavior, exact public query order, provider failure ordering,
CLI spelling/defaults/exits, input redaction, zero-output failure, and
publication invariants.

The signed helper harness proves a real macOS variant and owner mode, real
two-vCPU default and explicit `None` state, explicit custom precedence over a
pending static selection, unrepresentable V1N1 failure before publication,
collision preservation, successful retry, resource reuse, and unsigned
zero-publication failure. It runs through
`scripts/run-integration-tests.sh --test cpu_template_helper` without
`--allow-unsupported` for local terminal evidence.

The scoped audit accepts only the ordered historical, #1792 helper, #1793 strip,
and #1866 fingerprint-dump phases. It rejects partial evidence, reordered
dependencies, compare leakage, and aggregate leakage. The existing helper and
strip final gates remain valid after this transition.

## Terminal certification

| Capability | Disposition | Evidence boundary |
| --- | --- | --- |
| `tool-argument:cpu-template-helper/fingerprint/dump/config` | `implemented-and-verified` | Exact optional config spelling, strict projection, precedence, real signed capture, and zero-output failures above. |
| `tool-argument:cpu-template-helper/fingerprint/dump/output` | `implemented-and-verified` | Exact optional output spelling/default plus owner-only absent publication, collision, retry, and uncertainty contract above. |
| `tool-argument:cpu-template-helper/fingerprint/dump/template` | `implemented-and-verified` | Exact optional template spelling, strict nested format, final precedence, and real custom capture above. |
| `tool-operation:cpu-template-helper/fingerprint/dump` | `implemented-and-verified` | Versioned closed document, public macOS provider, typed Linux variant, signed effective capture, canonical reparse, publication, and scoped audit above. |

The compare `curr`, `prev`, and `filters` arguments and compare operation remain
exact `audit-required` handoffs to #1867. `corpus:cpu-template-helper`,
`corpus:cpu-templates`, and
`semantic.cpu:configuration-templates-and-feature-state` remain exact
`audit-required` handoffs to #1795.
