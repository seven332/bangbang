#![no_main]
#![no_std]

use core::arch::asm;
use core::hint::black_box;
use core::panic::PanicInfo;
use core::ptr;

const STDOUT: usize = 1;
const AT_FDCWD: usize = (-100_isize) as usize;
const O_RDONLY: usize = 0;
const O_RDWR: usize = 2;
const O_SYNC: usize = 0x10_1000;
const PROT_WRITE: usize = 2;
const MAP_SHARED: usize = 1;
const MS_ASYNC: usize = 1;
const CLOCK_MONOTONIC: usize = 1;
const TCSETS: usize = 0x5402;

const SYS_IOCTL: usize = 29;
const SYS_MOUNT: usize = 40;
const SYS_OPENAT: usize = 56;
const SYS_CLOSE: usize = 57;
const SYS_READ: usize = 63;
const SYS_WRITE: usize = 64;
const SYS_EXIT: usize = 93;
const SYS_CLOCK_GETTIME: usize = 113;
const SYS_REBOOT: usize = 142;
const SYS_MUNMAP: usize = 215;
const SYS_MMAP: usize = 222;
const SYS_MSYNC: usize = 227;

const EINTR: isize = 4;
const EBUSY: isize = 16;
const LINUX_REBOOT_MAGIC1: usize = 0xfee1_dead;
const LINUX_REBOOT_MAGIC2: usize = 0x2812_1969;
const LINUX_REBOOT_CMD_POWER_OFF: usize = 0x4321_fedc;

const GUEST_PAGE_SIZE: usize = 4096;
const BOOT_TIMER_ADDRESS: usize = 0x4000_0000;
const BOOT_TIMER_MAGIC: u8 = 123;
const RELEASE_TOKEN: u8 = b'R';
const COMPUTE_OPERATIONS: u64 = 5_000_000;
const COMPUTE_CHECKSUM: u64 = 8_398_723_902_783_368_615;
const STORAGE_BYTES: u64 = 16 * 1024 * 1024;
const STORAGE_BLOCK_BYTES: usize = 4096;
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const SERIAL_WRITE_CHUNK_BYTES: usize = 16;

const HEADER: &[u8] = b"BANGBANG_SPEC_WORKLOAD_V1\n";
const READY: &[u8] = b"BANGBANG_SPEC_INIT_READY release_byte=82\n";
const SUCCESS: &[u8] = b"BANGBANG_SPEC_WORKLOAD_OK\n";
const FAILURE_PREFIX: &[u8] = b"BANGBANG_SPEC_WORKLOAD_FAIL phase=";

const DEVTMPFS: &[u8] = b"devtmpfs\0";
const DEV: &[u8] = b"/dev\0";
const SERIAL: &[u8] = b"/dev/ttyS0\0";
const MEMORY: &[u8] = b"/dev/mem\0";
const ROOT_BLOCK: &[u8] = b"/dev/vda\0";

#[repr(C)]
#[derive(Clone, Copy)]
struct Timespec {
    seconds: i64,
    nanoseconds: i64,
}

#[repr(C)]
struct Termios {
    input_flags: u32,
    output_flags: u32,
    control_flags: u32,
    local_flags: u32,
    line: u8,
    control_characters: [u8; 19],
}

const _: [(); 36] = [(); core::mem::size_of::<Termios>()];

#[derive(Clone, Copy)]
struct Failure {
    output_fd: usize,
    phase: &'static [u8],
}

struct Line {
    bytes: [u8; 192],
    length: usize,
}

impl Line {
    const fn new() -> Self {
        Self {
            bytes: [0; 192],
            length: 0,
        }
    }

    fn append(&mut self, value: &[u8]) -> bool {
        let Some(end) = self.length.checked_add(value.len()) else {
            return false;
        };
        let Some(destination) = self.bytes.get_mut(self.length..end) else {
            return false;
        };
        destination.copy_from_slice(value);
        self.length = end;
        true
    }

    fn append_u64(&mut self, value: u64) -> bool {
        let mut digits = [0_u8; 20];
        let mut cursor = digits.len();
        let mut remaining = value;
        loop {
            cursor -= 1;
            digits[cursor] = b'0' + (remaining % 10) as u8;
            remaining /= 10;
            if remaining == 0 {
                break;
            }
        }
        self.append(&digits[cursor..])
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    let _ = write_all(STDOUT, b"BANGBANG_SPEC_WORKLOAD_FAIL phase=panic\n");
    power_off(STDOUT, 101)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok(output_fd) => {
            if !write_all(output_fd, SUCCESS) {
                fail(Failure {
                    output_fd,
                    phase: b"success-write",
                });
            }
            power_off(output_fd, 0)
        }
        Err(failure) => fail(failure),
    }
}

