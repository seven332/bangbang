#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const STDOUT: usize = 1;
const SYS_WRITE: usize = 64;
const SYS_EXIT: usize = 93;
const REPORT_HEADER: &[u8] = b"BANGBANG_ARM64_ID_REPORT_V1\n";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    exit(101)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // SAFETY: these architected identification registers are readable from
    // Linux EL0 through the kernel's sanitized CPU-feature view.
    let values = unsafe { read_id_registers() };
    let mut report = [0_u8; 160];
    let mut length = 0;

    if !append(&mut report, &mut length, REPORT_HEADER)
        || !append_register(&mut report, &mut length, b"pfr0=", values[0])
        || !append_register(&mut report, &mut length, b"isar0=", values[1])
        || !append_register(&mut report, &mut length, b"isar1=", values[2])
        || !append_register(&mut report, &mut length, b"mmfr2=", values[3])
    {
        exit(2);
    }

    let mut written = 0;
    while written < length {
        // SAFETY: `written` remains within the initialized report prefix, so
        // the remaining pointer and length form a readable buffer.
        let result = unsafe {
            write(
                STDOUT,
                report.as_ptr().add(written),
                length.saturating_sub(written),
            )
        };
        if result <= 0 {
            exit(3);
        }
        written = written.saturating_add(result as usize);
    }

    exit(0)
}

unsafe fn read_id_registers() -> [u64; 4] {
    let pfr0;
    let isar0;
    let isar1;
    let mmfr2;
    // SAFETY: the helper runs only as an aarch64 Linux userspace process, and
    // Linux exposes these four identification registers through its safe EL0
    // emulation path.
    unsafe {
        asm!(
            "mrs {pfr0}, ID_AA64PFR0_EL1",
            "mrs {isar0}, ID_AA64ISAR0_EL1",
            "mrs {isar1}, ID_AA64ISAR1_EL1",
            "mrs {mmfr2}, ID_AA64MMFR2_EL1",
            pfr0 = out(reg) pfr0,
            isar0 = out(reg) isar0,
            isar1 = out(reg) isar1,
            mmfr2 = out(reg) mmfr2,
            options(nomem, nostack, preserves_flags),
        );
    }
    [pfr0, isar0, isar1, mmfr2]
}

fn append_register(buffer: &mut [u8], length: &mut usize, name: &[u8], value: u64) -> bool {
    if !append(buffer, length, name) {
        return false;
    }
    for shift in (0..16).rev().map(|index| index * 4) {
        let digit = ((value >> shift) & 0xf) as usize;
        let Some(byte) = HEX.get(digit).copied() else {
            return false;
        };
        if !append(buffer, length, &[byte]) {
            return false;
        }
    }
    append(buffer, length, b"\n")
}

fn append(buffer: &mut [u8], length: &mut usize, bytes: &[u8]) -> bool {
    let Some(end) = length.checked_add(bytes.len()) else {
        return false;
    };
    let Some(destination) = buffer.get_mut(*length..end) else {
        return false;
    };
    destination.copy_from_slice(bytes);
    *length = end;
    true
}

unsafe fn write(fd: usize, buffer: *const u8, length: usize) -> isize {
    let result;
    // SAFETY: the caller provides a readable buffer for `length` bytes, and
    // the Linux aarch64 write syscall uses the declared register ABI.
    unsafe {
        asm!(
            "svc 0",
            in("x8") SYS_WRITE,
            inlateout("x0") fd => result,
            in("x1") buffer,
            in("x2") length,
            lateout("x3") _,
            lateout("x4") _,
            lateout("x5") _,
            lateout("x6") _,
            lateout("x7") _,
            options(nostack),
        );
    }
    result
}

fn exit(status: usize) -> ! {
    // SAFETY: the Linux aarch64 exit syscall takes only the supplied status
    // and never returns.
    unsafe {
        asm!(
            "svc 0",
            in("x8") SYS_EXIT,
            in("x0") status,
            options(noreturn, nostack),
        );
    }
}

#[unsafe(no_mangle)]
#[inline(never)]
unsafe extern "C" fn memcpy(destination: *mut u8, source: *const u8, length: usize) -> *mut u8 {
    for index in 0..length {
        // SAFETY: the compiler calls this symbol with non-overlapping readable
        // and writable ranges of at least `length` bytes.
        unsafe {
            destination
                .add(index)
                .write_volatile(source.add(index).read_volatile());
        }
    }
    destination
}

#[unsafe(no_mangle)]
#[inline(never)]
unsafe extern "C" fn memset(destination: *mut u8, value: i32, length: usize) -> *mut u8 {
    for index in 0..length {
        // SAFETY: the compiler calls this symbol with a writable range of at
        // least `length` bytes.
        unsafe {
            destination.add(index).write_volatile(value as u8);
        }
    }
    destination
}
