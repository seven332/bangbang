# libseccomp v2.6.0 syscall data

`libseccomp-v2.6.0-syscalls.csv` is a deterministic three-column reduction of
libseccomp's `src/syscalls.csv` at annotated tag `v2.6.0`, commit
`c7c0caed1d04292500ed4b9bb386566053eb9775`.

The complete upstream input has SHA-256
`3fc607fffc9c3b0aca77fd6ffc3aa0f86c61b90dc255baedfc396e9a5e102fdc`.
It contains 502 unique names. The reduced x86_64 column contains 379 numeric
and 123 `PNR` entries; aarch64 contains 322 numeric and 180 `PNR` entries.

To reproduce the reduction from a checkout of the exact commit:

```sh
shasum -a 256 src/syscalls.csv
awk -f /path/to/bangbang/tools/seccompiler/scripts/reduce-libseccomp-syscalls.awk \
    src/syscalls.csv
```

Compare the command's standard output with the checked CSV. The reducer checks
the exact upstream header, row count, name uniqueness, and value shape, then
emits alphabetical order; the crate's unit tests independently check the
reduced table and sentinel mappings.

The derived data is distributed under libseccomp's LGPL-2.1-or-later terms.
See `LICENSE-LGPL-2.1` in this directory. Compiler code adapted from the former
Firecracker pure-Rust backend carries its separate Apache-2.0 notice in the
relevant source files; see `../LICENSE-APACHE-2.0` for those terms.

Sources:

- https://github.com/seccomp/libseccomp/blob/v2.6.0/src/syscalls.csv
- https://github.com/seccomp/libseccomp/tree/v2.6.0
