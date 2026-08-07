# CPU-template fingerprint compare contract

This document is the checked terminal contract for #1867, the second and final
#1794 delivery slice. It covers the Firecracker v1.16.0
`cpu-template-helper fingerprint compare` command, Bangbang's platform-honest
field vocabulary, strict persisted inputs, deterministic selected-value
diagnostic, portable execution, and scoped capability transition. Fingerprint
dump remains owned by #1866; #1795 terminally composes the corpus and aggregate
scope without changing this command's four-row ownership.

## Pinned upstream boundary

The clean sibling source authority is Firecracker v1.16.0 commit
`d83d72b710361a10294480131377b1b00b163af8`. Relevant upstream files are:

- `src/cpu-template-helper/src/main.rs`;
- `src/cpu-template-helper/src/fingerprint/mod.rs`;
- `src/cpu-template-helper/src/fingerprint/compare.rs`; and
- `docs/cpu_templates/cpu-template-helper.md`.

Upstream requires `-p/--prev` and `-c/--curr`, accepts one-or-more values after
`-f/--filters`, and defaults an absent filter option to its six declared fields.
Those Linux-oriented fields are Firecracker version, kernel version,
microcode version, BIOS version, BIOS revision, and guest CPU configuration.
It iterates caller order without duplicate rejection, pretty-serializes one
`name`/`prev`/`curr` object per difference, joins those top-level objects, and
returns them through the normal failure path. An unequal guest configuration is
first passed through the same two-input template strip operation.

The upstream documentation defines change awareness, not a compatibility
decision. It explicitly notes that a detected change may be inconsequential for
a particular template. Its ordinary file reads, permissive unversioned JSON,
unbounded output, caller-controlled ordering, and raw errors do not define
Bangbang's persisted-input or diagnostic security boundary.

## Public command and filter vocabulary

Bangbang exposes exactly:

```text
cpu-template-helper fingerprint compare \
  --prev|-p PATH \
  --curr|-c PATH \
  [--filters|-f FIELD...]
```

Both paths are required. An explicit filter occurrence consumes at least one
value, and multiple occurrences append to the same requested set. The closed
global vocabulary and public comparison order are:

1. `producer_version`;
2. `kernel_release`;
3. `macos_product`;
4. `macos_target`;
5. `macos_cpu_family`;
6. `linux_microcode_version`;
7. `linux_bios_version`;
8. `linux_bios_revision`; and
9. `guest_cpu_config`.

The names describe Bangbang's versioned document honestly. In particular,
Apple product/target/CPU-family facts never occupy Firecracker's Linux
microcode or DMI names.

An absent filter option selects every applicable variable fact. A macOS pair
therefore selects producer version, kernel release, the three macOS facts, and
guest CPU state. A Linux pair selects producer version, kernel release, the
three Linux facts, and guest CPU state. Explicit caller order never changes
diagnostic order. Unknown, explicitly valueless, or duplicate fields are one
invalid invocation and fail before either path is accessed. A known field for
the other platform is syntactically valid but operationally unavailable after
strict document admission.

## Strict input and platform admission

Each path is independently opened read-only, close-on-exec, no-follow, and
nonblocking. The retained descriptor must identify a regular file. The reader
checks the inspected length and a one-byte-over-limit read so a growing input
cannot exceed 1 MiB, then validates UTF-8. Missing paths, symlinks, directories,
FIFOs, other special files, invalid UTF-8, oversized input, and read/inspection
failure produce path-redacted categories.

Both complete strings pass through the #1866 version-1 authority before any
field is compared. That decoder requires schema `1`, fixed producer name
`bangbang-cpu-template-helper`, exact Firecracker compatibility `1.16.0`, one
bounded canonical producer SemVer, normalized bounded kernel/host facts, one
closed tagged host variant, and one strict nested arm64 CPU-template document.
Unknown, duplicate, mixed, missing, invalid-null, malformed, unsupported, or
noncanonical semantic state fails without a partial diagnostic.

Producer SemVer is comparison data so helper upgrades remain visible. Schema,
producer name, and Firecracker compatibility are admission identities. The
host tag is correlated with fixed OS/machine provenance: macOS is
`Darwin`/`arm64`, Linux is `Linux`/`aarch64`. Kernel release remains comparison
data. Two valid documents with different tags fail with a platform-mismatch
category even if the explicit filter set contains only common fields.

An explicit macOS fact on Linux or Linux fact on macOS fails as unavailable;
it is never treated as a synthetic null. An applicable nullable macOS product,
target, or CPU-family fact compares normally. Thus null/null is equal and
null/value is an intentional reportable difference. Same-path and hard-link
aliases are accepted because the operation is read-only; each requested read
still binds and reads its own opened descriptor. The command does not claim a
simultaneous filesystem snapshot or authenticate concurrently mutable input.

## Deterministic difference diagnostic

Equal selected state writes no stdout or stderr and exits successfully. One or
more differences produce one standalone pretty JSON value on stderr:

```json
{
  "differences": [
    {
      "name": "kernel_release",
      "prev": "25.4.0",
      "curr": "25.5.0"
    }
  ]
}
```

The envelope has only the ordered `differences` member. Every record has only
`name`, `prev`, and `curr` in that order. Records follow the global order,
independent of explicit argument order. Required scalar facts are JSON strings;
nullable macOS facts use string or null; CPU family retains the artifact's
`0x` plus eight lowercase hexadecimal digits. Unselected values, fixed
admission identities, paths, and parse/error details never enter this payload.

