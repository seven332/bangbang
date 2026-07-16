# Copyright (c) 2026 bangbang contributors
# SPDX-License-Identifier: Apache-2.0

BEGIN {
    FS = ","
    expected_header = "#syscall (v6.13.0 2025-01-23),x86,x86_kver,x86_64,x86_64_kver,x32,x32_kver,arm,arm_kver,aarch64,aarch64_kver"
    expected_header = expected_header ",loongarch64,loongarch64_kver,m68k,m68k_kver,mips,mips_kver,mips64,mips64_kver,mips64n32,mips64n32_kver"
    expected_header = expected_header ",parisc,parisc_kver,parisc64,parisc64_kver,ppc,ppc_kver,ppc64,ppc64_kver,riscv64,riscv64_kver"
    expected_header = expected_header ",s390,s390_kver,s390x,s390x_kver,sh,sh_kver"
}

NR == 1 {
    if ($0 != expected_header) {
        print "unexpected libseccomp syscall header" > "/dev/stderr"
        exit 1
    }

    next
}

{
    if ($1 in reduced_rows) {
        print "duplicate syscall name" > "/dev/stderr"
        exit 1
    }
    if ($4 != "PNR" && $4 !~ /^[0-9]+$/) {
        print "invalid x86_64 syscall value" > "/dev/stderr"
        exit 1
    }
    if ($10 != "PNR" && $10 !~ /^[0-9]+$/) {
        print "invalid aarch64 syscall value" > "/dev/stderr"
        exit 1
    }

    reduced_rows[$1] = $1 "," $4 "," $10
    rows++
}

END {
    if (rows != 502) {
        print "unexpected libseccomp syscall row count" > "/dev/stderr"
        exit 1
    }

    print "# SPDX-License-Identifier: LGPL-2.1-or-later"
    print "# Source: libseccomp v2.6.0 src/syscalls.csv"
    print "# Source commit: c7c0caed1d04292500ed4b9bb386566053eb9775"
    print "# Source SHA-256: 3fc607fffc9c3b0aca77fd6ffc3aa0f86c61b90dc255baedfc396e9a5e102fdc"
    print "name,x86_64,aarch64"

    # POSIX awk has no asorti(). A 502-row selection sort keeps this reducer
    # portable to the awk shipped with macOS and makes review order stable.
    for (output_rows = 0; output_rows < rows; output_rows++) {
        next_name = ""
        for (name in reduced_rows) {
            if (next_name == "" || name < next_name) {
                next_name = name
            }
        }
        print reduced_rows[next_name]
        delete reduced_rows[next_name]
    }
}
