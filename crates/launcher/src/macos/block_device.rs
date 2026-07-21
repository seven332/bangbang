//! Narrow public macOS block-device controls used by retained grant descriptors.

use std::io;
use std::os::fd::RawFd;

use bangbang_session::BlockDeviceGrant;

const DARWIN_IOC_OUT: libc::c_ulong = 0x4000_0000;
const DARWIN_IOC_VOID: libc::c_ulong = 0x2000_0000;
const DARWIN_IOCPARM_MASK: libc::c_ulong = 0x1fff;

const fn darwin_ioctl_request(
    direction: libc::c_ulong,
    group: u8,
    number: u8,
    len: usize,
) -> libc::c_ulong {
    direction
        | (((len as libc::c_ulong) & DARWIN_IOCPARM_MASK) << 16)
        | ((group as libc::c_ulong) << 8)
        | number as libc::c_ulong
}

const DKIOCGETBLOCKSIZE: libc::c_ulong =
    darwin_ioctl_request(DARWIN_IOC_OUT, b'd', 24, std::mem::size_of::<u32>());
const DKIOCGETBLOCKCOUNT: libc::c_ulong =
    darwin_ioctl_request(DARWIN_IOC_OUT, b'd', 25, std::mem::size_of::<u64>());
const DKIOCSYNCHRONIZECACHE: libc::c_ulong = darwin_ioctl_request(DARWIN_IOC_VOID, b'd', 22, 0);

pub(crate) fn inspect(descriptor: RawFd, target_device: u64) -> io::Result<BlockDeviceGrant> {
    let mut logical_block_size = 0_u32;
    // SAFETY: DKIOCGETBLOCKSIZE writes one u32 to the valid out pointer and
    // inspects only the live borrowed descriptor for this synchronous call.
    if unsafe { libc::ioctl(descriptor, DKIOCGETBLOCKSIZE, &raw mut logical_block_size) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut block_count = 0_u64;
    // SAFETY: DKIOCGETBLOCKCOUNT writes one u64 to the valid out pointer and
    // inspects only the same live borrowed descriptor.
    if unsafe { libc::ioctl(descriptor, DKIOCGETBLOCKCOUNT, &raw mut block_count) } < 0 {
        return Err(io::Error::last_os_error());
    }
    BlockDeviceGrant::new(target_device, logical_block_size, block_count)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
}

pub(crate) fn synchronize_cache(descriptor: RawFd) -> io::Result<()> {
    // SAFETY: DKIOCSYNCHRONIZECACHE has no pointer payload and operates only on
    // the live borrowed descriptor for this synchronous call.
    if unsafe { libc::ioctl(descriptor, DKIOCSYNCHRONIZECACHE) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_sdk_request_numbers_remain_exact() {
        assert_eq!(DKIOCGETBLOCKSIZE, 0x4004_6418);
        assert_eq!(DKIOCGETBLOCKCOUNT, 0x4008_6419);
        assert_eq!(DKIOCSYNCHRONIZECACHE, 0x2000_6416);
    }
}