fn run() -> Result<usize, Failure> {
    let mut output_fd = STDOUT;

    // SAFETY: all strings are static NUL-terminated buffers and the mount data
    // pointer is unused for devtmpfs.
    let mounted = unsafe {
        syscall6(
            SYS_MOUNT,
            DEVTMPFS.as_ptr() as usize,
            DEV.as_ptr() as usize,
            DEVTMPFS.as_ptr() as usize,
            0,
            0,
            0,
        )
    };
    // The pinned kernel may have already mounted devtmpfs through
    // CONFIG_DEVTMPFS_MOUNT; EBUSY therefore proves the same ready state.
    if mounted != 0 && mounted != -EBUSY {
        return Err(Failure {
            output_fd,
            phase: b"mount-devtmpfs",
        });
    }

    let serial_fd = open(SERIAL, O_RDWR).ok_or(Failure {
        output_fd,
        phase: b"open-serial",
    })?;
    output_fd = serial_fd;
    configure_serial(output_fd)?;

    signal_boot_complete(output_fd)?;

    let root_block_fd = open(ROOT_BLOCK, O_RDONLY).ok_or(Failure {
        output_fd,
        phase: b"open-root-block",
    })?;

    if !write_all(output_fd, HEADER) || !write_all(output_fd, READY) {
        return Err(Failure {
            output_fd,
            phase: b"ready-write",
        });
    }

    wait_for_release(output_fd)?;

    let compute_started = monotonic_now(output_fd, b"compute-clock-start")?;
    let compute_checksum = compute_workload();
    let compute_finished = monotonic_now(output_fd, b"compute-clock-end")?;
    let compute_duration = elapsed_ns(compute_started, compute_finished).ok_or(Failure {
        output_fd,
        phase: b"compute-clock-range",
    })?;
    if compute_checksum != COMPUTE_CHECKSUM {
        return Err(Failure {
            output_fd,
            phase: b"compute-checksum",
        });
    }
    emit_compute(output_fd, compute_duration, compute_checksum)?;

    let storage_started = monotonic_now(output_fd, b"storage-clock-start")?;
    let storage_checksum = read_storage(root_block_fd, output_fd)?;
    let storage_finished = monotonic_now(output_fd, b"storage-clock-end")?;
    let storage_duration = elapsed_ns(storage_started, storage_finished).ok_or(Failure {
        output_fd,
        phase: b"storage-clock-range",
    })?;
    emit_storage(output_fd, storage_duration, storage_checksum)?;

    require_zero(close(root_block_fd), output_fd, b"close-root-block")?;
    Ok(output_fd)
}

fn configure_serial(output_fd: usize) -> Result<(), Failure> {
    let mut control_characters = [0_u8; 19];
    control_characters[6] = 1;
    let raw = Termios {
        input_flags: 0,
        output_flags: 0,
        control_flags: 0x08bf,
        local_flags: 0,
        line: 0,
        control_characters,
    };
    // SAFETY: `raw` is the Linux arm64 termios layout used by TCSETS and stays
    // readable for the duration of the ioctl.
    let result = unsafe {
        syscall6(
            SYS_IOCTL,
            output_fd,
            TCSETS,
            ptr::from_ref(&raw) as usize,
            0,
            0,
            0,
        )
    };
    require_zero(result, output_fd, b"configure-serial")
}

fn signal_boot_complete(output_fd: usize) -> Result<(), Failure> {
    let memory_fd = open(MEMORY, O_RDWR | O_SYNC).ok_or(Failure {
        output_fd,
        phase: b"open-boot-timer",
    })?;
    // SAFETY: the boot timer occupies one checked guest page at the fixed
    // production MMIO address and `memory_fd` is an open /dev/mem descriptor.
    let mapped = unsafe {
        syscall6(
            SYS_MMAP,
            0,
            GUEST_PAGE_SIZE,
            PROT_WRITE,
            MAP_SHARED,
            memory_fd,
            BOOT_TIMER_ADDRESS,
        )
    };
    if is_error(mapped) {
        return Err(Failure {
            output_fd,
            phase: b"map-boot-timer",
        });
    }
    let mapped_address = mapped as usize;
    // SAFETY: the successful mapping above covers at least one writable byte.
    unsafe {
        ptr::write_volatile(mapped_address as *mut u8, BOOT_TIMER_MAGIC);
    }
    // SAFETY: the mapped address and length are the exact successful mapping.
    let synced = unsafe {
        syscall6(
            SYS_MSYNC,
            mapped_address,
            GUEST_PAGE_SIZE,
            MS_ASYNC,
            0,
            0,
            0,
        )
    };
    require_zero(synced, output_fd, b"sync-boot-timer")?;
    // SAFETY: the mapped address and length are the exact successful mapping.
    let unmapped = unsafe { syscall6(SYS_MUNMAP, mapped_address, GUEST_PAGE_SIZE, 0, 0, 0, 0) };
    require_zero(unmapped, output_fd, b"unmap-boot-timer")?;
    require_zero(close(memory_fd), output_fd, b"close-boot-timer")
}

