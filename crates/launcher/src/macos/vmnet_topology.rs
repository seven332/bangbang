use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::time::Instant;

use bangbang_session::vmnet_topology::{
    VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE, VMNET_TOPOLOGY_FD,
    VMNET_TOPOLOGY_PROVIDER_FD, VmnetTopologyContext, VmnetTopologyMessage, VmnetTopologyTerminal,
    VmnetTopologyTransport, VmnetTopologyTransportError,
};
use bangbang_session::{ObjectIdentity, SessionId, VmnetAuthority};

use crate::grant_manifest::InheritedVmnetProvider;
use crate::{BundleLayout, LauncherError};

const TOPOLOGY_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const CF_USER_TEXT_ENCODING_ENV_KEY: &str = "__CF_USER_TEXT_ENCODING";

pub(crate) struct ChildBootstrap {
    context: VmnetTopologyContext,
    topology: VmnetTopologyTransport,
    provider: Option<UnixStream>,
    session: Option<SessionId>,
    ready: bool,
}

impl std::fmt::Debug for ChildBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VmnetTopologyChildBootstrap(<redacted>)")
    }
}

pub(crate) fn child_bootstrap() -> Result<Option<ChildBootstrap>, LauncherError> {
    let Some(marker) = env::var_os(VMNET_TOPOLOGY_ENV_KEY) else {
        return Ok(None);
    };
    // SAFETY: Credential getters have no pointer or ownership contract.
    let (uid, effective_uid) = unsafe { (libc::getuid(), libc::geteuid()) };
    let exact_environment =
        uid != 0 && uid == effective_uid && validate_private_environment(env::vars_os(), uid);
    // SAFETY: This is the first launcher-library boundary before application
    // threads are created. The transition supplied only the private marker;
    // Darwin may add the UID-bound CoreFoundation entry during exec.
    unsafe {
        env::remove_var(VMNET_TOPOLOGY_ENV_KEY);
        env::remove_var(CF_USER_TEXT_ENCODING_ENV_KEY);
    }
    if marker != VMNET_TOPOLOGY_ENV_VALUE || !exact_environment {
        return Err(LauncherError::VmnetTopology);
    }

    bangbang_session::macos::set_cloexec(VMNET_TOPOLOGY_FD)
        .map_err(|_| LauncherError::VmnetTopology)?;
    bangbang_session::macos::set_cloexec(VMNET_TOPOLOGY_PROVIDER_FD)
        .map_err(|_| LauncherError::VmnetTopology)?;
    // SAFETY: The exact private exec contract transfers each fixed descriptor
    // to this first-boundary adoption exactly once.
    let topology = UnixStream::from(unsafe { OwnedFd::from_raw_fd(VMNET_TOPOLOGY_FD) });
    // SAFETY: This is the distinct provider descriptor from the same exact
    // private exec contract and is likewise adopted exactly once.
    let provider = UnixStream::from(unsafe { OwnedFd::from_raw_fd(VMNET_TOPOLOGY_PROVIDER_FD) });
    let topology_peer = bangbang_session::macos::peer_identity(topology.as_raw_fd())
        .map_err(|_| LauncherError::VmnetTopology)?;
    let provider_peer = bangbang_session::macos::peer_identity(provider.as_raw_fd())
        .map_err(|_| LauncherError::VmnetTopology)?;
    if topology_peer != provider_peer
        || topology_peer.uid != 0
        || topology_peer.gid != 0
        || topology_peer.pid <= 0
    {
        return Err(LauncherError::VmnetTopology);
    }
    set_nonblocking(provider.as_raw_fd())?;
    let mut topology =
        VmnetTopologyTransport::new(topology, TOPOLOGY_IO_TIMEOUT).map_err(map_transport_error)?;
    let context = match topology.receive().map_err(map_transport_error)? {
        VmnetTopologyMessage::OuterStart(context) => context,
        _ => return Err(LauncherError::VmnetTopology),
    };
    if context.launcher_pid() != std::process::id() || context.target().uid() == 0 {
        return Err(LauncherError::VmnetTopology);
    }
    topology
        .send(VmnetTopologyMessage::OuterHello(context))
        .map_err(map_transport_error)?;
    match topology.receive().map_err(map_transport_error)? {
        VmnetTopologyMessage::Proceed(received) if received == context => {}
        _ => return Err(LauncherError::VmnetTopology),
    }
    Ok(Some(ChildBootstrap {
        context,
        topology,
        provider: Some(provider),
        session: None,
        ready: false,
    }))
}

