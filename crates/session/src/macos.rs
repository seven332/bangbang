//! macOS peer identity and private runtime namespace support.

use std::io;
use std::os::fd::RawFd;

pub mod runtime;

/// Kernel-authenticated identity of a connected local-socket peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Effective peer user ID.
    pub uid: libc::uid_t,
    /// Effective peer group ID.
    pub gid: libc::gid_t,
    /// Live peer process ID.
    pub pid: libc::pid_t,
}

/// Reads effective credentials and live PID from a connected Unix stream.
pub fn peer_identity(fd: RawFd) -> io::Result<PeerIdentity> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: `uid` and `gid` are writable for the synchronous credential query;
    // `fd` remains owned by the caller.
    if unsafe { libc::getpeereid(fd, &raw mut uid, &raw mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let mut pid = 0;
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::pid_t>())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `pid` points to `length` writable bytes of the requested integer
    // option; the call does not retain either pointer.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &raw mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(length).ok() != Some(std::mem::size_of::<libc::pid_t>()) || pid <= 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(PeerIdentity { uid, gid, pid })
}

/// Verifies the exact effective identity and PID expected by one side.
pub fn verify_peer(fd: RawFd, expected_pid: libc::pid_t) -> io::Result<PeerIdentity> {
    let peer = peer_identity(fd)?;
    // SAFETY: These process identity calls have no pointer or ownership contract.
    let expected_uid = unsafe { libc::geteuid() };
    // SAFETY: These process identity calls have no pointer or ownership contract.
    let expected_gid = unsafe { libc::getegid() };
    if peer.uid != expected_uid || peer.gid != expected_gid || peer.pid != expected_pid {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(peer)
}

/// Marks a taken bootstrap descriptor close-on-exec immediately.
pub fn set_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` remains owned by the caller and `F_GETFD` has no pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` remains owned by the caller; this only updates descriptor flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn socketpair_reports_current_effective_identity_and_process() {
        let (left, _right) = UnixStream::pair().expect("socketpair should open");
        let peer = peer_identity(left.as_raw_fd()).expect("peer identity should read");
        // SAFETY: Identity calls have no pointer or ownership contract.
        assert_eq!(peer.uid, unsafe { libc::geteuid() });
        // SAFETY: Identity calls have no pointer or ownership contract.
        assert_eq!(peer.gid, unsafe { libc::getegid() });
        // A same-process socketpair reports the current process on Darwin.
        // SAFETY: `getpid` has no pointer or ownership contract.
        assert_eq!(peer.pid, unsafe { libc::getpid() });
    }
}