fn wait_for_release(output_fd: usize) -> Result<(), Failure> {
    let mut tokens = [0_u8; 2];
    loop {
        // SAFETY: `tokens` is writable for the requested two bytes and the
        // serial descriptor remains open for the workload lifetime.
        let result = unsafe {
            syscall6(
                SYS_READ,
                output_fd,
                tokens.as_mut_ptr() as usize,
                tokens.len(),
                0,
                0,
                0,
            )
        };
        if result == -EINTR {
            continue;
        }
        if result == 0 {
            return Err(Failure {
                output_fd,
                phase: b"release-eof",
            });
        }
        if result > 1 {
            return Err(Failure {
                output_fd,
                phase: b"release-extra",
            });
        }
        if result != 1 {
            return Err(Failure {
                output_fd,
                phase: b"release-read",
            });
        }
        if tokens[0] != RELEASE_TOKEN {
            return Err(Failure {
                output_fd,
                phase: b"release-byte",
            });
        }
        return Ok(());
    }
}

fn compute_workload() -> u64 {
    let mut checksum = 0x6a09_e667_f3bc_c909_u64;
    for operation in 0..COMPUTE_OPERATIONS {
        checksum ^= operation.wrapping_mul(0x9e37_79b1_85eb_ca87);
        checksum = checksum.rotate_left(17);
        checksum = checksum.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        checksum ^= checksum >> 29;
        checksum = black_box(checksum);
    }
    checksum
}

fn read_storage(root_block_fd: usize, output_fd: usize) -> Result<u64, Failure> {
    let mut buffer = [0_u8; STORAGE_BLOCK_BYTES];
    let mut remaining = STORAGE_BYTES;
    let mut checksum = FNV_OFFSET_BASIS;
    while remaining != 0 {
        // SAFETY: `buffer` is writable for exactly the requested block and the
        // checked root block descriptor remains open.
        let result = unsafe {
            syscall6(
                SYS_READ,
                root_block_fd,
                buffer.as_mut_ptr() as usize,
                STORAGE_BLOCK_BYTES,
                0,
                0,
                0,
            )
        };
        if result == -EINTR {
            continue;
        }
        if result != STORAGE_BLOCK_BYTES as isize {
            return Err(Failure {
                output_fd,
                phase: b"storage-short-read",
            });
        }
        for byte in buffer {
            checksum ^= u64::from(byte);
            checksum = checksum.wrapping_mul(FNV_PRIME);
        }
        remaining -= STORAGE_BLOCK_BYTES as u64;
    }
    Ok(checksum)
}

fn monotonic_now(output_fd: usize, phase: &'static [u8]) -> Result<Timespec, Failure> {
    let mut time = Timespec {
        seconds: 0,
        nanoseconds: 0,
    };
    // SAFETY: `time` is writable for one Linux aarch64 timespec.
    let result = unsafe {
        syscall6(
            SYS_CLOCK_GETTIME,
            CLOCK_MONOTONIC,
            ptr::from_mut(&mut time) as usize,
            0,
            0,
            0,
            0,
        )
    };
    require_zero(result, output_fd, phase)?;
    if time.seconds < 0 || !(0..1_000_000_000).contains(&time.nanoseconds) {
        return Err(Failure { output_fd, phase });
    }
    Ok(time)
}

fn elapsed_ns(start: Timespec, end: Timespec) -> Option<u64> {
    if end.seconds < start.seconds {
        return None;
    }
    let mut seconds = end.seconds - start.seconds;
    let nanoseconds = if end.nanoseconds < start.nanoseconds {
        seconds = seconds.checked_sub(1)?;
        end.nanoseconds + 1_000_000_000 - start.nanoseconds
    } else {
        end.nanoseconds - start.nanoseconds
    };
    let seconds = u64::try_from(seconds).ok()?;
    let nanoseconds = u64::try_from(nanoseconds).ok()?;
    seconds.checked_mul(1_000_000_000)?.checked_add(nanoseconds)
}

fn emit_compute(output_fd: usize, duration_ns: u64, checksum: u64) -> Result<(), Failure> {
    let mut line = Line::new();
    if !line.append(b"BANGBANG_SPEC_COMPUTE duration_ns=")
        || !line.append_u64(duration_ns)
        || !line.append(b" operations=")
        || !line.append_u64(COMPUTE_OPERATIONS)
        || !line.append(b" checksum=")
        || !line.append_u64(checksum)
        || !line.append(b"\n")
        || !write_all(output_fd, line.as_bytes())
    {
        return Err(Failure {
            output_fd,
            phase: b"compute-write",
        });
    }
    Ok(())
}