fn validate_private_environment<I>(variables: I, current_uid: u32) -> bool
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut marker = false;
    let mut text_encoding = false;
    for (key, value) in variables {
        if key == OsStr::new(VMNET_TOPOLOGY_ENV_KEY) {
            if marker || value != OsStr::new(VMNET_TOPOLOGY_ENV_VALUE) {
                return false;
            }
            marker = true;
        } else if key == OsStr::new(CF_USER_TEXT_ENCODING_ENV_KEY) {
            if text_encoding || !valid_cf_user_text_encoding(&value, current_uid) {
                return false;
            }
            text_encoding = true;
        } else {
            return false;
        }
    }
    marker
}

fn valid_cf_user_text_encoding(value: &OsStr, current_uid: u32) -> bool {
    let Some(value) = value.to_str() else {
        return false;
    };
    let mut components = value.split(':');
    let values = [
        components.next().and_then(parse_canonical_hex),
        components.next().and_then(parse_canonical_hex),
        components.next().and_then(parse_canonical_hex),
    ];
    components.next().is_none()
        && values[0] == Some(current_uid)
        && values[1..].iter().all(Option::is_some)
}

fn parse_canonical_hex(value: &str) -> Option<u32> {
    let digits = value.strip_prefix("0x")?;
    if digits.is_empty()
        || digits.len() > 8
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

impl ChildBootstrap {
    pub(crate) const fn daemonized(&self) -> bool {
        self.context.mode().is_daemon()
    }

    pub(crate) fn validate_request(
        &self,
        target_uid: u32,
        target_gid: u32,
        daemonized: bool,
    ) -> Result<(), LauncherError> {
        if (target_uid, target_gid) == (self.context.target().uid(), self.context.target().gid())
            && daemonized == self.daemonized()
        {
            Ok(())
        } else {
            Err(LauncherError::InvalidLaunchPolicy)
        }
    }

    pub(crate) fn take_provider(
        &mut self,
        layout: &BundleLayout,
    ) -> Result<InheritedVmnetProvider, LauncherError> {
        let metadata = fs::symlink_metadata(layout.vmnet_provider_executable())
            .map_err(|_| LauncherError::InvalidBundleEntry)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.nlink() != 1
            || metadata.mode() & 0o7111 == 0
            || metadata.mode() & 0o7022 != 0
        {
            return Err(LauncherError::InvalidBundleEntry);
        }
        let source_identity = ObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if source_identity.device == 0 || source_identity.inode == 0 {
            return Err(LauncherError::InvalidBundleEntry);
        }
        let peer = bangbang_session::macos::peer_identity(
            self.provider
                .as_ref()
                .ok_or(LauncherError::VmnetTopology)?
                .as_raw_fd(),
        )
        .map_err(|_| LauncherError::VmnetTopology)?;
        if peer.uid != 0 || peer.gid != 0 || peer.pid <= 0 {
            return Err(LauncherError::VmnetTopology);
        }
        InheritedVmnetProvider::new(
            self.provider.take().ok_or(LauncherError::VmnetTopology)?,
            source_identity,
            u32::try_from(peer.pid).map_err(|_| LauncherError::VmnetTopology)?,
        )
    }

    pub(crate) fn activate(
        &mut self,
        session: SessionId,
        authority: VmnetAuthority,
    ) -> Result<(), LauncherError> {
        if self.session.is_some() || authority.is_denied() {
            return Err(LauncherError::VmnetTopology);
        }
        self.topology
            .send(VmnetTopologyMessage::Activate {
                context: self.context,
                session,
                authority,
            })
            .map_err(map_transport_error)?;
        match self.topology.receive().map_err(map_transport_error)? {
            VmnetTopologyMessage::BrokerReady {
                context,
                session: received,
            } if context == self.context && received == session => {
                self.session = Some(session);
                Ok(())
            }
            _ => Err(LauncherError::VmnetTopology),
        }
    }

    pub(crate) fn ready(&mut self) -> Result<(), LauncherError> {
        if self.ready {
            return Err(LauncherError::VmnetTopology);
        }
        let session = self.session.ok_or(LauncherError::VmnetTopology)?;
        self.topology
            .send(VmnetTopologyMessage::LauncherReady {
                context: self.context,
                session,
            })
            .map_err(map_transport_error)?;
        match self.topology.receive().map_err(map_transport_error)? {
            VmnetTopologyMessage::ReadyAck {
                context,
                session: received,
            } if context == self.context && received == session => {
                self.ready = true;
                Ok(())
            }
            _ => Err(LauncherError::VmnetTopology),
        }
    }

    pub(crate) fn finish(
        &mut self,
        result: Result<u8, &LauncherError>,
    ) -> Result<(), LauncherError> {
        let session = self.session.ok_or(LauncherError::VmnetTopology)?;
        if !self.ready {
            self.topology.shutdown();
            return Ok(());
        }
        let result = match result {
            Ok(0) => VmnetTopologyTerminal::Complete,
            Ok(_) | Err(_) => VmnetTopologyTerminal::Launcher,
        };
        self.topology
            .send(VmnetTopologyMessage::Terminal {
                context: self.context,
                session,
                result,
            })
            .map_err(map_transport_error)?;
        match self.topology.receive().map_err(map_transport_error)? {
            VmnetTopologyMessage::TerminalAck {
                context,
                session: received,
                result: acknowledged,
            } if context == self.context && received == session && acknowledged == result => Ok(()),
            _ => Err(LauncherError::VmnetTopology),
        }
    }
}

impl super::daemon::SessionNotifier for ChildBootstrap {
    fn as_raw_fd(&self) -> Result<libc::c_int, LauncherError> {
        Ok(self.topology.as_raw_fd())
    }

    fn deadline(&self) -> Option<Instant> {
        None
    }

    fn is_awaiting_ready(&self) -> bool {
        self.session.is_some() && !self.ready
    }

    fn notify_ready(&mut self, _supervisor_pid: libc::pid_t) -> Result<(), LauncherError> {
        self.ready()
    }

    fn drain(&mut self) -> Result<super::daemon::NotifierEvent, LauncherError> {
        let _ = self.topology.receive();
        Ok(super::daemon::NotifierEvent::ParentLost)
    }

    fn close_transport(&mut self) {
        self.topology.shutdown();
    }
}

fn set_nonblocking(descriptor: libc::c_int) -> Result<(), LauncherError> {
    // SAFETY: `F_GETFL` inspects the live owned descriptor only.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(LauncherError::VmnetTopology);
    }
    // SAFETY: `F_SETFL` updates only the status flags on this live provider
    // endpoint before it is admitted to grant preparation.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(LauncherError::VmnetTopology);
    }
    Ok(())
}

