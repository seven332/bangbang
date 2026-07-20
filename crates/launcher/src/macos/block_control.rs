//! Launcher-side controller for exact retained block-special grants.

use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixDatagram;

use bangbang_session::macos::block_control::{
    BlockControlError, BlockControlMessage, BlockControlOperation, BlockControlTarget,
    receive_block_control_message, send_block_control_message,
};
use bangbang_session::macos::{normalized_block_status_flags, verify_peer_pid};
use bangbang_session::{BlockDeviceGrant, GrantAccess, LauncherState, ObjectIdentity, SessionId};

use super::block_device;
use crate::LauncherError;
use crate::grant_manifest::{BlockDriveAnchor, PreparedGrantBatch};

/// Session-bound serial controller for worker block-control requests.
pub(crate) struct LauncherBlockControlBroker {
    session: SessionId,
    next_sequence: u64,
}

impl std::fmt::Debug for LauncherBlockControlBroker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LauncherBlockControlBroker")
            .field("session", &"<redacted>")
            .field("next_sequence", &"<redacted>")
            .finish()
    }
}

impl LauncherBlockControlBroker {
    pub(crate) const fn new(session: SessionId) -> Self {
        Self {
            session,
            next_sequence: 1,
        }
    }

    pub(crate) fn drain(
        &mut self,
        socket: &UnixDatagram,
        worker_pid: libc::pid_t,
        lifecycle_state: LauncherState,
        lifecycle_cancelled: bool,
        grants: &PreparedGrantBatch,
    ) -> Result<(), LauncherError> {
        loop {
            let message = match receive_block_control_message(socket) {
                Ok(message) => message,
                Err(BlockControlError::Io(io::ErrorKind::WouldBlock)) => return Ok(()),
                Err(_) => return Err(LauncherError::BlockControlBroker),
            };
            verify_peer_pid(socket.as_raw_fd(), worker_pid)
                .map_err(|_| LauncherError::BlockControlBroker)?;
            if message.session() != self.session
                || message.sequence() != self.next_sequence
                || lifecycle_cancelled
                || !matches!(
                    lifecycle_state,
                    LauncherState::Starting | LauncherState::Ready(_)
                )
                || !matches!(
                    &message,
                    BlockControlMessage::Inspect { .. }
                        | BlockControlMessage::SynchronizeCache { .. }
                )
            {
                return Err(LauncherError::BlockControlBroker);
            }
            let sequence = self.next_sequence;
            self.next_sequence = self
                .next_sequence
                .checked_add(1)
                .ok_or(LauncherError::BlockControlBroker)?;
            let anchor = grants
                .block_drive_anchor(message.target().grant_id())
                .ok_or(LauncherError::BlockControlBroker)?;
            if !target_matches_anchor(message.target(), anchor) {
                return Err(LauncherError::BlockControlBroker);
            }

            match message {
                BlockControlMessage::Inspect { target, .. } => match inspect_anchor(anchor) {
                    Ok(observed) => send(
                        socket,
                        &BlockControlMessage::Inspected {
                            session: self.session,
                            sequence,
                            target,
                            observed,
                        },
                    )?,
                    Err(ControlFailure::Endpoint(kind)) => send(
                        socket,
                        &BlockControlMessage::Failed {
                            session: self.session,
                            sequence,
                            target,
                            operation: BlockControlOperation::Inspect,
                            kind,
                        },
                    )?,
                    Err(ControlFailure::Rejected) => {
                        return Err(LauncherError::BlockControlBroker);
                    }
                },
                BlockControlMessage::SynchronizeCache { target, .. } => {
                    match synchronize_anchor(anchor) {
                        Ok(()) => send(
                            socket,
                            &BlockControlMessage::Synchronized {
                                session: self.session,
                                sequence,
                                target,
                            },
                        )?,
                        Err(ControlFailure::Endpoint(kind)) => send(
                            socket,
                            &BlockControlMessage::Failed {
                                session: self.session,
                                sequence,
                                target,
                                operation: BlockControlOperation::SynchronizeCache,
                                kind,
                            },
                        )?,
                        Err(ControlFailure::Rejected) => {
                            return Err(LauncherError::BlockControlBroker);
                        }
                    }
                }
                BlockControlMessage::Inspected { .. }
                | BlockControlMessage::Synchronized { .. }
                | BlockControlMessage::Failed { .. } => {
                    return Err(LauncherError::BlockControlBroker);
                }
            }
        }
    }
}