fn emit_storage(output_fd: usize, duration_ns: u64, checksum: u64) -> Result<(), Failure> {
    let mut line = Line::new();
    if !line.append(b"BANGBANG_SPEC_STORAGE duration_ns=")
        || !line.append_u64(duration_ns)
        || !line.append(b" bytes=")
        || !line.append_u64(STORAGE_BYTES)
        || !line.append(b" block_bytes=")
        || !line.append_u64(STORAGE_BLOCK_BYTES as u64)
        || !line.append(b" checksum=")
        || !line.append_u64(checksum)
        || !line.append(b"\n")
        || !write_all(output_fd, line.as_bytes())
    {
        return Err(Failure {
            output_fd,
            phase: b"storage-write",
        });
    }
    Ok(())
}

fn open(path: &'static [u8], flags: usize) -> Option<usize> {
    // SAFETY: `path` is one of the static NUL-terminated device paths above.
    let result = unsafe { syscall6(SYS_OPENAT, AT_FDCWD, path.as_ptr() as usize, flags, 0, 0, 0) };
    (!is_error(result)).then_some(result as usize)
}

fn close(fd: usize) -> isize {
    // SAFETY: the syscall accepts any descriptor value and returns an error for
    // an invalid one.
    unsafe { syscall6(SYS_CLOSE, fd, 0, 0, 0, 0, 0) }
}

fn require_zero(result: isize, output_fd: usize, phase: &'static [u8]) -> Result<(), Failure> {
    if result == 0 {
        Ok(())
    } else {
        Err(Failure { output_fd, phase })
    }
}

fn is_error(result: isize) -> bool {
    (-4095..0).contains(&result)
}

fn write_all(fd: usize, bytes: &[u8]) -> bool {
    let mut written = 0;
    while written < bytes.len() {
        let request = (bytes.len() - written).min(SERIAL_WRITE_CHUNK_BYTES);
        // SAFETY: `written` and `request` select a readable range within
        // `bytes`, and the syscall accepts any descriptor value.
        let result = unsafe {
            syscall6(
                SYS_WRITE,
                fd,
                bytes.as_ptr().add(written) as usize,
                request,
                0,
                0,
                0,
            )
        };
        if result == -EINTR {
            continue;
        }
        if result <= 0 || result as usize > request {
            return false;
        }
        written += result as usize;
    }
    true
}

fn fail(failure: Failure) -> ! {
    let mut line = Line::new();
    if line.append(FAILURE_PREFIX) && line.append(failure.phase) && line.append(b"\n") {
        let _ = write_all(failure.output_fd, line.as_bytes());
    }
    power_off(failure.output_fd, 1)
}

fn power_off(output_fd: usize, status: usize) -> ! {
    // SAFETY: these are the Linux reboot magic constants and the power-off
    // command; a PID-1 workload has the required guest privilege.
    unsafe {
        let _ = syscall6(
            SYS_REBOOT,
            LINUX_REBOOT_MAGIC1,
            LINUX_REBOOT_MAGIC2,
            LINUX_REBOOT_CMD_POWER_OFF,
            0,
            0,
            0,
        );
    }
    if status == 0 {
        let _ = write_all(
            output_fd,
            b"BANGBANG_SPEC_WORKLOAD_FAIL phase=poweroff\n",
        );
        exit(1)
    }
    exit(status)
}

fn exit(status: usize) -> ! {
    // SAFETY: the Linux aarch64 exit syscall takes only the status and never
    // returns.
    unsafe {
        asm!(
            "svc 0",
            in("x8") SYS_EXIT,
            in("x0") status,
            options(noreturn, nostack),
        );
    }
}

unsafe fn syscall6(
    number: usize,
    argument0: usize,
    argument1: usize,
    argument2: usize,
    argument3: usize,
    argument4: usize,
    argument5: usize,
) -> isize {
    let result: usize;
    // SAFETY: callers document every pointer and descriptor supplied to the
    // Linux aarch64 syscall ABI. Linux returns its scalar result in x0.
    unsafe {
        asm!(
            "svc 0",
            in("x8") number,
            inlateout("x0") argument0 => result,
            in("x1") argument1,
            in("x2") argument2,
            in("x3") argument3,
            in("x4") argument4,
            in("x5") argument5,
            options(nostack),
        );
    }
    result as isize
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

// The static no-std link still references the personality symbol through a
// core cold path. `panic=abort` guarantees that unwinding never calls it.
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() {}