The complete output is serialized in memory with serde's two-space pretty
layout, receives exactly one trailing newline, and must not exceed the existing
1 MiB helper-document bound. Serialization and the bound check finish before
the first stream write. A stream can still fail after accepting a prefix; the
process retains its nonzero result and makes no filesystem mutation.

The selected values are intentionally user-requested diagnostic material and
are not category-only errors. They are emitted without the ordinary
`cpu-template-helper:` prefix so the payload remains one canonical JSON value.
Every actual admission, transform, encoding, or size failure remains a bounded
value/path-free category instead. A stream failure cannot add another
diagnostic reliably; it exposes no extra values and retains exit 1.

## Guest CPU difference semantics

Raw typed guest documents are first compared for equality. If they differ,
the previous and current documents are cloned into one exact two-item call to
`strip_cpu_template_documents`. For every identity present on both sides, the
transform removes bits selecting the same value and narrows each modifier to
the union of differing selected bits. An identity missing from either side
remains complete on the side where it exists.

U32, U64, and U128 widths remain native. Filters and values stay masked, sorted,
and encoded through the same strict `CpuTemplateDocument` serializer used by
template dump/strip. The diagnostic's nested `prev` and `curr` therefore show
only the meaningful bit difference while remaining valid canonical typed
template objects. No recursive or generic JSON diff exists.

## Execution, diagnostics, and exit classes

Comparison is portable host-side work. The dispatcher receives the helper's
usual provider objects but never calls the host-fingerprint or effective-CPU
provider, constructs an HVF VM/vCPU, checks an entitlement, or invokes either
artifact publisher. It reads exactly the two requested inputs and writes only
the terminal diagnostic stream.

- Help/version use stdout and exit 0.
- Equal selected state is silent and exits 0.
- A detected difference writes the canonical JSON to stderr and exits 1.
- Input, document, platform, unavailable-filter, transform, encoding, or bound
  failure uses a fixed category and exits 1.
- A terminal stream failure retains exit 1; no additional output is guaranteed
  once that stream rejects bytes.
- Missing/extra arguments and unknown, empty, or duplicate filters write the
  existing fixed invalid-arguments line and exit 2 before path access.

Difference exit 1 matches Firecracker's failure result and remains distinct
from invocation exit 2. Difference values are the sole exception to the
helper's category-only stderr rule. No error string or `Debug` implementation
retains artifact values; comparison-result `Debug` is explicitly redacted.

## Security and compatibility boundary

Both fingerprints are untrusted user-supplied artifacts. Strict parsing and
closed filtering limit interpretation and disclosure; neither provides origin,
freshness, integrity, or host identity. Same-path success proves only that the
two reads normalized equally. A difference reports selected change and does
not prove that the change affects a template.

The command does not recollect a host, verify template application, authorize
migration, prove snapshot portability, compare Firecracker artifact bytes,
establish cross-host equivalence, or write a replacement fingerprint. Users
must separately protect stored artifacts and decide whether a reported change
requires template review.

## Verification

Typed unit tests lock both platform defaults; every common and platform fact;
nullable values; producer upgrades; explicit subsets; duplicate, platform, and
unavailable failures; declaration ordering; canonical repeat; output bound and
redacted `Debug`; plus native U32/U64/U128 guest stripping and missing
identities.

Portable CLI and real-process tests lock exact help/token spelling, short and
long path flags, repeated filter groups, required arguments, default/subset
behavior, caller-order normalization, equal/difference streams and exits,
canonical scalar and nested output, and zero calls to both providers. They also
cover malformed/unsupported documents, invalid unavailable encodings,
cross-platform pairs, oversized/invalid-UTF-8 inputs, symlinks, directories,
FIFOs, aliases, input nonmutation, value/path redaction, and failed diagnostic
writes.

The ordinary unsigned process successfully compares documents on macOS. The
Linux-musl check proves the branch compiles without a capture provider. Full
repository verification still runs the signed integration wrapper without
unsupported skips so earlier dump and effective-HVF behavior cannot regress.

The scoped audit accepts only the ordered historical, #1792 helper, #1793
strip, #1866 fingerprint-dump, and #1867 fingerprint-compare phases. It rejects
partial evidence, reordered dependencies, and partial aggregate transitions.
Every earlier scoped gate remains valid at the terminal #1795 aggregate phase.

## Terminal certification

| Capability | Disposition | Evidence boundary |
| --- | --- | --- |
| `tool-argument:cpu-template-helper/fingerprint/compare/curr` | `implemented-and-verified` | Required short/long current path, safe bounded descriptor input, strict decode, aliases, redaction, and nonmutation above. |
| `tool-argument:cpu-template-helper/fingerprint/compare/filters` | `implemented-and-verified` | Closed vocabulary, applicable defaults, unique explicit subset, fixed order, platform availability, values, and exit behavior above. |
| `tool-argument:cpu-template-helper/fingerprint/compare/prev` | `implemented-and-verified` | Required short/long previous path, safe bounded descriptor input, strict decode, aliases, redaction, and nonmutation above. |
| `tool-operation:cpu-template-helper/fingerprint/compare` | `implemented-and-verified` | Typed platform admission, canonical selected-value diagnostic, exact guest strip, portable zero-provider execution, tests, and scoped audit above. |

`corpus:cpu-template-helper`, `corpus:cpu-templates`, and
`semantic.cpu:configuration-templates-and-feature-state` are terminally
composed by the checked
[`cpu-template-helper-audit.json`](cpu-template-helper-audit.json). The
aggregate result still does not infer distinct-host equivalence, artifact
authenticity, migration safety, or snapshot portability from comparison.