fn send(socket: &UnixDatagram, message: &BlockControlMessage) -> Result<(), LauncherError> {
    send_block_control_message(socket, message).map_err(|_| LauncherError::BlockControlBroker)
}

fn target_matches_anchor(target: &BlockControlTarget, anchor: BlockDriveAnchor) -> bool {
    target.access() == anchor.access()
        && target.identity() == anchor.identity()
        && target.status_flags() == anchor.status_flags()
        && target.block_device() == anchor.block_device()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlFailure {
    Endpoint(io::ErrorKind),
    Rejected,
}

fn inspect_anchor(anchor: BlockDriveAnchor) -> Result<BlockDeviceGrant, ControlFailure> {
    validate_descriptor(anchor)?;
    block_device::inspect(anchor.descriptor(), anchor.block_device().target_device())
        .map_err(|error| ControlFailure::Endpoint(error.kind()))
}

fn synchronize_anchor(anchor: BlockDriveAnchor) -> Result<(), ControlFailure> {
    if inspect_anchor(anchor)? != anchor.block_device() {
        return Err(ControlFailure::Rejected);
    }
    block_device::synchronize_cache(anchor.descriptor())
        .map_err(|error| ControlFailure::Endpoint(error.kind()))?;
    match inspect_anchor(anchor) {
        Ok(observed) if observed == anchor.block_device() => Ok(()),
        Ok(_) | Err(_) => Err(ControlFailure::Rejected),
    }
}

fn validate_descriptor(anchor: BlockDriveAnchor) -> Result<(), ControlFailure> {
    // SAFETY: F_GETFD and F_GETFL inspect the live retained descriptor.
    let descriptor_flags = unsafe { libc::fcntl(anchor.descriptor(), libc::F_GETFD) };
    // SAFETY: F_GETFL inspects status on the same live descriptor.
    let status_flags = unsafe { libc::fcntl(anchor.descriptor(), libc::F_GETFL) };
    if descriptor_flags < 0 || status_flags < 0 {
        return Err(ControlFailure::Endpoint(io::Error::last_os_error().kind()));
    }
    if descriptor_flags & libc::FD_CLOEXEC == 0
        || normalized_block_status_flags(status_flags) != Some(anchor.status_flags())
        || !access_matches(status_flags, anchor.access())
    {
        return Err(ControlFailure::Rejected);
    }
    let stat = descriptor_stat(anchor.descriptor())?;
    let identity = ObjectIdentity {
        device: normalized_device(stat.st_dev),
        inode: stat.st_ino,
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFBLK
        || identity != anchor.identity()
        || normalized_device(stat.st_rdev) != anchor.block_device().target_device()
    {
        return Err(ControlFailure::Rejected);
    }
    Ok(())
}

fn descriptor_stat(descriptor: RawFd) -> Result<libc::stat, ControlFailure> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat points to writable storage and descriptor remains live.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return Err(ControlFailure::Endpoint(io::Error::last_os_error().kind()));
    }
    // SAFETY: Successful fstat initialized the complete structure.
    Ok(unsafe { stat.assume_init() })
}

fn access_matches(flags: libc::c_int, access: GrantAccess) -> bool {
    let actual = flags & libc::O_ACCMODE;
    match access {
        GrantAccess::ReadOnly => actual == libc::O_RDONLY,
        GrantAccess::ReadWrite => actual == libc::O_RDWR,
        GrantAccess::WriteOnly | GrantAccess::CreateChildren | GrantAccess::ConnectChildren => {
            false
        }
    }
}