fn map_transport_error(_: VmnetTopologyTransportError) -> LauncherError {
    LauncherError::VmnetTopology
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(entries: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        entries
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect()
    }

    #[test]
    fn private_environment_accepts_only_marker_and_uid_bound_darwin_entry() {
        assert!(validate_private_environment(
            environment(&[(VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE)]),
            501
        ));
        assert!(validate_private_environment(
            environment(&[
                (CF_USER_TEXT_ENCODING_ENV_KEY, "0x1F5:0x0:0x0"),
                (VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE),
            ]),
            501
        ));

        for hostile in [
            environment(&[(CF_USER_TEXT_ENCODING_ENV_KEY, "0x1F5:0x0:0x0")]),
            environment(&[
                (VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE),
                ("PATH", "/private"),
            ]),
            environment(&[
                (VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE),
                (CF_USER_TEXT_ENCODING_ENV_KEY, "0x1F6:0x0:0x0"),
            ]),
            environment(&[
                (VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE),
                (CF_USER_TEXT_ENCODING_ENV_KEY, "0x01F5:0x0:0x0"),
            ]),
            environment(&[
                (VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE),
                (VMNET_TOPOLOGY_ENV_KEY, VMNET_TOPOLOGY_ENV_VALUE),
            ]),
        ] {
            assert!(!validate_private_environment(hostile, 501));
        }
    }
}