fn normalized_device(device: libc::dev_t) -> u64 {
    u64::from(u32::from_ne_bytes(device.to_ne_bytes()))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    use bangbang_session::macos::block_control::{BlockControlMessage, send_block_control_message};

    use super::*;

    fn session() -> SessionId {
        SessionId::from_bytes([81; 32])
    }

    fn target() -> BlockControlTarget {
        BlockControlTarget::new(
            bangbang_session::GrantId::parse("block-drive").expect("grant should parse"),
            GrantAccess::ReadWrite,
            ObjectIdentity {
                device: 82,
                inode: 83,
            },
            u32::try_from(libc::O_RDWR | libc::O_NONBLOCK).expect("status flags should fit"),
            BlockDeviceGrant::new(84, 512, 32).expect("block tuple should validate"),
        )
        .expect("target should validate")
    }

    fn assert_drain_rejects(
        broker_session: SessionId,
        message: BlockControlMessage,
        worker_pid: libc::pid_t,
        lifecycle_state: LauncherState,
        cancelled: bool,
    ) {
        let (worker, launcher) = UnixDatagram::pair().expect("block broker pair should open");
        launcher
            .set_nonblocking(true)
            .expect("launcher endpoint should become nonblocking");
        send_block_control_message(&worker, &message).expect("test request should send");
        let mut broker = LauncherBlockControlBroker::new(broker_session);
        assert_eq!(
            broker.drain(
                &launcher,
                worker_pid,
                lifecycle_state,
                cancelled,
                &PreparedGrantBatch::empty_for_test(),
            ),
            Err(LauncherError::BlockControlBroker)
        );
    }

    #[test]
    fn broker_state_and_error_are_redacted() {
        let mut broker = LauncherBlockControlBroker::new(session());
        broker.next_sequence = 918;
        let debug = format!("{broker:?}");
        assert!(!debug.contains("5151"));
        assert!(!debug.contains("918"));
        assert_eq!(
            LauncherError::BlockControlBroker.to_string(),
            "private block-control broker failed"
        );
    }

    #[test]
    fn drain_rejects_wrong_peer_session_sequence_phase_cancellation_and_operation() {
        // SAFETY: The connected socketpair peer is the current test process.
        let pid = unsafe { libc::getpid() };
        let request = |message_session, sequence| BlockControlMessage::Inspect {
            session: message_session,
            sequence,
            target: target(),
        };
        assert_drain_rejects(
            session(),
            request(SessionId::from_bytes([82; 32]), 1),
            pid,
            LauncherState::Starting,
            false,
        );
        assert_drain_rejects(
            session(),
            request(session(), 2),
            pid,
            LauncherState::Starting,
            false,
        );
        assert_drain_rejects(
            session(),
            request(session(), 1),
            pid,
            LauncherState::AwaitHello,
            false,
        );
        assert_drain_rejects(
            session(),
            request(session(), 1),
            pid,
            LauncherState::Starting,
            true,
        );
        assert_drain_rejects(
            session(),
            request(session(), 1),
            pid.checked_add(1).expect("test PID should fit"),
            LauncherState::Starting,
            false,
        );
        assert_drain_rejects(
            session(),
            BlockControlMessage::Synchronized {
                session: session(),
                sequence: 1,
                target: target(),
            },
            pid,
            LauncherState::Starting,
            false,
        );
        assert_drain_rejects(
            session(),
            request(session(), 1),
            pid,
            LauncherState::Ready(bangbang_session::Readiness::NoApi),
            false,
        );
    }

    #[test]
    fn target_match_requires_every_immutable_anchor_field() {
        let file = File::open("/dev/null").expect("descriptor fixture should open");
        let target = target();
        let anchor = BlockDriveAnchor::for_test(
            file.as_raw_fd(),
            target.access(),
            target.identity(),
            target.status_flags(),
            target.block_device(),
        );
        assert!(target_matches_anchor(&target, anchor));

        let different_access = BlockControlTarget::new(
            target.grant_id().clone(),
            GrantAccess::ReadOnly,
            target.identity(),
            u32::try_from(libc::O_RDONLY | libc::O_NONBLOCK).expect("status should fit"),
            target.block_device(),
        )
        .expect("read-only target should validate");
        assert!(!target_matches_anchor(&different_access, anchor));
        let different_identity = BlockControlTarget::new(
            target.grant_id().clone(),
            target.access(),
            ObjectIdentity {
                device: target.identity().device,
                inode: target.identity().inode + 1,
            },
            target.status_flags(),
            target.block_device(),
        )
        .expect("identity variant should validate");
        assert!(!target_matches_anchor(&different_identity, anchor));
        let different_status = BlockControlTarget::new(
            target.grant_id().clone(),
            target.access(),
            target.identity(),
            target.status_flags() ^ u32::try_from(libc::O_NONBLOCK).expect("flag should fit"),
            target.block_device(),
        )
        .expect("status variant should validate");
        assert!(!target_matches_anchor(&different_status, anchor));
        let different_geometry = BlockControlTarget::new(
            target.grant_id().clone(),
            target.access(),
            target.identity(),
            target.status_flags(),
            BlockDeviceGrant::new(
                target.block_device().target_device(),
                512,
                target.block_device().block_count() - 1,
            )
            .expect("geometry variant should validate"),
        )
        .expect("geometry target should validate");
        assert!(!target_matches_anchor(&different_geometry, anchor));
        assert!(format!("{anchor:?}").contains("<redacted>"));
    }
}
