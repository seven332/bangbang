//! Backend-neutral vsock configuration model.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque, hash_map::Entry};
use std::fmt;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use crate::interrupt::DeviceInterruptKind;
use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryRange,
};
use crate::mmio::{
    MmioAccessBytes, MmioAccessBytesError, MmioBusError, MmioDispatchError, MmioDispatcher,
    MmioHandlerError, MmioRegion, MmioRegionId,
};
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError,
    VirtioMmioDeviceActivationHandler, VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError,
    VirtioMmioDeviceConfigHandler, VirtioMmioQueueRegisterError, VirtioMmioQueueState,
    VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
};
use crate::virtio_queue::{
    VirtqueueAvailableRing, VirtqueueAvailableRingError, VirtqueueDescriptor,
    VirtqueueDescriptorChain, VirtqueueUsedRing, VirtqueueUsedRingError,
};

pub const MIN_GUEST_CID: u32 = 3;
pub const VIRTIO_VSOCK_DEVICE_ID: u32 = 19;
pub const VIRTIO_VSOCK_RX_QUEUE_INDEX: usize = 0;
pub const VIRTIO_VSOCK_TX_QUEUE_INDEX: usize = 1;
pub const VIRTIO_VSOCK_EVENT_QUEUE_INDEX: usize = 2;
pub const VIRTIO_VSOCK_QUEUE_COUNT: usize = 3;
pub const VIRTIO_VSOCK_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_VSOCK_QUEUE_SIZES: [u16; VIRTIO_VSOCK_QUEUE_COUNT] =
    [VIRTIO_VSOCK_QUEUE_SIZE; VIRTIO_VSOCK_QUEUE_COUNT];
pub const VIRTIO_VSOCK_CONFIG_GUEST_CID_SIZE: usize = 8;
pub const VIRTIO_VSOCK_PACKET_HEADER_SIZE: usize = 44;
pub const VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE: u32 = 64 * 1024;
pub const VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE: u32 = 64 * 1024;
pub const VIRTIO_VSOCK_HOST_CID: u64 = 2;
pub const VIRTIO_VSOCK_PACKET_TYPE_STREAM: u16 = 1;
pub const VIRTIO_VSOCK_OP_REQUEST: u16 = 1;
pub const VIRTIO_VSOCK_OP_RESPONSE: u16 = 2;
pub const VIRTIO_VSOCK_OP_RST: u16 = 3;
pub const VIRTIO_VSOCK_OP_SHUTDOWN: u16 = 4;
pub const VIRTIO_VSOCK_OP_RW: u16 = 5;
pub const VIRTIO_VSOCK_OP_CREDIT_UPDATE: u16 = 6;
pub const VIRTIO_VSOCK_OP_CREDIT_REQUEST: u16 = 7;
pub const VIRTIO_VSOCK_FLAGS_SHUTDOWN_RCV: u32 = 1;
pub const VIRTIO_VSOCK_FLAGS_SHUTDOWN_SEND: u32 = 2;
pub const VIRTIO_RING_FEATURE_EVENT_IDX: u32 = 29;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;
pub const VIRTIO_FEATURE_IN_ORDER: u32 = 35;
pub const VSOCK_HOST_CONNECT_REQUEST_MAX_LEN: usize = 32;
pub const VSOCK_HOST_CONNECTION_LIMIT: usize = VIRTIO_VSOCK_QUEUE_SIZE as usize;
pub const VSOCK_HOST_LOCAL_PORT_BASE: u32 = 1_u32 << 30;
pub const VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE: u32 = 1_u32 << 31;
pub const VSOCK_HOST_LOCAL_PORT_CAPACITY: u32 =
    VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE - VSOCK_HOST_LOCAL_PORT_BASE;
pub const VSOCK_HOST_RW_READ_LIMIT: usize = VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE as usize;

const VIRTIO_VSOCK_RX_QUEUE_INDEX_U32: u32 = 0;
const VIRTIO_VSOCK_TX_QUEUE_INDEX_U32: u32 = 1;
const VIRTIO_VSOCK_EVENT_QUEUE_INDEX_U32: u32 = 2;
const VIRTIO_VSOCK_PACKET_HEADER_SIZE_U32: u32 = VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32;
const VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64: u64 = VIRTIO_VSOCK_PACKET_HEADER_SIZE as u64;
const NONBLOCKING_CONNECT_INTERRUPTED_RETRY_LIMIT: usize = 4;
const VSOCK_HOST_CONNECT_COMMAND: &str = "connect";

pub type VirtioVsockMmioHandler =
    VirtioMmioRegisterHandler<VirtioVsockConfigSpace, VirtioVsockDevice>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockConfigInput {
    vsock_id: Option<String>,
    guest_cid: u32,
    uds_path: String,
}

impl VsockConfigInput {
    pub fn new(guest_cid: u32, uds_path: impl Into<String>) -> Self {
        Self {
            vsock_id: None,
            guest_cid,
            uds_path: uds_path.into(),
        }
    }

    pub fn with_vsock_id(mut self, vsock_id: impl Into<String>) -> Self {
        self.vsock_id = Some(vsock_id.into());
        self
    }

    pub fn vsock_id(&self) -> Option<&str> {
        self.vsock_id.as_deref()
    }

    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &str {
        &self.uds_path
    }

    pub fn validate(self) -> Result<VsockConfig, VsockConfigError> {
        VsockConfig::try_from(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockConfig {
    vsock_id: Option<String>,
    guest_cid: u32,
    uds_path: PathBuf,
}

impl VsockConfig {
    pub fn vsock_id(&self) -> Option<&str> {
        self.vsock_id.as_deref()
    }

    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &Path {
        &self.uds_path
    }
}

impl TryFrom<VsockConfigInput> for VsockConfig {
    type Error = VsockConfigError;

    fn try_from(input: VsockConfigInput) -> Result<Self, Self::Error> {
        if input.guest_cid < MIN_GUEST_CID {
            return Err(VsockConfigError::GuestCidTooSmall {
                guest_cid: input.guest_cid,
                min: MIN_GUEST_CID,
            });
        }

        if let Some(vsock_id) = input.vsock_id.as_deref() {
            if vsock_id.is_empty() {
                return Err(VsockConfigError::EmptyVsockId);
            }
            if has_control_character(vsock_id) {
                return Err(VsockConfigError::InvalidVsockId {
                    vsock_id: vsock_id.to_string(),
                });
            }
        }

        if input.uds_path.is_empty() {
            return Err(VsockConfigError::EmptySocketPath);
        }
        if has_control_character(&input.uds_path) {
            return Err(VsockConfigError::InvalidSocketPath);
        }

        Ok(Self {
            vsock_id: input.vsock_id,
            guest_cid: input.guest_cid,
            uds_path: PathBuf::from(input.uds_path),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VsockConfigError {
    GuestCidTooSmall { guest_cid: u32, min: u32 },
    EmptyVsockId,
    InvalidVsockId { vsock_id: String },
    EmptySocketPath,
    InvalidSocketPath,
}

impl fmt::Display for VsockConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestCidTooSmall { guest_cid, min } => {
                write!(f, "vsock guest_cid {guest_cid} is below minimum {min}")
            }
            Self::EmptyVsockId => f.write_str("vsock_id must not be empty"),
            Self::InvalidVsockId { .. } => {
                f.write_str("vsock_id must not contain control characters")
            }
            Self::EmptySocketPath => f.write_str("vsock uds_path must not be empty"),
            Self::InvalidSocketPath => {
                f.write_str("vsock uds_path must not contain control characters")
            }
        }
    }
}

impl std::error::Error for VsockConfigError {}

#[derive(Debug)]
pub struct VsockHostAcceptedConnection {
    stream: UnixStream,
    connect_request_buf: [u8; VSOCK_HOST_CONNECT_REQUEST_MAX_LEN],
    connect_request_len: usize,
    connect_request: Option<VsockHostConnectRequest>,
}

impl VsockHostAcceptedConnection {
    fn from_stream(stream: UnixStream) -> Result<Self, VsockHostSocketAcceptError> {
        stream
            .set_nonblocking(true)
            .map_err(|err| VsockHostSocketAcceptError::SetNonblocking(err.kind()))?;
        Ok(Self {
            stream,
            connect_request_buf: [0; VSOCK_HOST_CONNECT_REQUEST_MAX_LEN],
            connect_request_len: 0,
            connect_request: None,
        })
    }

    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    pub fn into_stream(self) -> UnixStream {
        self.stream
    }

    pub fn read_connect_request(
        &mut self,
    ) -> Result<Option<VsockHostConnectRequest>, VsockHostConnectHandshakeError> {
        if let Some(request) = self.connect_request {
            return Ok(Some(request));
        }

        loop {
            let Some(next_byte) = self.connect_request_buf.get_mut(self.connect_request_len) else {
                return Err(VsockHostConnectHandshakeError::TooLong {
                    max: VSOCK_HOST_CONNECT_REQUEST_MAX_LEN,
                });
            };

            match self.stream.read(std::slice::from_mut(next_byte)) {
                Ok(0) => return Err(VsockHostConnectHandshakeError::Closed),
                Ok(_) => {
                    let byte = *next_byte;
                    self.connect_request_len += 1;
                    if byte == b'\n' {
                        let bytes = self
                            .connect_request_buf
                            .get(..self.connect_request_len)
                            .ok_or(VsockHostConnectHandshakeError::TooLong {
                                max: VSOCK_HOST_CONNECT_REQUEST_MAX_LEN,
                            })?;
                        let request = VsockHostConnectRequest::parse(bytes)
                            .map_err(|source| VsockHostConnectHandshakeError::Parse { source })?;
                        self.connect_request = Some(request);
                        return Ok(Some(request));
                    }
                }
                Err(err) if is_transient_host_socket_read_error(err.kind()) => return Ok(None),
                Err(err) => return Err(VsockHostConnectHandshakeError::Read(err.kind())),
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct VsockHostSocketOwner {
    listener: UnixListener,
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl VsockHostSocketOwner {
    fn bind(path: impl AsRef<Path>) -> Result<Self, VsockHostSocketOwnerError> {
        let path = path.as_ref();
        if socket_path_exists_without_following_links(path)? {
            return Err(VsockHostSocketOwnerError::SocketPathExists);
        }

        let listener = UnixListener::bind(path).map_err(|err| match err.kind() {
            std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AlreadyExists => {
                VsockHostSocketOwnerError::SocketPathExists
            }
            kind => VsockHostSocketOwnerError::Bind(kind),
        })?;
        let metadata = socket_path_metadata(path)?;
        listener.set_nonblocking(true).map_err(|err| {
            remove_socket_path_if_owned(path, metadata.dev(), metadata.ino());
            VsockHostSocketOwnerError::SetNonblocking(err.kind())
        })?;

        let owner = Self {
            listener,
            path: path.to_path_buf(),
            dev: metadata.dev(),
            ino: metadata.ino(),
        };
        if owner.owns_current_path() {
            Ok(owner)
        } else {
            Err(VsockHostSocketOwnerError::SocketPathChanged)
        }
    }

    pub(crate) fn accept_host_connection(
        &self,
    ) -> Result<Option<VsockHostAcceptedConnection>, VsockHostSocketAcceptError> {
        match self.listener.accept() {
            Ok((stream, _addr)) => VsockHostAcceptedConnection::from_stream(stream).map(Some),
            Err(err) if is_transient_host_socket_accept_error(err.kind()) => Ok(None),
            Err(err) => Err(VsockHostSocketAcceptError::Accept(err.kind())),
        }
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    fn listener(&self) -> &UnixListener {
        &self.listener
    }

    fn owns_current_path(&self) -> bool {
        socket_path_is_owned(&self.path, self.dev, self.ino).unwrap_or(false)
    }
}

impl Drop for VsockHostSocketOwner {
    fn drop(&mut self) {
        if self.owns_current_path() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsockHostSocketOwnerError {
    SocketPathCheck(std::io::ErrorKind),
    SocketPathExists,
    Bind(std::io::ErrorKind),
    SocketMetadata(std::io::ErrorKind),
    SocketPathIsNotSocket,
    SetNonblocking(std::io::ErrorKind),
    SocketPathChanged,
}

impl fmt::Display for VsockHostSocketOwnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SocketPathCheck(kind) => {
                write!(f, "failed to check vsock host socket path: {kind:?}")
            }
            Self::SocketPathExists => f.write_str("vsock host socket path already exists"),
            Self::Bind(kind) => write!(f, "failed to bind vsock host socket: {kind:?}"),
            Self::SocketMetadata(kind) => {
                write!(f, "failed to inspect vsock host socket: {kind:?}")
            }
            Self::SocketPathIsNotSocket => f.write_str("bound vsock host path is not a socket"),
            Self::SetNonblocking(kind) => {
                write!(f, "failed to set vsock host socket nonblocking: {kind:?}")
            }
            Self::SocketPathChanged => f.write_str("vsock host socket path changed during bind"),
        }
    }
}

impl std::error::Error for VsockHostSocketOwnerError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsockHostSocketAcceptError {
    HostSocketNotAttached,
    Accept(std::io::ErrorKind),
    SetNonblocking(std::io::ErrorKind),
}

impl fmt::Display for VsockHostSocketAcceptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostSocketNotAttached => f.write_str("vsock host socket is not attached"),
            Self::Accept(kind) => {
                write!(f, "failed to accept vsock host connection: {kind:?}")
            }
            Self::SetNonblocking(kind) => {
                write!(
                    f,
                    "failed to set accepted vsock host connection nonblocking: {kind:?}"
                )
            }
        }
    }
}

impl std::error::Error for VsockHostSocketAcceptError {}

fn is_transient_host_socket_accept_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::ConnectionAborted
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsockHostConnectHandshakeError {
    Read(std::io::ErrorKind),
    Closed,
    TooLong {
        max: usize,
    },
    Parse {
        source: VsockHostConnectRequestError,
    },
}

impl fmt::Display for VsockHostConnectHandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(kind) => write!(f, "failed to read vsock host CONNECT request: {kind:?}"),
            Self::Closed => f.write_str("vsock host connection closed before CONNECT request"),
            Self::TooLong { max } => {
                write!(f, "vsock host CONNECT request exceeds maximum length {max}")
            }
            Self::Parse { source } => write!(f, "invalid vsock host CONNECT request: {source}"),
        }
    }
}

impl std::error::Error for VsockHostConnectHandshakeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse { source } => Some(source),
            Self::Read(_) | Self::Closed | Self::TooLong { .. } => None,
        }
    }
}

fn is_transient_host_socket_read_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsockHostConnectRequest {
    guest_port: u32,
}

impl VsockHostConnectRequest {
    pub fn parse(bytes: &[u8]) -> Result<Self, VsockHostConnectRequestError> {
        parse_vsock_host_connect_request(bytes)
    }

    pub const fn guest_port(self) -> u32 {
        self.guest_port
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsockHostConnectRequestError {
    Empty,
    TooLong { len: usize, max: usize },
    MissingNewline,
    TrailingData,
    InvalidUtf8,
    MissingCommand,
    InvalidCommand,
    MissingPort,
    InvalidPort,
    ExtraTokens,
}

impl fmt::Display for VsockHostConnectRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("vsock host CONNECT request must not be empty"),
            Self::TooLong { len, max } => {
                write!(
                    f,
                    "vsock host CONNECT request length {len} exceeds maximum {max}"
                )
            }
            Self::MissingNewline => f.write_str("vsock host CONNECT request must end with newline"),
            Self::TrailingData => f.write_str("vsock host CONNECT request has data after newline"),
            Self::InvalidUtf8 => f.write_str("vsock host CONNECT request is not valid UTF-8"),
            Self::MissingCommand => f.write_str("vsock host CONNECT request is missing command"),
            Self::InvalidCommand => f.write_str("vsock host CONNECT request command is invalid"),
            Self::MissingPort => f.write_str("vsock host CONNECT request is missing port"),
            Self::InvalidPort => f.write_str("vsock host CONNECT request port is invalid"),
            Self::ExtraTokens => f.write_str("vsock host CONNECT request has extra tokens"),
        }
    }
}

impl std::error::Error for VsockHostConnectRequestError {}

pub fn parse_vsock_host_connect_request(
    bytes: &[u8],
) -> Result<VsockHostConnectRequest, VsockHostConnectRequestError> {
    if bytes.is_empty() {
        return Err(VsockHostConnectRequestError::Empty);
    }
    if bytes.len() > VSOCK_HOST_CONNECT_REQUEST_MAX_LEN {
        return Err(VsockHostConnectRequestError::TooLong {
            len: bytes.len(),
            max: VSOCK_HOST_CONNECT_REQUEST_MAX_LEN,
        });
    }
    if bytes.last() != Some(&b'\n') {
        if bytes.contains(&b'\n') {
            return Err(VsockHostConnectRequestError::TrailingData);
        }
        return Err(VsockHostConnectRequestError::MissingNewline);
    }

    let request =
        std::str::from_utf8(bytes).map_err(|_| VsockHostConnectRequestError::InvalidUtf8)?;
    let mut words = request.split_whitespace();

    let command = words
        .next()
        .ok_or(VsockHostConnectRequestError::MissingCommand)?;
    if !command.eq_ignore_ascii_case(VSOCK_HOST_CONNECT_COMMAND) {
        return Err(VsockHostConnectRequestError::InvalidCommand);
    }

    let port = words
        .next()
        .ok_or(VsockHostConnectRequestError::MissingPort)?;
    if !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(VsockHostConnectRequestError::InvalidPort);
    }
    let port = port
        .parse::<u32>()
        .map_err(|_| VsockHostConnectRequestError::InvalidPort)?;

    if words.next().is_some() {
        return Err(VsockHostConnectRequestError::ExtraTokens);
    }

    Ok(VsockHostConnectRequest { guest_port: port })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VsockHostLocalPort {
    raw: u32,
}

impl VsockHostLocalPort {
    const fn from_offset(offset: u32) -> Self {
        Self {
            raw: VSOCK_HOST_LOCAL_PORT_BASE + offset,
        }
    }

    pub fn try_from_raw(raw: u32) -> Result<Self, VsockHostLocalPortError> {
        Self::try_from(raw)
    }

    pub const fn raw(self) -> u32 {
        self.raw
    }

    const fn offset(self) -> u32 {
        self.raw - VSOCK_HOST_LOCAL_PORT_BASE
    }
}

impl TryFrom<u32> for VsockHostLocalPort {
    type Error = VsockHostLocalPortError;

    fn try_from(raw: u32) -> Result<Self, Self::Error> {
        if !(VSOCK_HOST_LOCAL_PORT_BASE..VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE).contains(&raw) {
            return Err(VsockHostLocalPortError::InvalidRawPort {
                raw,
                min: VSOCK_HOST_LOCAL_PORT_BASE,
                max_exclusive: VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE,
            });
        }

        Ok(Self { raw })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsockHostLocalPortError {
    InvalidRawPort {
        raw: u32,
        min: u32,
        max_exclusive: u32,
    },
}

impl fmt::Display for VsockHostLocalPortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRawPort {
                raw,
                min,
                max_exclusive,
            } => write!(
                f,
                "vsock host local port {raw} is outside range [{min}, {max_exclusive})"
            ),
        }
    }
}

impl std::error::Error for VsockHostLocalPortError {}

#[derive(Debug)]
pub struct VsockHostLocalPortAllocator {
    capacity: u32,
    next_offset: u32,
    allocated_offsets: HashSet<u32>,
    freed_offsets: BTreeSet<u32>,
}

impl VsockHostLocalPortAllocator {
    pub fn new() -> Self {
        Self::with_capacity(VSOCK_HOST_LOCAL_PORT_CAPACITY)
    }

    fn with_capacity(capacity: u32) -> Self {
        let capacity = capacity.min(VSOCK_HOST_LOCAL_PORT_CAPACITY);
        Self {
            capacity,
            next_offset: 0,
            allocated_offsets: HashSet::new(),
            freed_offsets: BTreeSet::new(),
        }
    }

    pub fn allocate(&mut self) -> Result<VsockHostLocalPort, VsockHostLocalPortAllocatorError> {
        if self.next_offset < self.capacity {
            let offset = self.next_offset;
            self.next_offset += 1;
            self.allocated_offsets.insert(offset);
            return Ok(VsockHostLocalPort::from_offset(offset));
        }

        let Some(offset) = self.freed_offsets.pop_first() else {
            return Err(VsockHostLocalPortAllocatorError::Exhausted);
        };
        self.allocated_offsets.insert(offset);
        Ok(VsockHostLocalPort::from_offset(offset))
    }

    pub fn free(&mut self, port: VsockHostLocalPort) -> bool {
        let offset = port.offset();
        if offset >= self.capacity {
            return false;
        }
        if self.allocated_offsets.remove(&offset) {
            self.freed_offsets.insert(offset);
            return true;
        }

        false
    }
}

impl Default for VsockHostLocalPortAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsockHostLocalPortAllocatorError {
    Exhausted,
}

impl fmt::Display for VsockHostLocalPortAllocatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted => f.write_str("vsock host local port allocation exhausted"),
        }
    }
}

impl std::error::Error for VsockHostLocalPortAllocatorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VsockHostConnectionKey {
    local_port: VsockHostLocalPort,
    peer_port: u32,
}

impl VsockHostConnectionKey {
    pub const fn new(local_port: VsockHostLocalPort, peer_port: u32) -> Self {
        Self {
            local_port,
            peer_port,
        }
    }

    pub const fn local_port(self) -> VsockHostLocalPort {
        self.local_port
    }

    pub const fn peer_port(self) -> u32 {
        self.peer_port
    }
}

#[derive(Debug)]
pub struct VsockHostConnection {
    accepted: VsockHostAcceptedConnection,
    request_packet_pending: bool,
    host_ack_sent: bool,
    pending_host_rw_payload: Option<Vec<u8>>,
}

impl VsockHostConnection {
    fn from_accepted(accepted: VsockHostAcceptedConnection) -> Self {
        Self {
            accepted,
            request_packet_pending: true,
            host_ack_sent: false,
            pending_host_rw_payload: None,
        }
    }

    pub fn stream(&self) -> &UnixStream {
        self.accepted.stream()
    }

    const fn has_pending_request_packet(&self) -> bool {
        self.request_packet_pending
    }

    fn has_pending_host_rw_packet(&self) -> bool {
        self.pending_host_rw_payload.is_some()
    }

    #[cfg(test)]
    const fn host_ack_sent(&self) -> bool {
        self.host_ack_sent
    }

    fn can_poll_host_rw_payload(&self) -> bool {
        !self.request_packet_pending && self.host_ack_sent && self.pending_host_rw_payload.is_none()
    }

    fn acknowledge_guest_response(
        &mut self,
        key: VsockHostConnectionKey,
    ) -> Result<(), VsockHostConnectionAcknowledgeError> {
        if self.host_ack_sent {
            return Err(VsockHostConnectionAcknowledgeError::AlreadyAcknowledged);
        }

        let message = format!("OK {}\n", key.local_port().raw());
        let mut stream = self.accepted.stream();
        match stream.write(message.as_bytes()) {
            Ok(written) if written == message.len() => {
                self.host_ack_sent = true;
                Ok(())
            }
            Ok(written) => Err(VsockHostConnectionAcknowledgeError::ShortWrite {
                expected: message.len(),
                written,
            }),
            Err(err) => Err(VsockHostConnectionAcknowledgeError::Write(err.kind())),
        }
    }

    fn take_pending_request_packet_header(
        &mut self,
        key: VsockHostConnectionKey,
        guest_cid: u32,
    ) -> Option<VirtioVsockPacketHeader> {
        if !self.request_packet_pending {
            return None;
        }
        self.request_packet_pending = false;

        Some(host_connection_request_packet_header(key, guest_cid))
    }

    fn pending_host_rw_packet(
        &self,
        key: VsockHostConnectionKey,
        guest_cid: u32,
    ) -> Option<VirtioVsockRxPacket> {
        let payload = self.pending_host_rw_payload.as_ref()?.clone();
        Some(VirtioVsockRxPacket::new(
            VirtioVsockRxPacketKind::HostRw,
            host_connection_rw_packet_header(key, guest_cid, payload_len_u32(payload.len())),
            payload,
        ))
    }

    fn take_pending_host_rw_payload(&mut self) -> Option<Vec<u8>> {
        self.pending_host_rw_payload.take()
    }

    fn poll_host_rw_payload(&mut self, scratch: &mut [u8]) -> VsockHostRwPollOutcome {
        if !self.can_poll_host_rw_payload() {
            return VsockHostRwPollOutcome::NoData;
        }

        let mut stream = self.accepted.stream();
        match stream.read(scratch) {
            Ok(0) => VsockHostRwPollOutcome::Closed,
            Ok(read_len) => {
                let Some(bytes) = scratch.get(..read_len) else {
                    return VsockHostRwPollOutcome::ReadError;
                };
                self.pending_host_rw_payload = Some(bytes.to_vec());
                VsockHostRwPollOutcome::Queued
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                VsockHostRwPollOutcome::NoData
            }
            Err(_) => VsockHostRwPollOutcome::ReadError,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VsockHostRwPollOutcome {
    NoData,
    Queued,
    Closed,
    ReadError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VsockHostConnectionAcknowledgeError {
    AlreadyAcknowledged,
    ShortWrite { expected: usize, written: usize },
    Write(std::io::ErrorKind),
}

impl fmt::Display for VsockHostConnectionAcknowledgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyAcknowledged => {
                f.write_str("vsock host connection was already acknowledged")
            }
            Self::ShortWrite { expected, written } => write!(
                f,
                "short write while acknowledging vsock host connection: wrote {written} of {expected} bytes"
            ),
            Self::Write(kind) => {
                write!(f, "failed to acknowledge vsock host connection: {kind:?}")
            }
        }
    }
}

impl std::error::Error for VsockHostConnectionAcknowledgeError {}

fn host_connection_request_packet_header(
    key: VsockHostConnectionKey,
    guest_cid: u32,
) -> VirtioVsockPacketHeader {
    VirtioVsockPacketHeader::new()
        .with_src_cid(VIRTIO_VSOCK_HOST_CID)
        .with_dst_cid(u64::from(guest_cid))
        .with_src_port(key.local_port().raw())
        .with_dst_port(key.peer_port())
        .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
        .with_operation(VIRTIO_VSOCK_OP_REQUEST)
        .with_buffer_allocation(VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE)
}

fn host_connection_rw_packet_header(
    key: VsockHostConnectionKey,
    guest_cid: u32,
    payload_len: u32,
) -> VirtioVsockPacketHeader {
    VirtioVsockPacketHeader::new()
        .with_src_cid(VIRTIO_VSOCK_HOST_CID)
        .with_dst_cid(u64::from(guest_cid))
        .with_src_port(key.local_port().raw())
        .with_dst_port(key.peer_port())
        .with_payload_len(payload_len)
        .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
        .with_operation(VIRTIO_VSOCK_OP_RW)
        .with_buffer_allocation(VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE)
}

fn host_connection_reset_packet_header(
    key: VsockHostConnectionKey,
    guest_cid: u32,
) -> VirtioVsockPacketHeader {
    VirtioVsockPacketHeader::new()
        .with_src_cid(VIRTIO_VSOCK_HOST_CID)
        .with_dst_cid(u64::from(guest_cid))
        .with_src_port(key.local_port().raw())
        .with_dst_port(key.peer_port())
        .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
        .with_operation(VIRTIO_VSOCK_OP_RST)
        .with_buffer_allocation(VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE)
}

fn guest_connection_response_packet_header(
    key: VsockGuestConnectionKey,
    guest_cid: u32,
) -> VirtioVsockPacketHeader {
    VirtioVsockPacketHeader::new()
        .with_src_cid(VIRTIO_VSOCK_HOST_CID)
        .with_dst_cid(u64::from(guest_cid))
        .with_src_port(key.host_port())
        .with_dst_port(key.guest_port())
        .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
        .with_operation(VIRTIO_VSOCK_OP_RESPONSE)
        .with_buffer_allocation(VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE)
}

fn guest_connection_rw_packet_header(
    key: VsockGuestConnectionKey,
    guest_cid: u32,
    payload_len: u32,
) -> VirtioVsockPacketHeader {
    VirtioVsockPacketHeader::new()
        .with_src_cid(VIRTIO_VSOCK_HOST_CID)
        .with_dst_cid(u64::from(guest_cid))
        .with_src_port(key.host_port())
        .with_dst_port(key.guest_port())
        .with_payload_len(payload_len)
        .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
        .with_operation(VIRTIO_VSOCK_OP_RW)
        .with_buffer_allocation(VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE)
}

fn guest_connection_reset_packet_header(
    key: VsockGuestConnectionKey,
    guest_cid: u32,
) -> VirtioVsockPacketHeader {
    VirtioVsockPacketHeader::new()
        .with_src_cid(VIRTIO_VSOCK_HOST_CID)
        .with_dst_cid(u64::from(guest_cid))
        .with_src_port(key.host_port())
        .with_dst_port(key.guest_port())
        .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
        .with_operation(VIRTIO_VSOCK_OP_RST)
        .with_buffer_allocation(VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE)
}

fn guest_connection_socket_path(host_socket_path: &Path, host_port: u32) -> PathBuf {
    let mut path = host_socket_path.as_os_str().to_owned();
    path.push(format!("_{host_port}"));
    PathBuf::from(path)
}

fn nonblocking_unix_stream_connect(path: &Path) -> io::Result<UnixStream> {
    let address = unix_socket_address(path)?;
    let socket = nonblocking_unix_stream_socket()?;
    let mut interrupted_attempts = 0;

    loop {
        // SAFETY: `socket` is a live AF_UNIX stream fd. `address.address` is a
        // properly initialized `sockaddr_un`, and `address.len` covers only the
        // initialized pathname bytes plus the trailing NUL.
        let result = unsafe {
            libc::connect(
                socket.as_raw_fd(),
                (&raw const address.address).cast::<libc::sockaddr>(),
                address.len,
            )
        };

        if result == 0 {
            return Ok(UnixStream::from(socket));
        }

        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted
            && interrupted_attempts < NONBLOCKING_CONNECT_INTERRUPTED_RETRY_LIMIT
        {
            interrupted_attempts += 1;
            continue;
        }
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(err);
        }
        break;
    }

    finish_nonblocking_unix_stream_connect(&socket)?;
    Ok(UnixStream::from(socket))
}

#[derive(Debug)]
struct UnixSocketAddress {
    address: libc::sockaddr_un,
    len: libc::socklen_t,
}

fn unix_socket_address(path: &Path) -> io::Result<UnixSocketAddress> {
    let bytes = path.as_os_str().as_bytes();

    // SAFETY: `sockaddr_un` is a plain C address struct. Zero initialization
    // produces a valid baseline with an empty `sun_path`.
    let mut address = unsafe { mem::zeroed::<libc::sockaddr_un>() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;

    if bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }

    for (target, byte) in address.sun_path.iter_mut().zip(bytes.iter().copied()) {
        *target = byte as libc::c_char;
    }

    let len = libc::socklen_t::try_from(sockaddr_un_path_offset() + bytes.len() + 1)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;

    #[cfg(any(
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_vendor = "apple"
    ))]
    {
        address.sun_len =
            u8::try_from(len).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    }

    Ok(UnixSocketAddress { address, len })
}

const fn sockaddr_un_path_offset() -> usize {
    sockaddr_un_len_prefix_size() + mem::size_of::<libc::sa_family_t>()
}

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_vendor = "apple"
))]
const fn sockaddr_un_len_prefix_size() -> usize {
    mem::size_of::<u8>()
}

#[cfg(not(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_vendor = "apple"
)))]
const fn sockaddr_un_len_prefix_size() -> usize {
    0
}

fn nonblocking_unix_stream_socket() -> io::Result<OwnedFd> {
    // SAFETY: `socket` has no Rust aliasing requirements. On success, the raw
    // fd is immediately wrapped in `OwnedFd` so it is closed on every error path.
    let raw_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `raw_fd` was just returned by `socket` and has not been wrapped
    // or transferred elsewhere.
    let socket = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    set_nonblocking_fd(&socket)?;
    Ok(socket)
}

fn set_nonblocking_fd(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: `fd` is a live file descriptor and `F_GETFL` does not require an
    // additional pointer argument.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `fd` is live and `flags | O_NONBLOCK` preserves existing status
    // flags while enabling nonblocking I/O.
    let result = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

fn finish_nonblocking_unix_stream_connect(socket: &OwnedFd) -> io::Result<()> {
    let mut poll_fd = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    let mut interrupted_attempts = 0;
    let result = loop {
        // SAFETY: `poll_fd` points to one initialized descriptor entry. A zero
        // timeout only observes immediately completed nonblocking connects.
        let result = unsafe { libc::poll(&mut poll_fd, 1, 0) };
        if result >= 0 {
            break result;
        }

        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted
            || interrupted_attempts >= NONBLOCKING_CONNECT_INTERRUPTED_RETRY_LIMIT
        {
            return Err(err);
        }
        interrupted_attempts += 1;
    };
    if result == 0 {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }

    let mut socket_error: libc::c_int = 0;
    let mut socket_error_len = libc::socklen_t::try_from(mem::size_of_val(&socket_error))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `socket_error` and `socket_error_len` are valid output pointers
    // for the `SO_ERROR` integer on a live socket fd.
    let result = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&raw mut socket_error).cast::<libc::c_void>(),
            &raw mut socket_error_len,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if socket_error != 0 {
        return Err(io::Error::from_raw_os_error(socket_error));
    }

    Ok(())
}

fn guest_reset_packet_header_for_tx_packet(
    packet: &VirtioVsockTxPacket,
    guest_cid: u32,
) -> Option<VirtioVsockPacketHeader> {
    let header = packet.header();
    if header.src_cid() != u64::from(guest_cid) || header.dst_cid() != VIRTIO_VSOCK_HOST_CID {
        return None;
    }
    if header.operation() == VIRTIO_VSOCK_OP_RST {
        return None;
    }

    Some(
        VirtioVsockPacketHeader::new()
            .with_src_cid(VIRTIO_VSOCK_HOST_CID)
            .with_dst_cid(u64::from(guest_cid))
            .with_src_port(header.dst_port())
            .with_dst_port(header.src_port())
            .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
            .with_operation(VIRTIO_VSOCK_OP_RST)
            .with_buffer_allocation(VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE),
    )
}

#[derive(Debug)]
pub struct VsockHostConnectionTable {
    local_ports: VsockHostLocalPortAllocator,
    connections: HashMap<VsockHostConnectionKey, VsockHostConnection>,
}

impl VsockHostConnectionTable {
    pub fn new() -> Self {
        Self::with_local_port_allocator(VsockHostLocalPortAllocator::new())
    }

    fn with_local_port_allocator(local_ports: VsockHostLocalPortAllocator) -> Self {
        Self {
            local_ports,
            connections: HashMap::new(),
        }
    }

    #[cfg(test)]
    fn with_local_port_capacity(capacity: u32) -> Self {
        Self::with_local_port_allocator(VsockHostLocalPortAllocator::with_capacity(capacity))
    }

    pub fn insert_accepted_host_connection(
        &mut self,
        accepted: VsockHostAcceptedConnection,
        request: VsockHostConnectRequest,
    ) -> Result<VsockHostConnectionKey, VsockHostConnectionTableError> {
        let local_port = self
            .local_ports
            .allocate()
            .map_err(VsockHostConnectionTableError::LocalPort)?;
        let key = VsockHostConnectionKey::new(local_port, request.guest_port());
        let connection = VsockHostConnection::from_accepted(accepted);

        if let Err(error) = self.insert_allocated_connection(key, connection) {
            let freed = self.local_ports.free(local_port);
            debug_assert!(freed);
            return Err(error);
        }

        Ok(key)
    }

    fn insert_allocated_connection(
        &mut self,
        key: VsockHostConnectionKey,
        connection: VsockHostConnection,
    ) -> Result<(), VsockHostConnectionTableError> {
        match self.connections.entry(key) {
            Entry::Occupied(_) => Err(VsockHostConnectionTableError::DuplicateKey { key }),
            Entry::Vacant(entry) => {
                entry.insert(connection);
                Ok(())
            }
        }
    }

    #[cfg(test)]
    fn insert_allocated_connection_for_test(
        &mut self,
        key: VsockHostConnectionKey,
        connection: VsockHostConnection,
    ) -> Result<(), VsockHostConnectionTableError> {
        self.insert_allocated_connection(key, connection)
    }

    pub fn remove(&mut self, key: VsockHostConnectionKey) -> bool {
        if self.connections.remove(&key).is_none() {
            return false;
        }

        let freed = self.local_ports.free(key.local_port());
        debug_assert!(freed);
        true
    }

    pub fn get(&self, key: VsockHostConnectionKey) -> Option<&VsockHostConnection> {
        self.connections.get(&key)
    }

    pub fn has_pending_request_packet(&self, key: VsockHostConnectionKey) -> bool {
        self.connections
            .get(&key)
            .is_some_and(VsockHostConnection::has_pending_request_packet)
    }

    pub fn take_pending_request_packet_header(
        &mut self,
        key: VsockHostConnectionKey,
        guest_cid: u32,
    ) -> Option<VirtioVsockPacketHeader> {
        self.connections
            .get_mut(&key)?
            .take_pending_request_packet_header(key, guest_cid)
    }

    fn first_pending_host_rw_packet_key(&self) -> Option<VsockHostConnectionKey> {
        self.connections
            .iter()
            .filter_map(|(key, connection)| connection.has_pending_host_rw_packet().then_some(*key))
            .min()
    }

    fn pending_host_rw_packet(
        &self,
        key: VsockHostConnectionKey,
        guest_cid: u32,
    ) -> Option<VirtioVsockRxPacket> {
        self.connections
            .get(&key)?
            .pending_host_rw_packet(key, guest_cid)
    }

    fn take_pending_host_rw_payload(&mut self, key: VsockHostConnectionKey) -> Option<Vec<u8>> {
        self.connections
            .get_mut(&key)?
            .take_pending_host_rw_payload()
    }

    fn poll_host_rw_payloads(
        &mut self,
        scratch: &mut [u8],
        guest_cid: u32,
    ) -> Vec<VirtioVsockPacketHeader> {
        let keys = self.connections.keys().copied().collect::<Vec<_>>();
        let mut reset_headers = Vec::new();

        for key in keys {
            let Some(connection) = self.connections.get_mut(&key) else {
                continue;
            };

            match connection.poll_host_rw_payload(scratch) {
                VsockHostRwPollOutcome::NoData | VsockHostRwPollOutcome::Queued => {}
                VsockHostRwPollOutcome::Closed | VsockHostRwPollOutcome::ReadError => {
                    let removed = self.remove(key);
                    debug_assert!(removed);
                    reset_headers.push(host_connection_reset_packet_header(key, guest_cid));
                }
            }
        }

        reset_headers
    }

    fn acknowledge_guest_response_packet(
        &mut self,
        packet: &VirtioVsockTxPacket,
        guest_cid: u32,
    ) -> Option<VsockHostGuestResponseOutcome> {
        let header = packet.header();
        if header.operation() != VIRTIO_VSOCK_OP_RESPONSE {
            return None;
        }

        if header.packet_type() != VIRTIO_VSOCK_PACKET_TYPE_STREAM {
            return Some(VsockHostGuestResponseOutcome::Ignored {
                reason: VsockHostGuestResponseIgnoreReason::UnsupportedPacketType,
            });
        }
        if header.payload_len() != 0 {
            return Some(VsockHostGuestResponseOutcome::Ignored {
                reason: VsockHostGuestResponseIgnoreReason::PayloadPresent,
            });
        }
        if header.src_cid() != u64::from(guest_cid) {
            return Some(VsockHostGuestResponseOutcome::Ignored {
                reason: VsockHostGuestResponseIgnoreReason::WrongSourceCid,
            });
        }
        if header.dst_cid() != VIRTIO_VSOCK_HOST_CID {
            return Some(VsockHostGuestResponseOutcome::Ignored {
                reason: VsockHostGuestResponseIgnoreReason::WrongDestinationCid,
            });
        }

        let Ok(local_port) = VsockHostLocalPort::try_from_raw(header.dst_port()) else {
            return Some(VsockHostGuestResponseOutcome::Ignored {
                reason: VsockHostGuestResponseIgnoreReason::InvalidLocalPort,
            });
        };
        let key = VsockHostConnectionKey::new(local_port, header.src_port());
        let Some(connection) = self.connections.get_mut(&key) else {
            return Some(VsockHostGuestResponseOutcome::Ignored {
                reason: VsockHostGuestResponseIgnoreReason::MissingConnection,
            });
        };
        if connection.has_pending_request_packet() {
            return Some(VsockHostGuestResponseOutcome::Ignored {
                reason: VsockHostGuestResponseIgnoreReason::RequestStillPending,
            });
        }

        match connection.acknowledge_guest_response(key) {
            Ok(()) => Some(VsockHostGuestResponseOutcome::Acknowledged { key }),
            Err(VsockHostConnectionAcknowledgeError::AlreadyAcknowledged) => {
                Some(VsockHostGuestResponseOutcome::Ignored {
                    reason: VsockHostGuestResponseIgnoreReason::AlreadyAcknowledged,
                })
            }
            Err(source) => {
                let removed = self.remove(key);
                debug_assert!(removed);
                Some(VsockHostGuestResponseOutcome::Dropped { key, source })
            }
        }
    }

    fn first_pending_request_packet_key(&self) -> Option<VsockHostConnectionKey> {
        self.connections
            .iter()
            .filter_map(|(key, connection)| connection.has_pending_request_packet().then_some(*key))
            .min()
    }

    pub fn contains(&self, key: VsockHostConnectionKey) -> bool {
        self.connections.contains_key(&key)
    }

    pub fn len(&self) -> usize {
        self.connections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
}

impl Default for VsockHostConnectionTable {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VsockHostGuestResponseOutcome {
    Acknowledged {
        key: VsockHostConnectionKey,
    },
    Ignored {
        reason: VsockHostGuestResponseIgnoreReason,
    },
    Dropped {
        key: VsockHostConnectionKey,
        source: VsockHostConnectionAcknowledgeError,
    },
}

impl VsockHostGuestResponseOutcome {
    const fn suppresses_guest_reset(self) -> bool {
        matches!(
            self,
            Self::Acknowledged { .. }
                | Self::Ignored {
                    reason: VsockHostGuestResponseIgnoreReason::AlreadyAcknowledged,
                }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VsockHostGuestResponseIgnoreReason {
    UnsupportedPacketType,
    PayloadPresent,
    WrongSourceCid,
    WrongDestinationCid,
    InvalidLocalPort,
    MissingConnection,
    RequestStillPending,
    AlreadyAcknowledged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VsockGuestConnectionKey {
    host_port: u32,
    guest_port: u32,
}

impl VsockGuestConnectionKey {
    pub const fn new(host_port: u32, guest_port: u32) -> Self {
        Self {
            host_port,
            guest_port,
        }
    }

    pub const fn host_port(self) -> u32 {
        self.host_port
    }

    pub const fn guest_port(self) -> u32 {
        self.guest_port
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VsockGuestConnectionRequestOutcome {
    Retained {
        key: VsockGuestConnectionKey,
    },
    Ignored {
        reason: VsockGuestConnectionRequestIgnoreReason,
    },
    Dropped {
        key: VsockGuestConnectionKey,
        source: VsockGuestConnectionRequestError,
    },
}

impl VsockGuestConnectionRequestOutcome {
    const fn suppresses_guest_reset(self) -> bool {
        matches!(self, Self::Retained { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VsockGuestConnectionRequestIgnoreReason {
    WrongSourceCid,
    WrongDestinationCid,
    UnsupportedPacketType,
    PayloadPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VsockGuestConnectionRequestError {
    MissingHostSocketPath,
    Connect(std::io::ErrorKind),
    Table(VsockGuestConnectionTableError),
}

#[derive(Debug)]
enum VsockGuestRwOutcome {
    Forwarded {
        key: VsockGuestConnectionKey,
        bytes: usize,
    },
    Ignored {
        reason: VsockGuestRwIgnoreReason,
    },
    Dropped {
        key: VsockGuestConnectionKey,
        source: VsockGuestRwForwardError,
    },
}

impl VsockGuestRwOutcome {
    const fn suppresses_guest_reset(&self) -> bool {
        matches!(self, Self::Forwarded { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VsockGuestRwIgnoreReason {
    WrongSourceCid,
    WrongDestinationCid,
    UnsupportedPacketType,
}

#[derive(Debug)]
enum VsockGuestRwForwardError {
    MissingConnection,
    ResponseStillPending,
    PayloadLenTooLarge {
        len: u32,
    },
    PayloadSegmentTooLarge {
        descriptor_index: u16,
        len: u32,
    },
    PayloadAllocationFailed {
        len: usize,
        source: std::collections::TryReserveError,
    },
    PayloadLengthMismatch {
        expected: usize,
        actual: usize,
    },
    PayloadRead {
        descriptor_index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    StreamWrite(std::io::ErrorKind),
    ShortWrite {
        expected: usize,
        actual: usize,
    },
    WriteZero {
        expected: usize,
    },
}

impl fmt::Display for VsockGuestRwForwardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingConnection => f.write_str("guest RW packet has no retained connection"),
            Self::ResponseStillPending => {
                f.write_str("guest RW packet arrived before response was delivered")
            }
            Self::PayloadLenTooLarge { len } => {
                write!(f, "guest RW payload length {len} cannot fit in host memory")
            }
            Self::PayloadSegmentTooLarge {
                descriptor_index,
                len,
            } => write!(
                f,
                "guest RW payload segment {descriptor_index} length {len} cannot fit in host memory"
            ),
            Self::PayloadAllocationFailed { len, source } => {
                write!(
                    f,
                    "failed to allocate {len} bytes for guest RW payload: {source}"
                )
            }
            Self::PayloadLengthMismatch { expected, actual } => write!(
                f,
                "guest RW payload segments contain {actual} bytes, expected {expected}"
            ),
            Self::PayloadRead {
                descriptor_index,
                address,
                len,
                source,
            } => write!(
                f,
                "failed to read guest RW payload segment {descriptor_index} at {address} ({len} bytes): {source}"
            ),
            Self::StreamWrite(kind) => {
                write!(
                    f,
                    "failed to write guest RW payload to host stream: {kind:?}"
                )
            }
            Self::ShortWrite { expected, actual } => write!(
                f,
                "short guest RW host stream write: wrote {actual} of {expected} bytes"
            ),
            Self::WriteZero { expected } => write!(
                f,
                "guest RW host stream write accepted 0 of {expected} bytes"
            ),
        }
    }
}

impl std::error::Error for VsockGuestRwForwardError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PayloadAllocationFailed { source, .. } => Some(source),
            Self::PayloadRead { source, .. } => Some(source),
            Self::MissingConnection
            | Self::ResponseStillPending
            | Self::PayloadLenTooLarge { .. }
            | Self::PayloadSegmentTooLarge { .. }
            | Self::PayloadLengthMismatch { .. }
            | Self::StreamWrite(_)
            | Self::ShortWrite { .. }
            | Self::WriteZero { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct VsockGuestConnection {
    stream: UnixStream,
    response_packet_pending: bool,
    pending_host_rw_payload: Option<Vec<u8>>,
}

impl VsockGuestConnection {
    fn from_stream(stream: UnixStream) -> Self {
        Self {
            stream,
            response_packet_pending: true,
            pending_host_rw_payload: None,
        }
    }

    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    const fn has_pending_response_packet(&self) -> bool {
        self.response_packet_pending
    }

    fn has_pending_host_rw_packet(&self) -> bool {
        self.pending_host_rw_payload.is_some()
    }

    fn can_poll_host_rw_payload(&self) -> bool {
        !self.response_packet_pending && self.pending_host_rw_payload.is_none()
    }

    fn write_guest_rw_payload(&mut self, payload: &[u8]) -> Result<(), VsockGuestRwForwardError> {
        if payload.is_empty() {
            return Ok(());
        }

        match self.stream.write(payload) {
            Ok(written) if written == payload.len() => Ok(()),
            Ok(0) => Err(VsockGuestRwForwardError::WriteZero {
                expected: payload.len(),
            }),
            Ok(written) => Err(VsockGuestRwForwardError::ShortWrite {
                expected: payload.len(),
                actual: written,
            }),
            Err(error) => Err(VsockGuestRwForwardError::StreamWrite(error.kind())),
        }
    }

    fn take_pending_response_packet_header(
        &mut self,
        key: VsockGuestConnectionKey,
        guest_cid: u32,
    ) -> Option<VirtioVsockPacketHeader> {
        if !self.response_packet_pending {
            return None;
        }
        self.response_packet_pending = false;

        Some(guest_connection_response_packet_header(key, guest_cid))
    }

    fn pending_host_rw_packet(
        &self,
        key: VsockGuestConnectionKey,
        guest_cid: u32,
    ) -> Option<VirtioVsockRxPacket> {
        let payload = self.pending_host_rw_payload.as_ref()?.clone();
        Some(VirtioVsockRxPacket::new(
            VirtioVsockRxPacketKind::HostRw,
            guest_connection_rw_packet_header(key, guest_cid, payload_len_u32(payload.len())),
            payload,
        ))
    }

    fn take_pending_host_rw_payload(&mut self) -> Option<Vec<u8>> {
        self.pending_host_rw_payload.take()
    }

    fn poll_host_rw_payload(&mut self, scratch: &mut [u8]) -> VsockHostRwPollOutcome {
        if !self.can_poll_host_rw_payload() {
            return VsockHostRwPollOutcome::NoData;
        }

        match self.stream.read(scratch) {
            Ok(0) => VsockHostRwPollOutcome::Closed,
            Ok(read_len) => {
                let Some(bytes) = scratch.get(..read_len) else {
                    return VsockHostRwPollOutcome::ReadError;
                };
                self.pending_host_rw_payload = Some(bytes.to_vec());
                VsockHostRwPollOutcome::Queued
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                VsockHostRwPollOutcome::NoData
            }
            Err(_) => VsockHostRwPollOutcome::ReadError,
        }
    }
}

#[derive(Debug)]
pub struct VsockGuestConnectionTable {
    limit: usize,
    connections: HashMap<VsockGuestConnectionKey, VsockGuestConnection>,
}

impl VsockGuestConnectionTable {
    pub fn new() -> Self {
        Self::with_limit(VSOCK_HOST_CONNECTION_LIMIT)
    }

    fn with_limit(limit: usize) -> Self {
        Self {
            limit,
            connections: HashMap::new(),
        }
    }

    fn check_insert(
        &self,
        key: VsockGuestConnectionKey,
    ) -> Result<(), VsockGuestConnectionTableError> {
        if self.connections.contains_key(&key) {
            return Err(VsockGuestConnectionTableError::DuplicateKey { key });
        }

        if self.connections.len() >= self.limit {
            return Err(VsockGuestConnectionTableError::LimitExceeded { limit: self.limit });
        }

        Ok(())
    }

    pub fn insert_connected_guest_connection(
        &mut self,
        key: VsockGuestConnectionKey,
        stream: UnixStream,
    ) -> Result<(), VsockGuestConnectionTableError> {
        self.check_insert(key)?;

        let replaced = self
            .connections
            .insert(key, VsockGuestConnection::from_stream(stream));
        debug_assert!(replaced.is_none());
        Ok(())
    }

    pub fn get(&self, key: VsockGuestConnectionKey) -> Option<&VsockGuestConnection> {
        self.connections.get(&key)
    }

    pub fn contains(&self, key: VsockGuestConnectionKey) -> bool {
        self.connections.contains_key(&key)
    }

    pub fn has_pending_response_packet(&self, key: VsockGuestConnectionKey) -> bool {
        self.connections
            .get(&key)
            .is_some_and(VsockGuestConnection::has_pending_response_packet)
    }

    pub fn take_pending_response_packet_header(
        &mut self,
        key: VsockGuestConnectionKey,
        guest_cid: u32,
    ) -> Option<VirtioVsockPacketHeader> {
        self.connections
            .get_mut(&key)?
            .take_pending_response_packet_header(key, guest_cid)
    }

    fn first_pending_host_rw_packet_key(&self) -> Option<VsockGuestConnectionKey> {
        self.connections
            .iter()
            .filter_map(|(key, connection)| connection.has_pending_host_rw_packet().then_some(*key))
            .min()
    }

    fn pending_host_rw_packet(
        &self,
        key: VsockGuestConnectionKey,
        guest_cid: u32,
    ) -> Option<VirtioVsockRxPacket> {
        self.connections
            .get(&key)?
            .pending_host_rw_packet(key, guest_cid)
    }

    fn take_pending_host_rw_payload(&mut self, key: VsockGuestConnectionKey) -> Option<Vec<u8>> {
        self.connections
            .get_mut(&key)?
            .take_pending_host_rw_payload()
    }

    fn poll_host_rw_payloads(
        &mut self,
        scratch: &mut [u8],
        guest_cid: u32,
    ) -> Vec<VirtioVsockPacketHeader> {
        let keys = self.connections.keys().copied().collect::<Vec<_>>();
        let mut reset_headers = Vec::new();

        for key in keys {
            let Some(connection) = self.connections.get_mut(&key) else {
                continue;
            };

            match connection.poll_host_rw_payload(scratch) {
                VsockHostRwPollOutcome::NoData | VsockHostRwPollOutcome::Queued => {}
                VsockHostRwPollOutcome::Closed | VsockHostRwPollOutcome::ReadError => {
                    let removed = self.remove(key);
                    debug_assert!(removed);
                    reset_headers.push(guest_connection_reset_packet_header(key, guest_cid));
                }
            }
        }

        reset_headers
    }

    fn forward_guest_rw_packet(
        &mut self,
        memory: &GuestMemory,
        packet: &VirtioVsockTxPacket,
        guest_cid: u32,
    ) -> Option<VsockGuestRwOutcome> {
        let header = packet.header();
        if header.operation() != VIRTIO_VSOCK_OP_RW {
            return None;
        }

        if header.packet_type() != VIRTIO_VSOCK_PACKET_TYPE_STREAM {
            return Some(VsockGuestRwOutcome::Ignored {
                reason: VsockGuestRwIgnoreReason::UnsupportedPacketType,
            });
        }
        if header.src_cid() != u64::from(guest_cid) {
            return Some(VsockGuestRwOutcome::Ignored {
                reason: VsockGuestRwIgnoreReason::WrongSourceCid,
            });
        }
        if header.dst_cid() != VIRTIO_VSOCK_HOST_CID {
            return Some(VsockGuestRwOutcome::Ignored {
                reason: VsockGuestRwIgnoreReason::WrongDestinationCid,
            });
        }

        let key = VsockGuestConnectionKey::new(header.dst_port(), header.src_port());
        let Some(connection) = self.connections.get(&key) else {
            return Some(VsockGuestRwOutcome::Dropped {
                key,
                source: VsockGuestRwForwardError::MissingConnection,
            });
        };
        if connection.has_pending_response_packet() {
            let removed = self.remove(key);
            debug_assert!(removed);
            return Some(VsockGuestRwOutcome::Dropped {
                key,
                source: VsockGuestRwForwardError::ResponseStillPending,
            });
        }

        let payload = match read_vsock_tx_payload_bytes(memory, packet) {
            Ok(payload) => payload,
            Err(source) => {
                let removed = self.remove(key);
                debug_assert!(removed);
                return Some(VsockGuestRwOutcome::Dropped { key, source });
            }
        };

        let Some(connection) = self.connections.get_mut(&key) else {
            return Some(VsockGuestRwOutcome::Dropped {
                key,
                source: VsockGuestRwForwardError::MissingConnection,
            });
        };
        let result = connection.write_guest_rw_payload(&payload);
        match result {
            Ok(()) => Some(VsockGuestRwOutcome::Forwarded {
                key,
                bytes: payload.len(),
            }),
            Err(source) => {
                let removed = self.remove(key);
                debug_assert!(removed);
                Some(VsockGuestRwOutcome::Dropped { key, source })
            }
        }
    }

    fn remove(&mut self, key: VsockGuestConnectionKey) -> bool {
        self.connections.remove(&key).is_some()
    }

    fn first_pending_response_packet_key(&self) -> Option<VsockGuestConnectionKey> {
        self.connections
            .iter()
            .filter_map(|(key, connection)| {
                connection.has_pending_response_packet().then_some(*key)
            })
            .min()
    }

    pub fn len(&self) -> usize {
        self.connections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
}

impl Default for VsockGuestConnectionTable {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsockGuestConnectionTableError {
    LimitExceeded { limit: usize },
    DuplicateKey { key: VsockGuestConnectionKey },
}

impl fmt::Display for VsockGuestConnectionTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded { limit } => {
                write!(f, "vsock guest connection limit {limit} reached")
            }
            Self::DuplicateKey { key } => write!(
                f,
                "vsock guest connection already exists for host port {} and guest port {}",
                key.host_port(),
                key.guest_port()
            ),
        }
    }
}

impl std::error::Error for VsockGuestConnectionTableError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsockHostConnectionTableError {
    LocalPort(VsockHostLocalPortAllocatorError),
    DuplicateKey { key: VsockHostConnectionKey },
}

impl fmt::Display for VsockHostConnectionTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalPort(source) => {
                write!(f, "failed to allocate vsock host local port: {source}")
            }
            Self::DuplicateKey { key } => write!(
                f,
                "vsock host connection already exists for local port {} and peer port {}",
                key.local_port().raw(),
                key.peer_port()
            ),
        }
    }
}

impl std::error::Error for VsockHostConnectionTableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LocalPort(source) => Some(source),
            Self::DuplicateKey { .. } => None,
        }
    }
}

fn socket_path_exists_without_following_links(
    path: &Path,
) -> Result<bool, VsockHostSocketOwnerError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(VsockHostSocketOwnerError::SocketPathCheck(err.kind())),
    }
}

fn socket_path_metadata(path: &Path) -> Result<fs::Metadata, VsockHostSocketOwnerError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|err| VsockHostSocketOwnerError::SocketMetadata(err.kind()))?;

    if !metadata.file_type().is_socket() {
        return Err(VsockHostSocketOwnerError::SocketPathIsNotSocket);
    }

    Ok(metadata)
}

fn socket_path_is_owned(
    path: &Path,
    dev: u64,
    ino: u64,
) -> Result<bool, VsockHostSocketOwnerError> {
    let metadata = socket_path_metadata(path)?;

    Ok(metadata.dev() == dev && metadata.ino() == ino)
}

fn remove_socket_path_if_owned(path: &Path, dev: u64, ino: u64) {
    if socket_path_is_owned(path, dev, ino).unwrap_or(false) {
        let _ = fs::remove_file(path);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioVsockPacketHeader {
    src_cid: u64,
    dst_cid: u64,
    src_port: u32,
    dst_port: u32,
    payload_len: u32,
    packet_type: u16,
    operation: u16,
    flags: u32,
    buffer_allocation: u32,
    forwarded_count: u32,
}

impl VirtioVsockPacketHeader {
    pub const fn new() -> Self {
        Self {
            src_cid: 0,
            dst_cid: 0,
            src_port: 0,
            dst_port: 0,
            payload_len: 0,
            packet_type: 0,
            operation: 0,
            flags: 0,
            buffer_allocation: 0,
            forwarded_count: 0,
        }
    }

    pub const fn src_cid(self) -> u64 {
        self.src_cid
    }

    pub const fn dst_cid(self) -> u64 {
        self.dst_cid
    }

    pub const fn src_port(self) -> u32 {
        self.src_port
    }

    pub const fn dst_port(self) -> u32 {
        self.dst_port
    }

    pub const fn payload_len(self) -> u32 {
        self.payload_len
    }

    pub const fn packet_type(self) -> u16 {
        self.packet_type
    }

    pub const fn operation(self) -> u16 {
        self.operation
    }

    pub const fn flags(self) -> u32 {
        self.flags
    }

    pub const fn buffer_allocation(self) -> u32 {
        self.buffer_allocation
    }

    pub const fn forwarded_count(self) -> u32 {
        self.forwarded_count
    }

    pub const fn with_src_cid(mut self, src_cid: u64) -> Self {
        self.src_cid = src_cid;
        self
    }

    pub const fn with_dst_cid(mut self, dst_cid: u64) -> Self {
        self.dst_cid = dst_cid;
        self
    }

    pub const fn with_src_port(mut self, src_port: u32) -> Self {
        self.src_port = src_port;
        self
    }

    pub const fn with_dst_port(mut self, dst_port: u32) -> Self {
        self.dst_port = dst_port;
        self
    }

    pub const fn with_payload_len(mut self, payload_len: u32) -> Self {
        self.payload_len = payload_len;
        self
    }

    pub const fn with_packet_type(mut self, packet_type: u16) -> Self {
        self.packet_type = packet_type;
        self
    }

    pub const fn with_operation(mut self, operation: u16) -> Self {
        self.operation = operation;
        self
    }

    pub const fn with_flags(mut self, flags: u32) -> Self {
        self.flags = flags;
        self
    }

    pub const fn with_buffer_allocation(mut self, buffer_allocation: u32) -> Self {
        self.buffer_allocation = buffer_allocation;
        self
    }

    pub const fn with_forwarded_count(mut self, forwarded_count: u32) -> Self {
        self.forwarded_count = forwarded_count;
        self
    }

    pub fn validate_payload_len(self) -> Result<(), VirtioVsockPacketLengthError> {
        if self.payload_len > VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE {
            return Err(VirtioVsockPacketLengthError {
                payload_len: self.payload_len,
                max_payload_len: VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE,
            });
        }

        Ok(())
    }

    pub fn to_bytes(self) -> [u8; VIRTIO_VSOCK_PACKET_HEADER_SIZE] {
        let [
            src_cid_0,
            src_cid_1,
            src_cid_2,
            src_cid_3,
            src_cid_4,
            src_cid_5,
            src_cid_6,
            src_cid_7,
        ] = self.src_cid.to_le_bytes();
        let [
            dst_cid_0,
            dst_cid_1,
            dst_cid_2,
            dst_cid_3,
            dst_cid_4,
            dst_cid_5,
            dst_cid_6,
            dst_cid_7,
        ] = self.dst_cid.to_le_bytes();
        let [src_port_0, src_port_1, src_port_2, src_port_3] = self.src_port.to_le_bytes();
        let [dst_port_0, dst_port_1, dst_port_2, dst_port_3] = self.dst_port.to_le_bytes();
        let [payload_len_0, payload_len_1, payload_len_2, payload_len_3] =
            self.payload_len.to_le_bytes();
        let [packet_type_0, packet_type_1] = self.packet_type.to_le_bytes();
        let [operation_0, operation_1] = self.operation.to_le_bytes();
        let [flags_0, flags_1, flags_2, flags_3] = self.flags.to_le_bytes();
        let [
            buffer_allocation_0,
            buffer_allocation_1,
            buffer_allocation_2,
            buffer_allocation_3,
        ] = self.buffer_allocation.to_le_bytes();
        let [
            forwarded_count_0,
            forwarded_count_1,
            forwarded_count_2,
            forwarded_count_3,
        ] = self.forwarded_count.to_le_bytes();

        [
            src_cid_0,
            src_cid_1,
            src_cid_2,
            src_cid_3,
            src_cid_4,
            src_cid_5,
            src_cid_6,
            src_cid_7,
            dst_cid_0,
            dst_cid_1,
            dst_cid_2,
            dst_cid_3,
            dst_cid_4,
            dst_cid_5,
            dst_cid_6,
            dst_cid_7,
            src_port_0,
            src_port_1,
            src_port_2,
            src_port_3,
            dst_port_0,
            dst_port_1,
            dst_port_2,
            dst_port_3,
            payload_len_0,
            payload_len_1,
            payload_len_2,
            payload_len_3,
            packet_type_0,
            packet_type_1,
            operation_0,
            operation_1,
            flags_0,
            flags_1,
            flags_2,
            flags_3,
            buffer_allocation_0,
            buffer_allocation_1,
            buffer_allocation_2,
            buffer_allocation_3,
            forwarded_count_0,
            forwarded_count_1,
            forwarded_count_2,
            forwarded_count_3,
        ]
    }

    pub fn try_from_bytes(
        bytes: [u8; VIRTIO_VSOCK_PACKET_HEADER_SIZE],
    ) -> Result<Self, VirtioVsockPacketLengthError> {
        let header = Self::decode_bytes(bytes);
        header.validate_payload_len()?;
        Ok(header)
    }

    const fn decode_bytes(bytes: [u8; VIRTIO_VSOCK_PACKET_HEADER_SIZE]) -> Self {
        let [
            src_cid_0,
            src_cid_1,
            src_cid_2,
            src_cid_3,
            src_cid_4,
            src_cid_5,
            src_cid_6,
            src_cid_7,
            dst_cid_0,
            dst_cid_1,
            dst_cid_2,
            dst_cid_3,
            dst_cid_4,
            dst_cid_5,
            dst_cid_6,
            dst_cid_7,
            src_port_0,
            src_port_1,
            src_port_2,
            src_port_3,
            dst_port_0,
            dst_port_1,
            dst_port_2,
            dst_port_3,
            payload_len_0,
            payload_len_1,
            payload_len_2,
            payload_len_3,
            packet_type_0,
            packet_type_1,
            operation_0,
            operation_1,
            flags_0,
            flags_1,
            flags_2,
            flags_3,
            buffer_allocation_0,
            buffer_allocation_1,
            buffer_allocation_2,
            buffer_allocation_3,
            forwarded_count_0,
            forwarded_count_1,
            forwarded_count_2,
            forwarded_count_3,
        ] = bytes;

        Self {
            src_cid: u64::from_le_bytes([
                src_cid_0, src_cid_1, src_cid_2, src_cid_3, src_cid_4, src_cid_5, src_cid_6,
                src_cid_7,
            ]),
            dst_cid: u64::from_le_bytes([
                dst_cid_0, dst_cid_1, dst_cid_2, dst_cid_3, dst_cid_4, dst_cid_5, dst_cid_6,
                dst_cid_7,
            ]),
            src_port: u32::from_le_bytes([src_port_0, src_port_1, src_port_2, src_port_3]),
            dst_port: u32::from_le_bytes([dst_port_0, dst_port_1, dst_port_2, dst_port_3]),
            payload_len: u32::from_le_bytes([
                payload_len_0,
                payload_len_1,
                payload_len_2,
                payload_len_3,
            ]),
            packet_type: u16::from_le_bytes([packet_type_0, packet_type_1]),
            operation: u16::from_le_bytes([operation_0, operation_1]),
            flags: u32::from_le_bytes([flags_0, flags_1, flags_2, flags_3]),
            buffer_allocation: u32::from_le_bytes([
                buffer_allocation_0,
                buffer_allocation_1,
                buffer_allocation_2,
                buffer_allocation_3,
            ]),
            forwarded_count: u32::from_le_bytes([
                forwarded_count_0,
                forwarded_count_1,
                forwarded_count_2,
                forwarded_count_3,
            ]),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockPacketLengthError {
    payload_len: u32,
    max_payload_len: u32,
}

impl VirtioVsockPacketLengthError {
    pub const fn payload_len(self) -> u32 {
        self.payload_len
    }

    pub const fn max_payload_len(self) -> u32 {
        self.max_payload_len
    }
}

impl fmt::Display for VirtioVsockPacketLengthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "virtio-vsock packet payload length {} exceeds maximum {}",
            self.payload_len, self.max_payload_len
        )
    }
}

impl std::error::Error for VirtioVsockPacketLengthError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockTxPayloadSegment {
    descriptor_index: u16,
    address: GuestAddress,
    len: u32,
}

impl VirtioVsockTxPayloadSegment {
    const fn new(descriptor_index: u16, address: GuestAddress, len: u32) -> Self {
        Self {
            descriptor_index,
            address,
            len,
        }
    }

    pub const fn descriptor_index(self) -> u16 {
        self.descriptor_index
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn len(self) -> u32 {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioVsockTxPacket {
    descriptor_head: u16,
    header: VirtioVsockPacketHeader,
    payload_segments: Vec<VirtioVsockTxPayloadSegment>,
}

impl VirtioVsockTxPacket {
    pub fn parse(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioVsockTxPacketParseError> {
        let (descriptor_head, total_readable_len) =
            validate_vsock_tx_descriptor_chain(memory, chain)?;
        validate_vsock_tx_header_available(descriptor_head, total_readable_len)?;

        let header_bytes = read_vsock_tx_header_bytes(memory, chain, descriptor_head)?;
        let header = VirtioVsockPacketHeader::try_from_bytes(header_bytes)
            .map_err(|source| VirtioVsockTxPacketParseError::InvalidHeaderLength { source })?;
        validate_vsock_tx_payload_len(descriptor_head, total_readable_len, header.payload_len())?;

        let payload_segments = vsock_tx_payload_segments(chain, header.payload_len())?;

        Ok(Self {
            descriptor_head,
            header,
            payload_segments,
        })
    }

    pub const fn descriptor_head(&self) -> u16 {
        self.descriptor_head
    }

    pub const fn header(&self) -> VirtioVsockPacketHeader {
        self.header
    }

    pub fn payload_segments(&self) -> &[VirtioVsockTxPayloadSegment] {
        &self.payload_segments
    }

    pub const fn payload_len(&self) -> u32 {
        self.header.payload_len()
    }

    pub fn packet_len(&self) -> u64 {
        VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64 + u64::from(self.payload_len())
    }
}

#[derive(Debug)]
pub enum VirtioVsockTxPacketParseError {
    DescriptorChainTooShort {
        expected: usize,
        actual: usize,
    },
    DescriptorWriteOnly {
        index: u16,
    },
    DescriptorRangeOverflow {
        index: u16,
        address: GuestAddress,
        len: u32,
    },
    DescriptorAccess {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    ReadableLengthOverflow {
        descriptor_head: u16,
    },
    HeaderTooShort {
        descriptor_head: u16,
        actual: u64,
        min: u64,
    },
    ReadHeader {
        index: u16,
        address: GuestAddress,
        len: usize,
        source: GuestMemoryAccessError,
    },
    InvalidHeaderLength {
        source: VirtioVsockPacketLengthError,
    },
    PayloadTooShort {
        descriptor_head: u16,
        required: u32,
        available: u64,
    },
    PayloadSegmentLengthOverflow {
        index: u16,
        available: u64,
    },
    PayloadSegmentsAllocationFailed {
        descriptor_count: usize,
        source: std::collections::TryReserveError,
    },
}

impl fmt::Display for VirtioVsockTxPacketParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DescriptorChainTooShort { expected, actual } => {
                write!(
                    f,
                    "virtio-vsock TX descriptor chain has {actual} descriptors; expected at least {expected}"
                )
            }
            Self::DescriptorWriteOnly { index } => {
                write!(f, "virtio-vsock TX descriptor {index} is write-only")
            }
            Self::DescriptorRangeOverflow {
                index,
                address,
                len,
            } => {
                write!(
                    f,
                    "virtio-vsock TX descriptor {index} at {address} with length {len} overflows address space"
                )
            }
            Self::DescriptorAccess {
                index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "virtio-vsock TX descriptor {index} at {address} with length {len} is not fully mapped: {source}"
                )
            }
            Self::ReadableLengthOverflow { descriptor_head } => {
                write!(
                    f,
                    "virtio-vsock TX descriptor chain headed by {descriptor_head} overflows readable length"
                )
            }
            Self::HeaderTooShort {
                descriptor_head,
                actual,
                min,
            } => {
                write!(
                    f,
                    "virtio-vsock TX descriptor chain headed by {descriptor_head} has {actual} readable bytes; expected at least {min} for the packet header"
                )
            }
            Self::ReadHeader {
                index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "failed to read {len} virtio-vsock TX header bytes from descriptor {index} at {address}: {source}"
                )
            }
            Self::InvalidHeaderLength { source } => {
                write!(f, "invalid virtio-vsock TX packet header length: {source}")
            }
            Self::PayloadTooShort {
                descriptor_head,
                required,
                available,
            } => {
                write!(
                    f,
                    "virtio-vsock TX descriptor chain headed by {descriptor_head} advertises {required} payload bytes but only {available} are readable after the header"
                )
            }
            Self::PayloadSegmentLengthOverflow { index, available } => {
                write!(
                    f,
                    "virtio-vsock TX payload segment in descriptor {index} has {available} readable bytes, which cannot fit in a segment length"
                )
            }
            Self::PayloadSegmentsAllocationFailed {
                descriptor_count,
                source,
            } => {
                write!(
                    f,
                    "failed to reserve virtio-vsock TX payload segment storage for {descriptor_count} descriptors: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioVsockTxPacketParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DescriptorAccess { source, .. } => Some(source),
            Self::ReadHeader { source, .. } => Some(source),
            Self::InvalidHeaderLength { source } => Some(source),
            Self::PayloadSegmentsAllocationFailed { source, .. } => Some(source),
            Self::DescriptorChainTooShort { .. }
            | Self::DescriptorWriteOnly { .. }
            | Self::DescriptorRangeOverflow { .. }
            | Self::ReadableLengthOverflow { .. }
            | Self::HeaderTooShort { .. }
            | Self::PayloadTooShort { .. }
            | Self::PayloadSegmentLengthOverflow { .. } => None,
        }
    }
}

fn validate_vsock_tx_descriptor_chain(
    memory: &GuestMemory,
    chain: &VirtqueueDescriptorChain,
) -> Result<(u16, u64), VirtioVsockTxPacketParseError> {
    let descriptor_head = chain
        .descriptors()
        .first()
        .copied()
        .ok_or(VirtioVsockTxPacketParseError::DescriptorChainTooShort {
            expected: 1,
            actual: 0,
        })?
        .index();

    let mut total_readable_len = 0_u64;
    for descriptor in chain.descriptors().iter().copied() {
        validate_vsock_tx_descriptor(memory, descriptor)?;
        total_readable_len = total_readable_len
            .checked_add(u64::from(descriptor.len()))
            .ok_or(VirtioVsockTxPacketParseError::ReadableLengthOverflow { descriptor_head })?;
    }

    Ok((descriptor_head, total_readable_len))
}

fn validate_vsock_tx_descriptor(
    memory: &GuestMemory,
    descriptor: VirtqueueDescriptor,
) -> Result<(), VirtioVsockTxPacketParseError> {
    if descriptor.is_write_only() {
        return Err(VirtioVsockTxPacketParseError::DescriptorWriteOnly {
            index: descriptor.index(),
        });
    }

    if descriptor.is_empty() {
        return Ok(());
    }

    let range =
        GuestMemoryRange::new(descriptor.address(), u64::from(descriptor.len())).map_err(|_| {
            VirtioVsockTxPacketParseError::DescriptorRangeOverflow {
                index: descriptor.index(),
                address: descriptor.address(),
                len: descriptor.len(),
            }
        })?;
    memory.validate_mapped_range(range).map_err(|source| {
        VirtioVsockTxPacketParseError::DescriptorAccess {
            index: descriptor.index(),
            address: descriptor.address(),
            len: descriptor.len(),
            source,
        }
    })
}

fn validate_vsock_tx_header_available(
    descriptor_head: u16,
    total_readable_len: u64,
) -> Result<(), VirtioVsockTxPacketParseError> {
    if total_readable_len < VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64 {
        return Err(VirtioVsockTxPacketParseError::HeaderTooShort {
            descriptor_head,
            actual: total_readable_len,
            min: VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64,
        });
    }

    Ok(())
}

fn read_vsock_tx_header_bytes(
    memory: &GuestMemory,
    chain: &VirtqueueDescriptorChain,
    descriptor_head: u16,
) -> Result<[u8; VIRTIO_VSOCK_PACKET_HEADER_SIZE], VirtioVsockTxPacketParseError> {
    let mut header = [0; VIRTIO_VSOCK_PACKET_HEADER_SIZE];
    let unread_header_len = fill_vsock_tx_header_bytes(memory, chain, &mut header)?;
    if unread_header_len != 0 {
        let actual = VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64
            - u64::try_from(unread_header_len).map_err(|_| {
                VirtioVsockTxPacketParseError::HeaderTooShort {
                    descriptor_head,
                    actual: 0,
                    min: VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64,
                }
            })?;
        return Err(VirtioVsockTxPacketParseError::HeaderTooShort {
            descriptor_head,
            actual,
            min: VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64,
        });
    }

    Ok(header)
}

fn fill_vsock_tx_header_bytes(
    memory: &GuestMemory,
    chain: &VirtqueueDescriptorChain,
    header: &mut [u8; VIRTIO_VSOCK_PACKET_HEADER_SIZE],
) -> Result<usize, VirtioVsockTxPacketParseError> {
    let mut remaining_header = header.as_mut_slice();
    for descriptor in chain.descriptors().iter().copied() {
        if remaining_header.is_empty() {
            return Ok(0);
        }

        let descriptor_len = usize::try_from(descriptor.len()).map_err(|_| {
            VirtioVsockTxPacketParseError::PayloadSegmentLengthOverflow {
                index: descriptor.index(),
                available: u64::from(descriptor.len()),
            }
        })?;
        let read_len = remaining_header.len().min(descriptor_len);
        if read_len == 0 {
            continue;
        }

        let (destination, next_remaining) = remaining_header.split_at_mut(read_len);
        memory
            .read_slice(destination, descriptor.address())
            .map_err(|source| VirtioVsockTxPacketParseError::ReadHeader {
                index: descriptor.index(),
                address: descriptor.address(),
                len: read_len,
                source,
            })?;
        remaining_header = next_remaining;
    }

    Ok(remaining_header.len())
}

fn read_vsock_tx_payload_bytes(
    memory: &GuestMemory,
    packet: &VirtioVsockTxPacket,
) -> Result<Vec<u8>, VsockGuestRwForwardError> {
    let payload_len = usize::try_from(packet.payload_len()).map_err(|_| {
        VsockGuestRwForwardError::PayloadLenTooLarge {
            len: packet.payload_len(),
        }
    })?;
    let mut payload = Vec::new();
    payload.try_reserve_exact(payload_len).map_err(|source| {
        VsockGuestRwForwardError::PayloadAllocationFailed {
            len: payload_len,
            source,
        }
    })?;

    for segment in packet.payload_segments().iter().copied() {
        let segment_len = usize::try_from(segment.len()).map_err(|_| {
            VsockGuestRwForwardError::PayloadSegmentTooLarge {
                descriptor_index: segment.descriptor_index(),
                len: segment.len(),
            }
        })?;
        let offset = payload.len();
        let Some(next_len) = offset.checked_add(segment_len) else {
            return Err(VsockGuestRwForwardError::PayloadSegmentTooLarge {
                descriptor_index: segment.descriptor_index(),
                len: segment.len(),
            });
        };
        if next_len > payload_len {
            return Err(VsockGuestRwForwardError::PayloadLenTooLarge {
                len: packet.payload_len(),
            });
        }
        payload.resize(next_len, 0);
        let Some(destination) = payload.get_mut(offset..next_len) else {
            return Err(VsockGuestRwForwardError::PayloadSegmentTooLarge {
                descriptor_index: segment.descriptor_index(),
                len: segment.len(),
            });
        };
        memory
            .read_slice(destination, segment.address())
            .map_err(|source| VsockGuestRwForwardError::PayloadRead {
                descriptor_index: segment.descriptor_index(),
                address: segment.address(),
                len: segment.len(),
                source,
            })?;
    }

    if payload.len() != payload_len {
        return Err(VsockGuestRwForwardError::PayloadLengthMismatch {
            expected: payload_len,
            actual: payload.len(),
        });
    }

    Ok(payload)
}

fn payload_len_u32(payload_len: usize) -> u32 {
    debug_assert!(payload_len <= VSOCK_HOST_RW_READ_LIMIT);
    payload_len as u32
}

fn validate_vsock_tx_payload_len(
    descriptor_head: u16,
    total_readable_len: u64,
    payload_len: u32,
) -> Result<(), VirtioVsockTxPacketParseError> {
    let available = total_readable_len - VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64;
    if u64::from(payload_len) > available {
        return Err(VirtioVsockTxPacketParseError::PayloadTooShort {
            descriptor_head,
            required: payload_len,
            available,
        });
    }

    Ok(())
}

fn vsock_tx_payload_segments(
    chain: &VirtqueueDescriptorChain,
    payload_len: u32,
) -> Result<Vec<VirtioVsockTxPayloadSegment>, VirtioVsockTxPacketParseError> {
    if payload_len == 0 {
        return Ok(Vec::new());
    }

    let mut segments = Vec::new();
    segments.try_reserve_exact(chain.len()).map_err(|source| {
        VirtioVsockTxPacketParseError::PayloadSegmentsAllocationFailed {
            descriptor_count: chain.len(),
            source,
        }
    })?;

    let mut header_remaining = VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64;
    let mut payload_remaining = payload_len;
    for descriptor in chain.descriptors().iter().copied() {
        if payload_remaining == 0 {
            break;
        }

        let descriptor_len = u64::from(descriptor.len());
        if header_remaining >= descriptor_len {
            header_remaining -= descriptor_len;
            continue;
        }

        let payload_offset = header_remaining;
        header_remaining = 0;
        let available = descriptor_len - payload_offset;
        let available = u32::try_from(available).map_err(|_| {
            VirtioVsockTxPacketParseError::PayloadSegmentLengthOverflow {
                index: descriptor.index(),
                available,
            }
        })?;
        let segment_len = payload_remaining.min(available);
        let segment_address = descriptor.address().checked_add(payload_offset).ok_or(
            VirtioVsockTxPacketParseError::DescriptorRangeOverflow {
                index: descriptor.index(),
                address: descriptor.address(),
                len: descriptor.len(),
            },
        )?;

        segments.push(VirtioVsockTxPayloadSegment::new(
            descriptor.index(),
            segment_address,
            segment_len,
        ));
        payload_remaining -= segment_len;
    }

    Ok(segments)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockConfigSpace {
    guest_cid: u64,
}

impl VirtioVsockConfigSpace {
    pub const fn new(guest_cid: u64) -> Self {
        Self { guest_cid }
    }

    pub const fn guest_cid(self) -> u64 {
        self.guest_cid
    }

    pub const fn available_features(self) -> u64 {
        virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_FEATURE_IN_ORDER)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX)
    }

    const fn guest_cid_bytes(self) -> [u8; VIRTIO_VSOCK_CONFIG_GUEST_CID_SIZE] {
        self.guest_cid.to_le_bytes()
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioVsockConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let bytes = self.guest_cid_bytes();
        let [b0, b1, b2, b3, b4, b5, b6, b7] = bytes;
        match (access.offset(), access.len()) {
            (0, 8) => MmioAccessBytes::new(&bytes),
            (0, 4) => MmioAccessBytes::new(&[b0, b1, b2, b3]),
            (4, 4) => MmioAccessBytes::new(&[b4, b5, b6, b7]),
            _ => {
                return Err(VirtioMmioDeviceConfigError::UnsupportedRead {
                    offset: access.offset(),
                    len: access.len(),
                });
            }
        }
        .map_err(config_bytes_error)
    }

    fn write_device_config(
        &mut self,
        access: VirtioMmioDeviceConfigAccess,
        _data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        Err(VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VirtioVsockRxBufferSegment {
    descriptor_index: u16,
    address: GuestAddress,
    len: u32,
}

impl VirtioVsockRxBufferSegment {
    const fn new(descriptor_index: u16, address: GuestAddress, len: u32) -> Self {
        Self {
            descriptor_index,
            address,
            len,
        }
    }

    const fn descriptor_index(self) -> u16 {
        self.descriptor_index
    }

    const fn address(self) -> GuestAddress {
        self.address
    }

    const fn len(self) -> u32 {
        self.len
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VirtioVsockRxBuffer {
    len: u64,
    segments: Vec<VirtioVsockRxBufferSegment>,
}

impl VirtioVsockRxBuffer {
    fn parse(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioVsockRxBufferParseError> {
        let mut len = 0;
        let mut segments = Vec::new();
        segments.try_reserve_exact(chain.len()).map_err(|source| {
            VirtioVsockRxBufferParseError::BufferSegmentsAllocationFailed {
                descriptor_count: chain.len(),
                source,
            }
        })?;

        for descriptor in chain.descriptors().iter().copied() {
            validate_vsock_rx_buffer_descriptor(&descriptor)?;
            len = push_vsock_rx_buffer_segment(
                memory,
                &mut segments,
                len,
                VirtioVsockRxBufferSegment::new(
                    descriptor.index(),
                    descriptor.address(),
                    descriptor.len(),
                ),
            )?;
        }

        Ok(Self { len, segments })
    }

    const fn len(&self) -> u64 {
        self.len
    }

    fn segments(&self) -> &[VirtioVsockRxBufferSegment] {
        &self.segments
    }
}

#[derive(Debug)]
pub enum VirtioVsockRxBufferParseError {
    BufferSegmentsAllocationFailed {
        descriptor_count: usize,
        source: std::collections::TryReserveError,
    },
    BufferDescriptorReadOnly {
        index: u16,
    },
    BufferDescriptorEmpty {
        index: u16,
    },
    BufferLengthOverflow {
        current: u64,
        len: u32,
    },
    BufferDescriptorRangeOverflow {
        index: u16,
        address: GuestAddress,
        len: u32,
    },
    BufferDescriptorAccess {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for VirtioVsockRxBufferParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferSegmentsAllocationFailed {
                descriptor_count,
                source,
            } => {
                write!(
                    f,
                    "failed to reserve virtio-vsock RX buffer segments for {descriptor_count} descriptors: {source}"
                )
            }
            Self::BufferDescriptorReadOnly { index } => {
                write!(
                    f,
                    "virtio-vsock RX buffer descriptor {index} is not writable"
                )
            }
            Self::BufferDescriptorEmpty { index } => {
                write!(f, "virtio-vsock RX buffer descriptor {index} is empty")
            }
            Self::BufferLengthOverflow { current, len } => {
                write!(
                    f,
                    "virtio-vsock RX buffer length overflows when adding descriptor length {len} to {current}"
                )
            }
            Self::BufferDescriptorRangeOverflow {
                index,
                address,
                len,
            } => {
                write!(
                    f,
                    "virtio-vsock RX buffer descriptor {index} at {address} with length {len} overflows address space"
                )
            }
            Self::BufferDescriptorAccess {
                index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "virtio-vsock RX buffer descriptor {index} at {address} with length {len} is not fully mapped: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioVsockRxBufferParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BufferSegmentsAllocationFailed { source, .. } => Some(source),
            Self::BufferDescriptorAccess { source, .. } => Some(source),
            Self::BufferDescriptorReadOnly { .. }
            | Self::BufferDescriptorEmpty { .. }
            | Self::BufferLengthOverflow { .. }
            | Self::BufferDescriptorRangeOverflow { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockRxBufferTooSmall {
    descriptor_head: u16,
    len: u64,
    required_len: u64,
}

impl VirtioVsockRxBufferTooSmall {
    pub const fn descriptor_head(self) -> u16 {
        self.descriptor_head
    }

    pub const fn buffer_len(self) -> u64 {
        self.len
    }

    pub const fn required_len(self) -> u64 {
        self.required_len
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockRxPacketDelivery {
    packet_kind: VirtioVsockRxPacketKind,
    descriptor_head: u16,
    bytes_written_to_guest: u32,
    payload_bytes_written_to_guest: usize,
}

impl VirtioVsockRxPacketDelivery {
    pub const fn packet_kind(self) -> VirtioVsockRxPacketKind {
        self.packet_kind
    }

    pub const fn descriptor_head(self) -> u16 {
        self.descriptor_head
    }

    pub const fn bytes_written_to_guest(self) -> u32 {
        self.bytes_written_to_guest
    }

    pub const fn payload_bytes_written_to_guest(self) -> usize {
        self.payload_bytes_written_to_guest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioVsockRxPacketKind {
    HostRequest,
    GuestResponse,
    GuestReset,
    HostRw,
}

#[derive(Debug)]
pub struct VirtioVsockRxQueueDispatch {
    processed_buffers: usize,
    delivered_requests: usize,
    delivered_responses: usize,
    delivered_reset_packets: usize,
    delivered_host_rw_packets: usize,
    delivered_host_rw_bytes: usize,
    buffer_parse_failures: usize,
    buffer_too_small_failures: usize,
    deliveries: Vec<VirtioVsockRxPacketDelivery>,
    first_buffer_parse_failure: Option<VirtioVsockRxBufferParseError>,
    first_buffer_too_small: Option<VirtioVsockRxBufferTooSmall>,
}

impl VirtioVsockRxQueueDispatch {
    const fn new() -> Self {
        Self {
            processed_buffers: 0,
            delivered_requests: 0,
            delivered_responses: 0,
            delivered_reset_packets: 0,
            delivered_host_rw_packets: 0,
            delivered_host_rw_bytes: 0,
            buffer_parse_failures: 0,
            buffer_too_small_failures: 0,
            deliveries: Vec::new(),
            first_buffer_parse_failure: None,
            first_buffer_too_small: None,
        }
    }

    fn with_delivery_capacity(
        delivery_capacity: usize,
    ) -> Result<Self, VirtioVsockRxQueueDispatchError> {
        let mut deliveries = Vec::new();
        deliveries
            .try_reserve_exact(delivery_capacity)
            .map_err(
                |source| VirtioVsockRxQueueDispatchError::PacketMetadataAllocation { source },
            )?;

        Ok(Self {
            processed_buffers: 0,
            delivered_requests: 0,
            delivered_responses: 0,
            delivered_reset_packets: 0,
            delivered_host_rw_packets: 0,
            delivered_host_rw_bytes: 0,
            buffer_parse_failures: 0,
            buffer_too_small_failures: 0,
            deliveries,
            first_buffer_parse_failure: None,
            first_buffer_too_small: None,
        })
    }

    pub const fn processed_buffers(&self) -> usize {
        self.processed_buffers
    }

    pub const fn delivered_requests(&self) -> usize {
        self.delivered_requests
    }

    pub const fn delivered_responses(&self) -> usize {
        self.delivered_responses
    }

    pub const fn delivered_reset_packets(&self) -> usize {
        self.delivered_reset_packets
    }

    pub const fn delivered_host_rw_packets(&self) -> usize {
        self.delivered_host_rw_packets
    }

    pub const fn delivered_host_rw_bytes(&self) -> usize {
        self.delivered_host_rw_bytes
    }

    pub const fn delivered_packets(&self, packet_kind: VirtioVsockRxPacketKind) -> usize {
        match packet_kind {
            VirtioVsockRxPacketKind::HostRequest => self.delivered_requests,
            VirtioVsockRxPacketKind::GuestResponse => self.delivered_responses,
            VirtioVsockRxPacketKind::GuestReset => self.delivered_reset_packets,
            VirtioVsockRxPacketKind::HostRw => self.delivered_host_rw_packets,
        }
    }

    pub const fn buffer_parse_failures(&self) -> usize {
        self.buffer_parse_failures
    }

    pub const fn buffer_too_small_failures(&self) -> usize {
        self.buffer_too_small_failures
    }

    pub fn deliveries(&self) -> &[VirtioVsockRxPacketDelivery] {
        &self.deliveries
    }

    pub const fn first_buffer_parse_failure(&self) -> Option<&VirtioVsockRxBufferParseError> {
        self.first_buffer_parse_failure.as_ref()
    }

    pub const fn first_buffer_too_small(&self) -> Option<VirtioVsockRxBufferTooSmall> {
        self.first_buffer_too_small
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.processed_buffers != 0
    }

    fn record(&mut self, outcome: VirtioVsockRxQueueDispatchOutcome) {
        self.processed_buffers += 1;
        match outcome {
            VirtioVsockRxQueueDispatchOutcome::Delivered(delivery) => {
                match delivery.packet_kind() {
                    VirtioVsockRxPacketKind::HostRequest => {
                        self.delivered_requests += 1;
                    }
                    VirtioVsockRxPacketKind::GuestResponse => {
                        self.delivered_responses += 1;
                    }
                    VirtioVsockRxPacketKind::GuestReset => {
                        self.delivered_reset_packets += 1;
                    }
                    VirtioVsockRxPacketKind::HostRw => {
                        self.delivered_host_rw_packets += 1;
                        self.delivered_host_rw_bytes += delivery.payload_bytes_written_to_guest();
                    }
                }
                self.deliveries.push(delivery);
            }
            VirtioVsockRxQueueDispatchOutcome::BufferParseError(source) => {
                self.buffer_parse_failures += 1;
                if self.first_buffer_parse_failure.is_none() {
                    self.first_buffer_parse_failure = Some(source);
                }
            }
            VirtioVsockRxQueueDispatchOutcome::BufferTooSmall(failure) => {
                self.buffer_too_small_failures += 1;
                if self.first_buffer_too_small.is_none() {
                    self.first_buffer_too_small = Some(failure);
                }
            }
        }
    }
}

#[derive(Debug)]
enum VirtioVsockRxQueueDispatchOutcome {
    Delivered(VirtioVsockRxPacketDelivery),
    BufferParseError(VirtioVsockRxBufferParseError),
    BufferTooSmall(VirtioVsockRxBufferTooSmall),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VirtioVsockRxPacket {
    packet_kind: VirtioVsockRxPacketKind,
    header: VirtioVsockPacketHeader,
    payload: Vec<u8>,
}

impl VirtioVsockRxPacket {
    fn header_only(packet_kind: VirtioVsockRxPacketKind, header: VirtioVsockPacketHeader) -> Self {
        debug_assert_eq!(header.payload_len(), 0);
        Self {
            packet_kind,
            header,
            payload: Vec::new(),
        }
    }

    fn new(
        packet_kind: VirtioVsockRxPacketKind,
        header: VirtioVsockPacketHeader,
        payload: Vec<u8>,
    ) -> Self {
        debug_assert_eq!(header.payload_len(), payload_len_u32(payload.len()));
        Self {
            packet_kind,
            header,
            payload,
        }
    }

    const fn packet_kind(&self) -> VirtioVsockRxPacketKind {
        self.packet_kind
    }

    const fn header(&self) -> VirtioVsockPacketHeader {
        self.header
    }

    fn payload(&self) -> &[u8] {
        &self.payload
    }

    fn required_len(&self) -> u64 {
        VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64 + u64::from(self.header.payload_len())
    }

    fn bytes_written_to_guest(&self) -> u32 {
        VIRTIO_VSOCK_PACKET_HEADER_SIZE_U32 + self.header.payload_len()
    }

    fn payload_bytes_written_to_guest(&self) -> usize {
        self.payload.len()
    }
}

#[derive(Debug)]
pub enum VirtioVsockRxPacketWriteError {
    SegmentOffsetTooLarge {
        descriptor_index: u16,
        offset: usize,
    },
    SegmentAddressOverflow {
        descriptor_index: u16,
        address: GuestAddress,
        offset: u64,
    },
    SegmentWrite {
        descriptor_index: u16,
        address: GuestAddress,
        len: usize,
        source: GuestMemoryAccessError,
    },
    IncompletePacket {
        remaining_bytes: usize,
    },
}

impl fmt::Display for VirtioVsockRxPacketWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SegmentOffsetTooLarge {
                descriptor_index,
                offset,
            } => {
                write!(
                    f,
                    "virtio-vsock RX buffer descriptor {descriptor_index} offset {offset} is too large"
                )
            }
            Self::SegmentAddressOverflow {
                descriptor_index,
                address,
                offset,
            } => {
                write!(
                    f,
                    "virtio-vsock RX buffer descriptor {descriptor_index} at {address} overflows when adding offset {offset}"
                )
            }
            Self::SegmentWrite {
                descriptor_index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "failed to write {len} bytes into virtio-vsock RX buffer descriptor {descriptor_index} at {address}: {source}"
                )
            }
            Self::IncompletePacket { remaining_bytes } => {
                write!(
                    f,
                    "virtio-vsock RX packet write finished with {remaining_bytes} bytes remaining"
                )
            }
        }
    }
}

impl std::error::Error for VirtioVsockRxPacketWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SegmentWrite { source, .. } => Some(source),
            Self::SegmentOffsetTooLarge { .. }
            | Self::SegmentAddressOverflow { .. }
            | Self::IncompletePacket { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioVsockRxQueueDispatchError {
    PacketMetadataAllocation {
        source: std::collections::TryReserveError,
    },
    AvailableRing {
        completed_dispatch: Box<VirtioVsockRxQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain {
        completed_dispatch: Box<VirtioVsockRxQueueDispatch>,
    },
    UsedRing {
        completed_dispatch: Box<VirtioVsockRxQueueDispatch>,
        descriptor_head: u16,
        bytes_written_to_guest: u32,
        source: VirtqueueUsedRingError,
    },
    BufferWrite {
        completed_dispatch: Box<VirtioVsockRxQueueDispatch>,
        descriptor_head: u16,
        source: VirtioVsockRxPacketWriteError,
    },
}

impl VirtioVsockRxQueueDispatchError {
    pub const fn completed_dispatch(&self) -> Option<&VirtioVsockRxQueueDispatch> {
        match self {
            Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            }
            | Self::BufferWrite {
                completed_dispatch, ..
            } => Some(completed_dispatch),
            Self::PacketMetadataAllocation { .. } => None,
        }
    }
}

impl fmt::Display for VirtioVsockRxQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketMetadataAllocation { source } => {
                write!(
                    f,
                    "failed to reserve virtio-vsock RX packet metadata: {source}"
                )
            }
            Self::AvailableRing { source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-vsock RX available descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain { .. } => {
                f.write_str("virtio-vsock RX queue produced an empty descriptor chain")
            }
            Self::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to publish virtio-vsock RX used descriptor head {descriptor_head} with length {bytes_written_to_guest}: {source}"
                )
            }
            Self::BufferWrite {
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to write virtio-vsock RX packet into descriptor head {descriptor_head}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioVsockRxQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PacketMetadataAllocation { source } => Some(source),
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::BufferWrite { source, .. } => Some(source),
            Self::EmptyDescriptorChain { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioVsockRxQueue {
    queue_state: VirtioMmioQueueState,
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
}

impl VirtioVsockRxQueue {
    pub fn from_mmio_queue_state(
        queue: VirtioMmioQueueState,
    ) -> Result<Self, VirtioVsockQueueBuildError> {
        validate_active_vsock_queue(queue)?;
        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioVsockQueueBuildError::AvailableRing { source })?;
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioVsockQueueBuildError::UsedRing { source })?;

        Ok(Self {
            queue_state: queue,
            available,
            used,
        })
    }

    pub const fn queue_state(&self) -> VirtioMmioQueueState {
        self.queue_state
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
    }

    pub fn dispatch_host_request(
        &mut self,
        memory: &mut GuestMemory,
        connections: &mut VsockHostConnectionTable,
        key: VsockHostConnectionKey,
        guest_cid: u32,
    ) -> Result<VirtioVsockRxQueueDispatch, VirtioVsockRxQueueDispatchError> {
        if !connections.has_pending_request_packet(key) {
            return Ok(VirtioVsockRxQueueDispatch::new());
        }

        let header = host_connection_request_packet_header(key, guest_cid);
        let packet = VirtioVsockRxPacket::header_only(VirtioVsockRxPacketKind::HostRequest, header);
        let dispatch = self.dispatch_packet(memory, &packet)?;
        if dispatch.delivered_requests() != 0 {
            let consumed = connections.take_pending_request_packet_header(key, guest_cid);
            debug_assert_eq!(consumed, Some(header));
        }

        Ok(dispatch)
    }

    fn dispatch_packet(
        &mut self,
        memory: &mut GuestMemory,
        packet: &VirtioVsockRxPacket,
    ) -> Result<VirtioVsockRxQueueDispatch, VirtioVsockRxQueueDispatchError> {
        let mut dispatch = VirtioVsockRxQueueDispatch::with_delivery_capacity(1)?;
        let Some(chain) = (match self.available.pop_descriptor_chain(memory) {
            Ok(chain) => chain,
            Err(source) => {
                return Err(VirtioVsockRxQueueDispatchError::AvailableRing {
                    completed_dispatch: Box::new(dispatch),
                    source,
                });
            }
        }) else {
            return Ok(dispatch);
        };

        let descriptor_head = match descriptor_chain_head(&chain) {
            Some(descriptor_head) => descriptor_head,
            None => {
                return Err(VirtioVsockRxQueueDispatchError::EmptyDescriptorChain {
                    completed_dispatch: Box::new(dispatch),
                });
            }
        };

        match VirtioVsockRxBuffer::parse(memory, &chain) {
            Ok(buffer) => {
                let required_len = packet.required_len();
                if required_len > buffer.len() {
                    if let Err(source) = self.used.publish_used_element(memory, descriptor_head, 0)
                    {
                        return Err(VirtioVsockRxQueueDispatchError::UsedRing {
                            completed_dispatch: Box::new(dispatch),
                            descriptor_head,
                            bytes_written_to_guest: 0,
                            source,
                        });
                    }
                    dispatch.record(VirtioVsockRxQueueDispatchOutcome::BufferTooSmall(
                        VirtioVsockRxBufferTooSmall {
                            descriptor_head,
                            len: buffer.len(),
                            required_len,
                        },
                    ));
                    return Ok(dispatch);
                }

                if let Err(source) = write_vsock_rx_packet(memory, &buffer, packet) {
                    return Err(VirtioVsockRxQueueDispatchError::BufferWrite {
                        completed_dispatch: Box::new(dispatch),
                        descriptor_head,
                        source,
                    });
                }
                let bytes_written_to_guest = packet.bytes_written_to_guest();
                if let Err(source) =
                    self.used
                        .publish_used_element(memory, descriptor_head, bytes_written_to_guest)
                {
                    return Err(VirtioVsockRxQueueDispatchError::UsedRing {
                        completed_dispatch: Box::new(dispatch),
                        descriptor_head,
                        bytes_written_to_guest,
                        source,
                    });
                }
                dispatch.record(VirtioVsockRxQueueDispatchOutcome::Delivered(
                    VirtioVsockRxPacketDelivery {
                        packet_kind: packet.packet_kind(),
                        descriptor_head,
                        bytes_written_to_guest,
                        payload_bytes_written_to_guest: packet.payload_bytes_written_to_guest(),
                    },
                ));
            }
            Err(source) => {
                if let Err(used_source) = self.used.publish_used_element(memory, descriptor_head, 0)
                {
                    return Err(VirtioVsockRxQueueDispatchError::UsedRing {
                        completed_dispatch: Box::new(dispatch),
                        descriptor_head,
                        bytes_written_to_guest: 0,
                        source: used_source,
                    });
                }
                dispatch.record(VirtioVsockRxQueueDispatchOutcome::BufferParseError(source));
            }
        }

        Ok(dispatch)
    }
}

fn validate_vsock_rx_buffer_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<(), VirtioVsockRxBufferParseError> {
    if !descriptor.is_write_only() {
        return Err(VirtioVsockRxBufferParseError::BufferDescriptorReadOnly {
            index: descriptor.index(),
        });
    }

    if descriptor.is_empty() {
        return Err(VirtioVsockRxBufferParseError::BufferDescriptorEmpty {
            index: descriptor.index(),
        });
    }

    Ok(())
}

fn push_vsock_rx_buffer_segment(
    memory: &GuestMemory,
    segments: &mut Vec<VirtioVsockRxBufferSegment>,
    len: u64,
    segment: VirtioVsockRxBufferSegment,
) -> Result<u64, VirtioVsockRxBufferParseError> {
    let next_len = len.checked_add(u64::from(segment.len())).ok_or(
        VirtioVsockRxBufferParseError::BufferLengthOverflow {
            current: len,
            len: segment.len(),
        },
    )?;

    validate_vsock_rx_buffer_segment_range(memory, segment)?;
    segments.push(segment);
    Ok(next_len)
}

fn validate_vsock_rx_buffer_segment_range(
    memory: &GuestMemory,
    segment: VirtioVsockRxBufferSegment,
) -> Result<(), VirtioVsockRxBufferParseError> {
    let range =
        GuestMemoryRange::new(segment.address(), u64::from(segment.len())).map_err(|_| {
            VirtioVsockRxBufferParseError::BufferDescriptorRangeOverflow {
                index: segment.descriptor_index(),
                address: segment.address(),
                len: segment.len(),
            }
        })?;

    memory.validate_mapped_range(range).map_err(|source| {
        VirtioVsockRxBufferParseError::BufferDescriptorAccess {
            index: segment.descriptor_index(),
            address: segment.address(),
            len: segment.len(),
            source,
        }
    })
}

fn write_vsock_rx_packet(
    memory: &mut GuestMemory,
    buffer: &VirtioVsockRxBuffer,
    packet: &VirtioVsockRxPacket,
) -> Result<(), VirtioVsockRxPacketWriteError> {
    let header = packet.header().to_bytes();
    write_vsock_rx_buffer_bytes(memory, buffer, 0, &header)?;
    write_vsock_rx_buffer_bytes(
        memory,
        buffer,
        VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64,
        packet.payload(),
    )
}

fn write_vsock_rx_buffer_bytes(
    memory: &mut GuestMemory,
    buffer: &VirtioVsockRxBuffer,
    mut buffer_offset: u64,
    mut bytes: &[u8],
) -> Result<(), VirtioVsockRxPacketWriteError> {
    for segment in buffer.segments() {
        if bytes.is_empty() {
            return Ok(());
        }

        let segment_len = u64::from(segment.len());
        if buffer_offset >= segment_len {
            buffer_offset -= segment_len;
            continue;
        }

        let available_len = segment_len - buffer_offset;
        let write_len_u64 = available_len.min(u64::try_from(bytes.len()).map_err(|_| {
            VirtioVsockRxPacketWriteError::SegmentOffsetTooLarge {
                descriptor_index: segment.descriptor_index(),
                offset: bytes.len(),
            }
        })?);
        let write_len = usize::try_from(write_len_u64).map_err(|_| {
            VirtioVsockRxPacketWriteError::SegmentOffsetTooLarge {
                descriptor_index: segment.descriptor_index(),
                offset: bytes.len(),
            }
        })?;
        let offset = usize::try_from(buffer_offset).map_err(|_| {
            VirtioVsockRxPacketWriteError::SegmentOffsetTooLarge {
                descriptor_index: segment.descriptor_index(),
                offset: usize::MAX,
            }
        })?;
        let (chunk, remaining) = bytes.split_at(write_len);
        write_vsock_rx_segment_bytes(memory, *segment, offset, chunk)?;
        bytes = remaining;
        buffer_offset = 0;
    }

    if bytes.is_empty() {
        Ok(())
    } else {
        Err(VirtioVsockRxPacketWriteError::IncompletePacket {
            remaining_bytes: bytes.len(),
        })
    }
}

fn write_vsock_rx_segment_bytes(
    memory: &mut GuestMemory,
    segment: VirtioVsockRxBufferSegment,
    offset: usize,
    bytes: &[u8],
) -> Result<(), VirtioVsockRxPacketWriteError> {
    if bytes.is_empty() {
        return Ok(());
    }

    let offset = u64::try_from(offset).map_err(|_| {
        VirtioVsockRxPacketWriteError::SegmentOffsetTooLarge {
            descriptor_index: segment.descriptor_index(),
            offset,
        }
    })?;
    let address = segment.address().checked_add(offset).ok_or(
        VirtioVsockRxPacketWriteError::SegmentAddressOverflow {
            descriptor_index: segment.descriptor_index(),
            address: segment.address(),
            offset,
        },
    )?;
    memory.write_slice(bytes, address).map_err(|source| {
        VirtioVsockRxPacketWriteError::SegmentWrite {
            descriptor_index: segment.descriptor_index(),
            address,
            len: bytes.len(),
            source,
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioVsockTxQueue {
    queue_state: VirtioMmioQueueState,
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
}

impl VirtioVsockTxQueue {
    pub fn from_mmio_queue_state(
        queue: VirtioMmioQueueState,
    ) -> Result<Self, VirtioVsockQueueBuildError> {
        validate_active_vsock_queue(queue)?;
        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioVsockQueueBuildError::AvailableRing { source })?;
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioVsockQueueBuildError::UsedRing { source })?;

        Ok(Self {
            queue_state: queue,
            available,
            used,
        })
    }

    pub const fn queue_state(&self) -> VirtioMmioQueueState {
        self.queue_state
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
    }

    pub fn dispatch(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioVsockTxQueueDispatch, VirtioVsockTxQueueDispatchError> {
        let mut dispatch = VirtioVsockTxQueueDispatch::with_capacity(self.available.queue_size())?;
        while let Some(chain) = match self.available.pop_descriptor_chain(memory) {
            Ok(chain) => chain,
            Err(source) => {
                return Err(VirtioVsockTxQueueDispatchError::AvailableRing {
                    completed_dispatch: Box::new(dispatch),
                    source,
                });
            }
        } {
            let descriptor_head = match descriptor_chain_head(&chain) {
                Some(descriptor_head) => descriptor_head,
                None => {
                    return Err(VirtioVsockTxQueueDispatchError::EmptyDescriptorChain {
                        completed_dispatch: Box::new(dispatch),
                    });
                }
            };

            let packet = VirtioVsockTxPacket::parse(memory, &chain);
            if let Err(source) = self.used.publish_used_element(memory, descriptor_head, 0) {
                return Err(VirtioVsockTxQueueDispatchError::UsedRing {
                    completed_dispatch: Box::new(dispatch),
                    descriptor_head,
                    bytes_written_to_guest: 0,
                    source,
                });
            }

            match packet {
                Ok(packet) => dispatch.record(VirtioVsockTxQueueDispatchOutcome::Ok(packet)),
                Err(source) => {
                    dispatch.record(VirtioVsockTxQueueDispatchOutcome::ParseError(source));
                }
            }
        }

        Ok(dispatch)
    }
}

#[derive(Debug)]
pub struct VirtioVsockTxQueueDispatch {
    processed_packets: usize,
    successful_packets: usize,
    parse_failures: usize,
    packets: Vec<VirtioVsockTxPacket>,
    first_parse_failure: Option<VirtioVsockTxPacketParseError>,
}

impl VirtioVsockTxQueueDispatch {
    fn with_capacity(queue_size: u16) -> Result<Self, VirtioVsockTxQueueDispatchError> {
        let mut packets = Vec::new();
        packets
            .try_reserve_exact(usize::from(queue_size))
            .map_err(
                |source| VirtioVsockTxQueueDispatchError::PacketMetadataAllocation { source },
            )?;

        Ok(Self {
            processed_packets: 0,
            successful_packets: 0,
            parse_failures: 0,
            packets,
            first_parse_failure: None,
        })
    }

    pub const fn processed_packets(&self) -> usize {
        self.processed_packets
    }

    pub const fn successful_packets(&self) -> usize {
        self.successful_packets
    }

    pub const fn parse_failures(&self) -> usize {
        self.parse_failures
    }

    pub fn packets(&self) -> &[VirtioVsockTxPacket] {
        &self.packets
    }

    pub const fn first_parse_failure(&self) -> Option<&VirtioVsockTxPacketParseError> {
        self.first_parse_failure.as_ref()
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.processed_packets != 0
    }

    fn record(&mut self, outcome: VirtioVsockTxQueueDispatchOutcome) {
        self.processed_packets += 1;
        match outcome {
            VirtioVsockTxQueueDispatchOutcome::Ok(packet) => {
                self.successful_packets += 1;
                self.packets.push(packet);
            }
            VirtioVsockTxQueueDispatchOutcome::ParseError(source) => {
                self.parse_failures += 1;
                if self.first_parse_failure.is_none() {
                    self.first_parse_failure = Some(source);
                }
            }
        }
    }
}

#[derive(Debug)]
enum VirtioVsockTxQueueDispatchOutcome {
    Ok(VirtioVsockTxPacket),
    ParseError(VirtioVsockTxPacketParseError),
}

#[derive(Debug)]
pub enum VirtioVsockTxQueueDispatchError {
    PacketMetadataAllocation {
        source: std::collections::TryReserveError,
    },
    AvailableRing {
        completed_dispatch: Box<VirtioVsockTxQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain {
        completed_dispatch: Box<VirtioVsockTxQueueDispatch>,
    },
    UsedRing {
        completed_dispatch: Box<VirtioVsockTxQueueDispatch>,
        descriptor_head: u16,
        bytes_written_to_guest: u32,
        source: VirtqueueUsedRingError,
    },
}

impl VirtioVsockTxQueueDispatchError {
    pub const fn completed_dispatch(&self) -> Option<&VirtioVsockTxQueueDispatch> {
        match self {
            Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            } => Some(completed_dispatch),
            Self::PacketMetadataAllocation { .. } => None,
        }
    }
}

impl fmt::Display for VirtioVsockTxQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketMetadataAllocation { source } => {
                write!(
                    f,
                    "failed to reserve virtio-vsock TX packet metadata: {source}"
                )
            }
            Self::AvailableRing { source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-vsock TX available descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain { .. } => {
                f.write_str("virtio-vsock TX queue produced an empty descriptor chain")
            }
            Self::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to publish virtio-vsock TX used descriptor head {descriptor_head} with length {bytes_written_to_guest}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioVsockTxQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PacketMetadataAllocation { source } => Some(source),
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::EmptyDescriptorChain { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockEventQueue {
    queue_state: VirtioMmioQueueState,
}

impl VirtioVsockEventQueue {
    pub fn from_mmio_queue_state(
        queue: VirtioMmioQueueState,
    ) -> Result<Self, VirtioVsockQueueBuildError> {
        validate_active_vsock_queue(queue)?;
        Ok(Self { queue_state: queue })
    }

    pub const fn queue_state(self) -> VirtioMmioQueueState {
        self.queue_state
    }
}

#[derive(Debug)]
pub enum VirtioVsockQueueBuildError {
    QueueNotReady,
    QueueSizeNotConfigured,
    AvailableRing { source: VirtqueueAvailableRingError },
    UsedRing { source: VirtqueueUsedRingError },
}

impl fmt::Display for VirtioVsockQueueBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("virtio-vsock queue is not ready"),
            Self::QueueSizeNotConfigured => {
                f.write_str("virtio-vsock queue size is not configured")
            }
            Self::AvailableRing { source } => {
                write!(f, "failed to build virtio-vsock available ring: {source}")
            }
            Self::UsedRing { source } => {
                write!(f, "failed to build virtio-vsock used ring: {source}")
            }
        }
    }
}

impl std::error::Error for VirtioVsockQueueBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source } => Some(source),
            Self::QueueNotReady | Self::QueueSizeNotConfigured => None,
        }
    }
}

fn descriptor_chain_head(chain: &VirtqueueDescriptorChain) -> Option<u16> {
    chain
        .descriptors()
        .first()
        .map(|descriptor| descriptor.index())
}

fn validate_active_vsock_queue(
    queue: VirtioMmioQueueState,
) -> Result<(), VirtioVsockQueueBuildError> {
    if !queue.ready() {
        return Err(VirtioVsockQueueBuildError::QueueNotReady);
    }
    if queue.size() == 0 {
        return Err(VirtioVsockQueueBuildError::QueueSizeNotConfigured);
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioVsockPendingRxPacket {
    GuestReset {
        header: VirtioVsockPacketHeader,
    },
    GuestResponse {
        key: VsockGuestConnectionKey,
        header: VirtioVsockPacketHeader,
    },
    HostRequest {
        key: VsockHostConnectionKey,
        header: VirtioVsockPacketHeader,
    },
    GuestConnectionRw {
        key: VsockGuestConnectionKey,
    },
    HostConnectionRw {
        key: VsockHostConnectionKey,
    },
}

impl VirtioVsockPendingRxPacket {
    fn header_only_rx_packet(
        packet_kind: VirtioVsockRxPacketKind,
        header: VirtioVsockPacketHeader,
    ) -> VirtioVsockRxPacket {
        VirtioVsockRxPacket::header_only(packet_kind, header)
    }

    fn rx_packet(self, device: &VirtioVsockDevice) -> Option<VirtioVsockRxPacket> {
        match self {
            Self::GuestReset { header } => Some(Self::header_only_rx_packet(
                VirtioVsockRxPacketKind::GuestReset,
                header,
            )),
            Self::GuestResponse { header, .. } => Some(Self::header_only_rx_packet(
                VirtioVsockRxPacketKind::GuestResponse,
                header,
            )),
            Self::HostRequest { header, .. } => Some(Self::header_only_rx_packet(
                VirtioVsockRxPacketKind::HostRequest,
                header,
            )),
            Self::GuestConnectionRw { key } => device
                .guest_connections
                .pending_host_rw_packet(key, device.guest_cid),
            Self::HostConnectionRw { key } => device
                .host_connections
                .pending_host_rw_packet(key, device.guest_cid),
        }
    }

    const fn packet_kind(self) -> VirtioVsockRxPacketKind {
        match self {
            Self::GuestReset { .. } => VirtioVsockRxPacketKind::GuestReset,
            Self::GuestResponse { .. } => VirtioVsockRxPacketKind::GuestResponse,
            Self::HostRequest { .. } => VirtioVsockRxPacketKind::HostRequest,
            Self::GuestConnectionRw { .. } | Self::HostConnectionRw { .. } => {
                VirtioVsockRxPacketKind::HostRw
            }
        }
    }
}

#[derive(Debug)]
pub struct VirtioVsockDevice {
    guest_cid: u32,
    active_rx_queue: Option<VirtioVsockRxQueue>,
    active_tx_queue: Option<VirtioVsockTxQueue>,
    active_event_queue: Option<VirtioVsockEventQueue>,
    host_socket_path: Option<PathBuf>,
    host_socket_owner: Option<VsockHostSocketOwner>,
    pending_host_connections: VecDeque<VsockHostAcceptedConnection>,
    host_connection_limit: usize,
    host_connections: VsockHostConnectionTable,
    guest_connections: VsockGuestConnectionTable,
    pending_guest_reset_packets: VecDeque<VirtioVsockPacketHeader>,
}

impl VirtioVsockDevice {
    pub fn new() -> Self {
        Self::default()
    }

    fn with_guest_cid(guest_cid: u32) -> Self {
        Self {
            guest_cid,
            active_rx_queue: None,
            active_tx_queue: None,
            active_event_queue: None,
            host_socket_path: None,
            host_socket_owner: None,
            pending_host_connections: VecDeque::new(),
            host_connection_limit: VSOCK_HOST_CONNECTION_LIMIT,
            host_connections: VsockHostConnectionTable::new(),
            guest_connections: VsockGuestConnectionTable::new(),
            pending_guest_reset_packets: VecDeque::new(),
        }
    }

    pub(crate) fn with_host_socket_owner(
        guest_cid: u32,
        host_socket_owner: VsockHostSocketOwner,
    ) -> Self {
        let host_socket_path = Some(host_socket_owner.path.clone());
        Self {
            guest_cid,
            host_socket_path,
            host_socket_owner: Some(host_socket_owner),
            ..Self::with_guest_cid(guest_cid)
        }
    }

    fn set_host_socket_path(&mut self, path: impl AsRef<Path>) {
        self.host_socket_path = Some(path.as_ref().to_path_buf());
    }

    pub fn is_activated(&self) -> bool {
        self.active_rx_queue.is_some()
            && self.active_tx_queue.is_some()
            && self.active_event_queue.is_some()
    }

    pub fn active_rx_queue(&self) -> Option<VirtioMmioQueueState> {
        self.active_rx_queue
            .as_ref()
            .map(VirtioVsockRxQueue::queue_state)
    }

    pub const fn active_rx_dispatch_queue(&self) -> Option<&VirtioVsockRxQueue> {
        self.active_rx_queue.as_ref()
    }

    pub fn active_tx_queue(&self) -> Option<VirtioMmioQueueState> {
        self.active_tx_queue
            .as_ref()
            .map(VirtioVsockTxQueue::queue_state)
    }

    pub const fn active_tx_dispatch_queue(&self) -> Option<&VirtioVsockTxQueue> {
        self.active_tx_queue.as_ref()
    }

    pub fn active_event_queue(&self) -> Option<VirtioMmioQueueState> {
        self.active_event_queue
            .map(VirtioVsockEventQueue::queue_state)
    }

    pub const fn active_event_dispatch_queue(&self) -> Option<&VirtioVsockEventQueue> {
        self.active_event_queue.as_ref()
    }

    /// Accepts one pending host connection from the owned listener.
    ///
    /// Returns `Ok(None)` when this call did not produce an accepted stream,
    /// including transient listener errors that are safe to retry.
    pub fn accept_host_connection(
        &self,
    ) -> Result<Option<VsockHostAcceptedConnection>, VsockHostSocketAcceptError> {
        self.host_socket_owner
            .as_ref()
            .ok_or(VsockHostSocketAcceptError::HostSocketNotAttached)?
            .accept_host_connection()
    }

    #[cfg(test)]
    fn insert_accepted_host_connection(
        &mut self,
        accepted: VsockHostAcceptedConnection,
        request: VsockHostConnectRequest,
    ) -> Result<VsockHostConnectionKey, VsockHostConnectionTableError> {
        self.host_connections
            .insert_accepted_host_connection(accepted, request)
    }

    #[cfg(test)]
    fn has_pending_host_request_packet(&self, key: VsockHostConnectionKey) -> bool {
        self.host_connections.has_pending_request_packet(key)
    }

    #[cfg(test)]
    fn has_host_connection(&self, key: VsockHostConnectionKey) -> bool {
        self.host_connections.contains(key)
    }

    #[cfg(test)]
    fn pending_host_connection_count(&self) -> usize {
        self.pending_host_connections.len()
    }

    #[cfg(test)]
    fn pending_guest_reset_packet_count(&self) -> usize {
        self.pending_guest_reset_packets.len()
    }

    #[cfg(test)]
    fn pending_guest_connection_count(&self) -> usize {
        self.guest_connections.len()
    }

    #[cfg(test)]
    fn has_guest_connection(&self, key: VsockGuestConnectionKey) -> bool {
        self.guest_connections.contains(key)
    }

    #[cfg(test)]
    fn has_pending_guest_response_packet(&self, key: VsockGuestConnectionKey) -> bool {
        self.guest_connections.has_pending_response_packet(key)
    }

    #[cfg(test)]
    fn set_host_connection_limit(&mut self, limit: usize) {
        self.host_connection_limit = limit;
    }

    #[cfg(test)]
    fn set_guest_connection_limit(&mut self, limit: usize) {
        self.guest_connections = VsockGuestConnectionTable::with_limit(limit);
    }

    pub fn activate_vsock(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioVsockDeviceActivationError> {
        if self.is_activated() {
            return Err(VirtioVsockDeviceActivationError::AlreadyActive);
        }

        if activation.queue_count() != VIRTIO_VSOCK_QUEUE_COUNT {
            return Err(VirtioVsockDeviceActivationError::QueueCountMismatch {
                expected: VIRTIO_VSOCK_QUEUE_COUNT,
                got: activation.queue_count(),
            });
        }

        let active_rx_queue = active_vsock_queue_state(activation, VIRTIO_VSOCK_RX_QUEUE_INDEX_U32)
            .and_then(|queue| {
                VirtioVsockRxQueue::from_mmio_queue_state(queue).map_err(|source| {
                    VirtioVsockDeviceActivationError::RxQueueBuild {
                        queue_index: VIRTIO_VSOCK_RX_QUEUE_INDEX_U32,
                        source,
                    }
                })
            })?;
        let active_tx_queue = active_vsock_queue_state(activation, VIRTIO_VSOCK_TX_QUEUE_INDEX_U32)
            .and_then(|queue| {
                VirtioVsockTxQueue::from_mmio_queue_state(queue).map_err(|source| {
                    VirtioVsockDeviceActivationError::TxQueueBuild {
                        queue_index: VIRTIO_VSOCK_TX_QUEUE_INDEX_U32,
                        source,
                    }
                })
            })?;
        let active_event_queue =
            active_vsock_queue_state(activation, VIRTIO_VSOCK_EVENT_QUEUE_INDEX_U32).and_then(
                |queue| {
                    VirtioVsockEventQueue::from_mmio_queue_state(queue).map_err(|source| {
                        VirtioVsockDeviceActivationError::EventQueueBuild {
                            queue_index: VIRTIO_VSOCK_EVENT_QUEUE_INDEX_U32,
                            source,
                        }
                    })
                },
            )?;

        self.active_rx_queue = Some(active_rx_queue);
        self.active_tx_queue = Some(active_tx_queue);
        self.active_event_queue = Some(active_event_queue);
        Ok(())
    }

    pub fn reset(&mut self) {
        self.active_rx_queue = None;
        self.active_tx_queue = None;
        self.active_event_queue = None;
        self.pending_host_connections.clear();
        self.host_connections = VsockHostConnectionTable::new();
        self.guest_connections = VsockGuestConnectionTable::new();
        self.pending_guest_reset_packets.clear();
    }

    fn first_pending_rx_packet(&self) -> Option<VirtioVsockPendingRxPacket> {
        if let Some(header) = self.pending_guest_reset_packets.front().copied() {
            return Some(VirtioVsockPendingRxPacket::GuestReset { header });
        }

        if let Some(key) = self.guest_connections.first_pending_response_packet_key() {
            return Some(VirtioVsockPendingRxPacket::GuestResponse {
                key,
                header: guest_connection_response_packet_header(key, self.guest_cid),
            });
        }

        self.host_connections
            .first_pending_request_packet_key()
            .map(|key| VirtioVsockPendingRxPacket::HostRequest {
                key,
                header: host_connection_request_packet_header(key, self.guest_cid),
            })
            .or_else(|| {
                self.guest_connections
                    .first_pending_host_rw_packet_key()
                    .map(|key| VirtioVsockPendingRxPacket::GuestConnectionRw { key })
            })
            .or_else(|| {
                self.host_connections
                    .first_pending_host_rw_packet_key()
                    .map(|key| VirtioVsockPendingRxPacket::HostConnectionRw { key })
            })
    }

    fn consume_pending_rx_packet(&mut self, packet: VirtioVsockPendingRxPacket) {
        match packet {
            VirtioVsockPendingRxPacket::GuestReset { header } => {
                let removed = self.pending_guest_reset_packets.pop_front();
                debug_assert_eq!(removed, Some(header));
            }
            VirtioVsockPendingRxPacket::GuestResponse { key, header } => {
                let consumed = self
                    .guest_connections
                    .take_pending_response_packet_header(key, self.guest_cid);
                debug_assert_eq!(consumed, Some(header));
            }
            VirtioVsockPendingRxPacket::HostRequest { key, header } => {
                let consumed = self
                    .host_connections
                    .take_pending_request_packet_header(key, self.guest_cid);
                debug_assert_eq!(consumed, Some(header));
            }
            VirtioVsockPendingRxPacket::GuestConnectionRw { key } => {
                let consumed = self.guest_connections.take_pending_host_rw_payload(key);
                debug_assert!(consumed.is_some());
            }
            VirtioVsockPendingRxPacket::HostConnectionRw { key } => {
                let consumed = self.host_connections.take_pending_host_rw_payload(key);
                debug_assert!(consumed.is_some());
            }
        }
    }

    fn dispatch_next_rx_packet(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioVsockRxQueueDispatch, VirtioVsockRxQueueDispatchError> {
        let Some(packet) = self.first_pending_rx_packet() else {
            return Ok(VirtioVsockRxQueueDispatch::new());
        };
        let Some(rx_packet) = packet.rx_packet(self) else {
            return Ok(VirtioVsockRxQueueDispatch::new());
        };
        let Some(queue) = self.active_rx_queue.as_mut() else {
            return Ok(VirtioVsockRxQueueDispatch::new());
        };

        let dispatch = queue.dispatch_packet(memory, &rx_packet)?;
        if dispatch.delivered_packets(packet.packet_kind()) != 0 {
            self.consume_pending_rx_packet(packet);
        }

        Ok(dispatch)
    }

    fn poll_host_rw_payloads(&mut self) {
        if self.active_rx_queue.is_none() {
            return;
        }
        if self.guest_connections.is_empty() && self.host_connections.is_empty() {
            return;
        }

        let mut scratch = vec![0; VSOCK_HOST_RW_READ_LIMIT];
        let mut reset_headers = self
            .guest_connections
            .poll_host_rw_payloads(&mut scratch, self.guest_cid);
        reset_headers.extend(
            self.host_connections
                .poll_host_rw_payloads(&mut scratch, self.guest_cid),
        );

        for header in reset_headers {
            let _ = self.queue_guest_reset_packet(header);
        }
    }

    fn queue_guest_reset_packet(&mut self, header: VirtioVsockPacketHeader) -> bool {
        if self.pending_guest_reset_packets.len() >= usize::from(VIRTIO_VSOCK_QUEUE_SIZE) {
            return false;
        }

        self.pending_guest_reset_packets.push_back(header);
        true
    }

    fn poll_host_request_connections(&mut self) -> VirtioVsockHostRequestDispatch {
        let mut dispatch = VirtioVsockHostRequestDispatch::new();

        let retained_connections = self
            .host_connections
            .len()
            .saturating_add(self.pending_host_connections.len());
        if retained_connections < self.host_connection_limit
            && let Some(host_socket_owner) = self.host_socket_owner.as_ref()
        {
            match host_socket_owner.accept_host_connection() {
                Ok(Some(accepted)) => {
                    self.pending_host_connections.push_back(accepted);
                    dispatch.accepted_connections += 1;
                }
                Ok(None) => {}
                Err(_) => {
                    dispatch.dropped_connections += 1;
                }
            }
        }

        let pending_to_poll = self.pending_host_connections.len();
        for _ in 0..pending_to_poll {
            let Some(mut accepted) = self.pending_host_connections.pop_front() else {
                break;
            };

            match accepted.read_connect_request() {
                Ok(Some(request)) => {
                    if self.host_connections.len() >= self.host_connection_limit {
                        dispatch.dropped_connections += 1;
                        continue;
                    }
                    match self
                        .host_connections
                        .insert_accepted_host_connection(accepted, request)
                    {
                        Ok(_) => {
                            dispatch.completed_requests += 1;
                        }
                        Err(_) => {
                            dispatch.dropped_connections += 1;
                        }
                    }
                }
                Ok(None) => {
                    self.pending_host_connections.push_back(accepted);
                }
                Err(_) => {
                    dispatch.dropped_connections += 1;
                }
            }
        }

        dispatch.pending_connections = self.pending_host_connections.len();
        dispatch
    }

    fn connect_guest_connection_request_packet(
        &mut self,
        packet: &VirtioVsockTxPacket,
    ) -> Option<VsockGuestConnectionRequestOutcome> {
        let header = packet.header();
        if header.operation() != VIRTIO_VSOCK_OP_REQUEST {
            return None;
        }
        if header.src_cid() != u64::from(self.guest_cid) {
            return Some(VsockGuestConnectionRequestOutcome::Ignored {
                reason: VsockGuestConnectionRequestIgnoreReason::WrongSourceCid,
            });
        }
        if header.dst_cid() != VIRTIO_VSOCK_HOST_CID {
            return Some(VsockGuestConnectionRequestOutcome::Ignored {
                reason: VsockGuestConnectionRequestIgnoreReason::WrongDestinationCid,
            });
        }
        if header.packet_type() != VIRTIO_VSOCK_PACKET_TYPE_STREAM {
            return Some(VsockGuestConnectionRequestOutcome::Ignored {
                reason: VsockGuestConnectionRequestIgnoreReason::UnsupportedPacketType,
            });
        }
        if header.payload_len() != 0 {
            return Some(VsockGuestConnectionRequestOutcome::Ignored {
                reason: VsockGuestConnectionRequestIgnoreReason::PayloadPresent,
            });
        }

        let key = VsockGuestConnectionKey::new(header.dst_port(), header.src_port());
        if let Err(source) = self.guest_connections.check_insert(key) {
            return Some(VsockGuestConnectionRequestOutcome::Dropped {
                key,
                source: VsockGuestConnectionRequestError::Table(source),
            });
        }

        let Some(host_socket_path) = self.host_socket_path.as_ref() else {
            return Some(VsockGuestConnectionRequestOutcome::Dropped {
                key,
                source: VsockGuestConnectionRequestError::MissingHostSocketPath,
            });
        };
        let path = guest_connection_socket_path(host_socket_path, key.host_port());
        let stream = match nonblocking_unix_stream_connect(&path) {
            Ok(stream) => stream,
            Err(err) => {
                return Some(VsockGuestConnectionRequestOutcome::Dropped {
                    key,
                    source: VsockGuestConnectionRequestError::Connect(err.kind()),
                });
            }
        };

        match self
            .guest_connections
            .insert_connected_guest_connection(key, stream)
        {
            Ok(()) => Some(VsockGuestConnectionRequestOutcome::Retained { key }),
            Err(source) => Some(VsockGuestConnectionRequestOutcome::Dropped {
                key,
                source: VsockGuestConnectionRequestError::Table(source),
            }),
        }
    }

    fn dispatch_guest_tx_control_packets(
        &mut self,
        memory: &GuestMemory,
        tx_dispatch: &VirtioVsockTxQueueDispatch,
    ) -> VirtioVsockGuestTxControlDispatch {
        let mut response_dispatch = VirtioVsockGuestResponseDispatch::new();
        let mut request_dispatch = VirtioVsockGuestRequestDispatch::new();
        let mut rw_dispatch = VirtioVsockGuestRwDispatch::new();
        let mut reset_dispatch = VirtioVsockGuestResetDispatch::new();

        for packet in tx_dispatch.packets() {
            let response_outcome = self
                .host_connections
                .acknowledge_guest_response_packet(packet, self.guest_cid);
            if let Some(outcome) = response_outcome {
                response_dispatch.record(outcome);
            }

            if response_outcome.is_some_and(VsockHostGuestResponseOutcome::suppresses_guest_reset) {
                continue;
            }

            let request_outcome = self.connect_guest_connection_request_packet(packet);
            if let Some(outcome) = request_outcome {
                request_dispatch.record(outcome);
            }

            if request_outcome
                .is_some_and(VsockGuestConnectionRequestOutcome::suppresses_guest_reset)
            {
                continue;
            }

            let rw_outcome =
                self.guest_connections
                    .forward_guest_rw_packet(memory, packet, self.guest_cid);
            if let Some(outcome) = rw_outcome.as_ref() {
                rw_dispatch.record(outcome);
            }

            if rw_outcome
                .as_ref()
                .is_some_and(VsockGuestRwOutcome::suppresses_guest_reset)
            {
                continue;
            }

            if let Some(header) = guest_reset_packet_header_for_tx_packet(packet, self.guest_cid) {
                let queued = self.queue_guest_reset_packet(header);
                reset_dispatch.record(queued);
            }
        }

        VirtioVsockGuestTxControlDispatch::new(
            response_dispatch,
            request_dispatch,
            rw_dispatch,
            reset_dispatch,
        )
    }

    fn dispatch_drained_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
    ) -> Result<VirtioVsockDeviceNotificationDispatch, VirtioVsockDeviceNotificationError> {
        if !self.is_activated() {
            if drained_notifications.is_empty() {
                return Ok(VirtioVsockDeviceNotificationDispatch::new(
                    drained_notifications,
                    VirtioVsockHostRequestDispatch::new(),
                    VirtioVsockGuestTxControlDispatch::empty(),
                    None,
                    None,
                ));
            }
            return Err(VirtioVsockDeviceNotificationError::Inactive {
                drained_notifications,
            });
        }

        if let Some(queue_index) = drained_notifications.iter().copied().find(|queue_index| {
            *queue_index != VIRTIO_VSOCK_RX_QUEUE_INDEX
                && *queue_index != VIRTIO_VSOCK_TX_QUEUE_INDEX
        }) {
            return Err(VirtioVsockDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        let host_request_dispatch = self.poll_host_request_connections();
        let dispatch_tx = drained_notifications
            .iter()
            .copied()
            .any(|queue_index| queue_index == VIRTIO_VSOCK_TX_QUEUE_INDEX);
        if !dispatch_tx {
            self.poll_host_rw_payloads();
        }

        let dispatch_rx = drained_notifications
            .iter()
            .copied()
            .any(|queue_index| queue_index == VIRTIO_VSOCK_RX_QUEUE_INDEX)
            || self.first_pending_rx_packet().is_some();

        if !dispatch_rx && !dispatch_tx {
            return Ok(VirtioVsockDeviceNotificationDispatch::new(
                drained_notifications,
                host_request_dispatch,
                VirtioVsockGuestTxControlDispatch::empty(),
                None,
                None,
            ));
        }

        let mut rx_queue_dispatch = if dispatch_rx {
            match self.dispatch_next_rx_packet(memory) {
                Ok(dispatch) => Some(dispatch),
                Err(source) => {
                    return Err(VirtioVsockDeviceNotificationError::RxQueueDispatch {
                        drained_notifications,
                        completed_tx_dispatch: None,
                        source,
                    });
                }
            }
        } else {
            None
        };

        let (tx_queue_dispatch, guest_tx_control_dispatch) = if dispatch_tx {
            let Some(queue) = self.active_tx_queue.as_mut() else {
                return Err(VirtioVsockDeviceNotificationError::Inactive {
                    drained_notifications,
                });
            };

            match queue.dispatch(memory) {
                Ok(dispatch) => {
                    let guest_tx_control_dispatch =
                        self.dispatch_guest_tx_control_packets(memory, &dispatch);
                    self.poll_host_rw_payloads();
                    (Some(dispatch), guest_tx_control_dispatch)
                }
                Err(source) => {
                    return Err(VirtioVsockDeviceNotificationError::TxQueueDispatch {
                        drained_notifications,
                        completed_rx_dispatch: rx_queue_dispatch.map(Box::new),
                        source,
                    });
                }
            }
        } else {
            (None, VirtioVsockGuestTxControlDispatch::empty())
        };

        if self.first_pending_rx_packet().is_some()
            && rx_queue_dispatch
                .as_ref()
                .is_none_or(|dispatch| dispatch.processed_buffers() == 0)
        {
            match self.dispatch_next_rx_packet(memory) {
                Ok(dispatch) => {
                    rx_queue_dispatch = Some(dispatch);
                }
                Err(source) => {
                    return Err(VirtioVsockDeviceNotificationError::RxQueueDispatch {
                        drained_notifications,
                        completed_tx_dispatch: tx_queue_dispatch.map(Box::new),
                        source,
                    });
                }
            }
        }

        Ok(VirtioVsockDeviceNotificationDispatch::new(
            drained_notifications,
            host_request_dispatch,
            guest_tx_control_dispatch,
            rx_queue_dispatch,
            tx_queue_dispatch,
        ))
    }
}

impl Default for VirtioVsockDevice {
    fn default() -> Self {
        Self::with_guest_cid(MIN_GUEST_CID)
    }
}

#[derive(Debug)]
pub enum VirtioVsockDeviceActivationError {
    AlreadyActive,
    QueueCountMismatch {
        expected: usize,
        got: usize,
    },
    QueueMetadata {
        queue_index: u32,
        source: VirtioMmioQueueRegisterError,
    },
    RxQueueBuild {
        queue_index: u32,
        source: VirtioVsockQueueBuildError,
    },
    TxQueueBuild {
        queue_index: u32,
        source: VirtioVsockQueueBuildError,
    },
    EventQueueBuild {
        queue_index: u32,
        source: VirtioVsockQueueBuildError,
    },
}

impl fmt::Display for VirtioVsockDeviceActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("virtio-vsock device is already active"),
            Self::QueueCountMismatch { expected, got } => {
                write!(f, "virtio-vsock expected {expected} queues, got {got}")
            }
            Self::QueueMetadata {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to read virtio-vsock queue {queue_index} activation metadata: {source}"
                )
            }
            Self::RxQueueBuild {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to activate virtio-vsock RX queue {queue_index}: {source}"
                )
            }
            Self::TxQueueBuild {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to activate virtio-vsock TX queue {queue_index}: {source}"
                )
            }
            Self::EventQueueBuild {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to activate virtio-vsock event queue {queue_index}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioVsockDeviceActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueMetadata { source, .. } => Some(source),
            Self::RxQueueBuild { source, .. }
            | Self::TxQueueBuild { source, .. }
            | Self::EventQueueBuild { source, .. } => Some(source),
            Self::AlreadyActive | Self::QueueCountMismatch { .. } => None,
        }
    }
}

impl From<VirtioVsockDeviceActivationError> for VirtioMmioDeviceActivationError {
    fn from(source: VirtioVsockDeviceActivationError) -> Self {
        MmioHandlerError::new(source.to_string()).into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VirtioVsockGuestTxControlDispatch {
    response_dispatch: VirtioVsockGuestResponseDispatch,
    request_dispatch: VirtioVsockGuestRequestDispatch,
    rw_dispatch: VirtioVsockGuestRwDispatch,
    reset_dispatch: VirtioVsockGuestResetDispatch,
}

impl VirtioVsockGuestTxControlDispatch {
    const fn empty() -> Self {
        Self::new(
            VirtioVsockGuestResponseDispatch::new(),
            VirtioVsockGuestRequestDispatch::new(),
            VirtioVsockGuestRwDispatch::new(),
            VirtioVsockGuestResetDispatch::new(),
        )
    }

    const fn new(
        response_dispatch: VirtioVsockGuestResponseDispatch,
        request_dispatch: VirtioVsockGuestRequestDispatch,
        rw_dispatch: VirtioVsockGuestRwDispatch,
        reset_dispatch: VirtioVsockGuestResetDispatch,
    ) -> Self {
        Self {
            response_dispatch,
            request_dispatch,
            rw_dispatch,
            reset_dispatch,
        }
    }
}

#[derive(Debug)]
pub struct VirtioVsockDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
    host_request_dispatch: VirtioVsockHostRequestDispatch,
    guest_response_dispatch: VirtioVsockGuestResponseDispatch,
    guest_request_dispatch: VirtioVsockGuestRequestDispatch,
    guest_rw_dispatch: VirtioVsockGuestRwDispatch,
    guest_reset_dispatch: VirtioVsockGuestResetDispatch,
    rx_queue_dispatch: Option<VirtioVsockRxQueueDispatch>,
    tx_queue_dispatch: Option<VirtioVsockTxQueueDispatch>,
}

impl VirtioVsockDeviceNotificationDispatch {
    const fn new(
        drained_notifications: Vec<usize>,
        host_request_dispatch: VirtioVsockHostRequestDispatch,
        guest_tx_control_dispatch: VirtioVsockGuestTxControlDispatch,
        rx_queue_dispatch: Option<VirtioVsockRxQueueDispatch>,
        tx_queue_dispatch: Option<VirtioVsockTxQueueDispatch>,
    ) -> Self {
        Self {
            drained_notifications,
            host_request_dispatch,
            guest_response_dispatch: guest_tx_control_dispatch.response_dispatch,
            guest_request_dispatch: guest_tx_control_dispatch.request_dispatch,
            guest_rw_dispatch: guest_tx_control_dispatch.rw_dispatch,
            guest_reset_dispatch: guest_tx_control_dispatch.reset_dispatch,
            rx_queue_dispatch,
            tx_queue_dispatch,
        }
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub const fn host_request_dispatch(&self) -> &VirtioVsockHostRequestDispatch {
        &self.host_request_dispatch
    }

    pub const fn guest_response_dispatch(&self) -> &VirtioVsockGuestResponseDispatch {
        &self.guest_response_dispatch
    }

    pub const fn guest_request_dispatch(&self) -> &VirtioVsockGuestRequestDispatch {
        &self.guest_request_dispatch
    }

    pub const fn guest_rw_dispatch(&self) -> &VirtioVsockGuestRwDispatch {
        &self.guest_rw_dispatch
    }

    pub const fn guest_reset_dispatch(&self) -> &VirtioVsockGuestResetDispatch {
        &self.guest_reset_dispatch
    }

    pub const fn tx_queue_dispatch(&self) -> Option<&VirtioVsockTxQueueDispatch> {
        self.tx_queue_dispatch.as_ref()
    }

    pub const fn rx_queue_dispatch(&self) -> Option<&VirtioVsockRxQueueDispatch> {
        self.rx_queue_dispatch.as_ref()
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.tx_queue_dispatch
            .as_ref()
            .is_some_and(VirtioVsockTxQueueDispatch::needs_queue_interrupt)
            || self
                .rx_queue_dispatch
                .as_ref()
                .is_some_and(VirtioVsockRxQueueDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockGuestResponseDispatch {
    response_packets: usize,
    acknowledged_responses: usize,
    ignored_responses: usize,
    dropped_connections: usize,
}

impl VirtioVsockGuestResponseDispatch {
    const fn new() -> Self {
        Self {
            response_packets: 0,
            acknowledged_responses: 0,
            ignored_responses: 0,
            dropped_connections: 0,
        }
    }

    pub const fn response_packets(&self) -> usize {
        self.response_packets
    }

    pub const fn acknowledged_responses(&self) -> usize {
        self.acknowledged_responses
    }

    pub const fn ignored_responses(&self) -> usize {
        self.ignored_responses
    }

    pub const fn dropped_connections(&self) -> usize {
        self.dropped_connections
    }

    fn record(&mut self, outcome: VsockHostGuestResponseOutcome) {
        self.response_packets += 1;
        match outcome {
            VsockHostGuestResponseOutcome::Acknowledged { .. } => {
                self.acknowledged_responses += 1;
            }
            VsockHostGuestResponseOutcome::Ignored { .. } => {
                self.ignored_responses += 1;
            }
            VsockHostGuestResponseOutcome::Dropped { .. } => {
                self.dropped_connections += 1;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockGuestRequestDispatch {
    request_packets: usize,
    retained_requests: usize,
    ignored_requests: usize,
    dropped_requests: usize,
}

impl VirtioVsockGuestRequestDispatch {
    const fn new() -> Self {
        Self {
            request_packets: 0,
            retained_requests: 0,
            ignored_requests: 0,
            dropped_requests: 0,
        }
    }

    pub const fn request_packets(&self) -> usize {
        self.request_packets
    }

    pub const fn retained_requests(&self) -> usize {
        self.retained_requests
    }

    pub const fn ignored_requests(&self) -> usize {
        self.ignored_requests
    }

    pub const fn dropped_requests(&self) -> usize {
        self.dropped_requests
    }

    fn record(&mut self, outcome: VsockGuestConnectionRequestOutcome) {
        self.request_packets += 1;
        match outcome {
            VsockGuestConnectionRequestOutcome::Retained { key } => {
                let _ = key;
                self.retained_requests += 1;
            }
            VsockGuestConnectionRequestOutcome::Ignored { reason } => {
                let _ = reason;
                self.ignored_requests += 1;
            }
            VsockGuestConnectionRequestOutcome::Dropped { key, source } => {
                let _ = (key, source);
                self.dropped_requests += 1;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockGuestRwDispatch {
    rw_packets: usize,
    forwarded_packets: usize,
    forwarded_bytes: usize,
    ignored_packets: usize,
    dropped_connections: usize,
}

impl VirtioVsockGuestRwDispatch {
    const fn new() -> Self {
        Self {
            rw_packets: 0,
            forwarded_packets: 0,
            forwarded_bytes: 0,
            ignored_packets: 0,
            dropped_connections: 0,
        }
    }

    pub const fn rw_packets(&self) -> usize {
        self.rw_packets
    }

    pub const fn forwarded_packets(&self) -> usize {
        self.forwarded_packets
    }

    pub const fn forwarded_bytes(&self) -> usize {
        self.forwarded_bytes
    }

    pub const fn ignored_packets(&self) -> usize {
        self.ignored_packets
    }

    pub const fn dropped_connections(&self) -> usize {
        self.dropped_connections
    }

    fn record(&mut self, outcome: &VsockGuestRwOutcome) {
        self.rw_packets += 1;
        match outcome {
            VsockGuestRwOutcome::Forwarded { key, bytes } => {
                let _ = key;
                self.forwarded_packets += 1;
                self.forwarded_bytes += bytes;
            }
            VsockGuestRwOutcome::Ignored { reason } => {
                let _ = reason;
                self.ignored_packets += 1;
            }
            VsockGuestRwOutcome::Dropped { key, source } => {
                let _ = (key, source);
                self.dropped_connections += 1;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockGuestResetDispatch {
    reset_candidates: usize,
    queued_resets: usize,
    dropped_resets: usize,
}

impl VirtioVsockGuestResetDispatch {
    const fn new() -> Self {
        Self {
            reset_candidates: 0,
            queued_resets: 0,
            dropped_resets: 0,
        }
    }

    pub const fn reset_candidates(&self) -> usize {
        self.reset_candidates
    }

    pub const fn queued_resets(&self) -> usize {
        self.queued_resets
    }

    pub const fn dropped_resets(&self) -> usize {
        self.dropped_resets
    }

    fn record(&mut self, queued: bool) {
        self.reset_candidates += 1;
        if queued {
            self.queued_resets += 1;
        } else {
            self.dropped_resets += 1;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockHostRequestDispatch {
    accepted_connections: usize,
    completed_requests: usize,
    dropped_connections: usize,
    pending_connections: usize,
}

impl VirtioVsockHostRequestDispatch {
    const fn new() -> Self {
        Self {
            accepted_connections: 0,
            completed_requests: 0,
            dropped_connections: 0,
            pending_connections: 0,
        }
    }

    pub const fn accepted_connections(&self) -> usize {
        self.accepted_connections
    }

    pub const fn completed_requests(&self) -> usize {
        self.completed_requests
    }

    pub const fn dropped_connections(&self) -> usize {
        self.dropped_connections
    }

    pub const fn pending_connections(&self) -> usize {
        self.pending_connections
    }
}

#[derive(Debug)]
pub enum VirtioVsockDeviceNotificationError {
    Inactive {
        drained_notifications: Vec<usize>,
    },
    UnsupportedQueue {
        drained_notifications: Vec<usize>,
        queue_index: usize,
    },
    TxQueueDispatch {
        drained_notifications: Vec<usize>,
        completed_rx_dispatch: Option<Box<VirtioVsockRxQueueDispatch>>,
        source: VirtioVsockTxQueueDispatchError,
    },
    RxQueueDispatch {
        drained_notifications: Vec<usize>,
        completed_tx_dispatch: Option<Box<VirtioVsockTxQueueDispatch>>,
        source: VirtioVsockRxQueueDispatchError,
    },
}

impl VirtioVsockDeviceNotificationError {
    pub fn drained_notifications(&self) -> &[usize] {
        match self {
            Self::Inactive {
                drained_notifications,
            }
            | Self::UnsupportedQueue {
                drained_notifications,
                ..
            }
            | Self::TxQueueDispatch {
                drained_notifications,
                ..
            }
            | Self::RxQueueDispatch {
                drained_notifications,
                ..
            } => drained_notifications,
        }
    }

    pub const fn completed_tx_dispatch(&self) -> Option<&VirtioVsockTxQueueDispatch> {
        match self {
            Self::TxQueueDispatch { source, .. } => source.completed_dispatch(),
            Self::RxQueueDispatch {
                completed_tx_dispatch,
                ..
            } => match completed_tx_dispatch {
                Some(dispatch) => Some(dispatch),
                None => None,
            },
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }

    pub const fn completed_rx_dispatch(&self) -> Option<&VirtioVsockRxQueueDispatch> {
        match self {
            Self::RxQueueDispatch { source, .. } => source.completed_dispatch(),
            Self::TxQueueDispatch {
                completed_rx_dispatch,
                ..
            } => match completed_rx_dispatch {
                Some(dispatch) => Some(dispatch),
                None => None,
            },
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

impl fmt::Display for VirtioVsockDeviceNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive { .. } => f.write_str(
                "virtio-vsock queue notification cannot be dispatched before activation",
            ),
            Self::UnsupportedQueue { queue_index, .. } => {
                write!(
                    f,
                    "virtio-vsock queue notification for unsupported queue {queue_index}"
                )
            }
            Self::TxQueueDispatch { source, .. } => {
                write!(f, "failed to dispatch virtio-vsock TX queue: {source}")
            }
            Self::RxQueueDispatch { source, .. } => {
                write!(f, "failed to dispatch virtio-vsock RX queue: {source}")
            }
        }
    }
}

impl std::error::Error for VirtioVsockDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TxQueueDispatch { source, .. } => Some(source),
            Self::RxQueueDispatch { source, .. } => Some(source),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

impl<C: VirtioMmioDeviceConfigHandler> VirtioMmioRegisterHandler<C, VirtioVsockDevice> {
    pub fn dispatch_vsock_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioVsockDeviceNotificationDispatch, VirtioVsockDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        let dispatch = self
            .activation_handler_mut()
            .dispatch_drained_queue_notifications(memory, drained_notifications);
        let needs_queue_interrupt = match &dispatch {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) => {
                error
                    .completed_tx_dispatch()
                    .is_some_and(VirtioVsockTxQueueDispatch::needs_queue_interrupt)
                    || error
                        .completed_rx_dispatch()
                        .is_some_and(VirtioVsockRxQueueDispatch::needs_queue_interrupt)
            }
        };
        if needs_queue_interrupt {
            self.mark_interrupt_pending(DeviceInterruptKind::Queue);
        }

        dispatch
    }
}

fn active_vsock_queue_state(
    activation: VirtioMmioDeviceActivation<'_>,
    queue_index: u32,
) -> Result<VirtioMmioQueueState, VirtioVsockDeviceActivationError> {
    activation.queue(queue_index).copied().map_err(|source| {
        VirtioVsockDeviceActivationError::QueueMetadata {
            queue_index,
            source,
        }
    })
}

impl VirtioMmioDeviceActivationHandler for VirtioVsockDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        self.activate_vsock(activation).map_err(Into::into)
    }

    fn reset(&mut self) {
        VirtioVsockDevice::reset(self);
    }
}

#[derive(Debug)]
pub struct PreparedVsockDevice {
    guest_cid: u32,
    uds_path: PathBuf,
    config_space: VirtioVsockConfigSpace,
    device: VirtioVsockDevice,
}

impl PreparedVsockDevice {
    pub fn from_config(config: &VsockConfig) -> Self {
        Self::from_config_with_device(
            config,
            VirtioVsockDevice::with_guest_cid(config.guest_cid()),
        )
    }

    pub fn from_config_with_host_socket(
        config: &VsockConfig,
    ) -> Result<Self, PreparedVsockDeviceError> {
        let owner = VsockHostSocketOwner::bind(config.uds_path()).map_err(|source| {
            PreparedVsockDeviceError::HostSocket {
                guest_cid: config.guest_cid(),
                source,
            }
        })?;

        Ok(Self::from_config_with_device(
            config,
            VirtioVsockDevice::with_host_socket_owner(config.guest_cid(), owner),
        ))
    }

    fn from_config_with_device(config: &VsockConfig, mut device: VirtioVsockDevice) -> Self {
        device.set_host_socket_path(config.uds_path());
        Self {
            guest_cid: config.guest_cid(),
            uds_path: config.uds_path().to_path_buf(),
            config_space: VirtioVsockConfigSpace::new(u64::from(config.guest_cid())),
            device,
        }
    }

    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &Path {
        &self.uds_path
    }

    pub const fn config_space(&self) -> VirtioVsockConfigSpace {
        self.config_space
    }

    pub const fn device(&self) -> &VirtioVsockDevice {
        &self.device
    }

    pub fn into_parts(self) -> (u32, PathBuf, VirtioVsockConfigSpace, VirtioVsockDevice) {
        (
            self.guest_cid,
            self.uds_path,
            self.config_space,
            self.device,
        )
    }

    pub fn register_mmio(
        self,
        layout: VsockMmioLayout,
    ) -> Result<VsockMmioDevice, VsockMmioRegistrationError> {
        VsockMmioDevice::from_prepared(self, layout)
    }

    pub fn register_mmio_with_dispatcher(
        self,
        layout: VsockMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<VsockMmioDevice, VsockMmioRegistrationError> {
        VsockMmioDevice::from_prepared_with_dispatcher(self, layout, dispatcher)
    }
}

#[derive(Debug)]
pub enum PreparedVsockDeviceError {
    HostSocket {
        guest_cid: u32,
        source: VsockHostSocketOwnerError,
    },
}

impl fmt::Display for PreparedVsockDeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostSocket { guest_cid, source } => {
                write!(
                    f,
                    "failed to prepare vsock host socket for guest CID {guest_cid}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for PreparedVsockDeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HostSocket { source, .. } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsockMmioLayout {
    address: GuestAddress,
    region_id: MmioRegionId,
}

impl VsockMmioLayout {
    pub const fn new(address: GuestAddress, region_id: MmioRegionId) -> Self {
        Self { address, region_id }
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region_id
    }

    fn region(self) -> Result<MmioRegion, VsockMmioRegistrationError> {
        MmioRegion::new(self.region_id, self.address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE).map_err(
            |source| VsockMmioRegistrationError::InvalidRegion {
                region_id: self.region_id,
                address: self.address,
                source,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockMmioDeviceRegistration {
    guest_cid: u32,
    uds_path: PathBuf,
    region: MmioRegion,
}

impl VsockMmioDeviceRegistration {
    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &Path {
        &self.uds_path
    }

    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    pub const fn region_id(&self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn address(&self) -> GuestAddress {
        self.region.range().start()
    }
}

#[derive(Debug)]
pub struct VsockMmioDevice {
    dispatcher: MmioDispatcher,
    registration: VsockMmioDeviceRegistration,
}

impl VsockMmioDevice {
    pub fn from_prepared(
        prepared: PreparedVsockDevice,
        layout: VsockMmioLayout,
    ) -> Result<Self, VsockMmioRegistrationError> {
        Self::from_prepared_with_dispatcher(prepared, layout, MmioDispatcher::new())
    }

    pub fn from_prepared_with_dispatcher(
        prepared: PreparedVsockDevice,
        layout: VsockMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<Self, VsockMmioRegistrationError> {
        let region = layout.region()?;
        let (guest_cid, uds_path, config_space, device) = prepared.into_parts();
        let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_VSOCK_DEVICE_ID,
            config_space.available_features(),
            &VIRTIO_VSOCK_QUEUE_SIZES,
            config_space,
            device,
        )
        .map_err(|source| VsockMmioRegistrationError::BuildHandler {
            guest_cid,
            region_id: layout.region_id(),
            source,
        })?;
        let mut dispatcher = dispatcher;
        let inserted_region = dispatcher
            .insert_region(
                layout.region_id(),
                layout.address(),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .map_err(|source| VsockMmioRegistrationError::InsertRegion {
                guest_cid,
                region_id: layout.region_id(),
                address: layout.address(),
                source,
            })?;
        dispatcher
            .register_handler(layout.region_id(), handler)
            .map_err(|source| VsockMmioRegistrationError::RegisterHandler {
                guest_cid,
                region_id: layout.region_id(),
                source,
            })?;
        debug_assert_eq!(inserted_region, region);

        Ok(Self {
            dispatcher,
            registration: VsockMmioDeviceRegistration {
                guest_cid,
                uds_path,
                region,
            },
        })
    }

    pub fn dispatcher(&self) -> &MmioDispatcher {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut MmioDispatcher {
        &mut self.dispatcher
    }

    pub const fn registration(&self) -> &VsockMmioDeviceRegistration {
        &self.registration
    }

    pub fn into_parts(self) -> (MmioDispatcher, VsockMmioDeviceRegistration) {
        (self.dispatcher, self.registration)
    }
}

#[derive(Debug)]
pub enum VsockMmioRegistrationError {
    InvalidRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: GuestMemoryError,
    },
    BuildHandler {
        guest_cid: u32,
        region_id: MmioRegionId,
        source: VirtioMmioRegisterHandlerError,
    },
    InsertRegion {
        guest_cid: u32,
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        guest_cid: u32,
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for VsockMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "invalid vsock MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::BuildHandler {
                guest_cid,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to build vsock MMIO handler for guest CID {guest_cid} region id={region_id}: {source}"
                )
            }
            Self::InsertRegion {
                guest_cid,
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "failed to insert vsock MMIO region for guest CID {guest_cid} region id={region_id} at {address}: {source}"
                )
            }
            Self::RegisterHandler {
                guest_cid,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to register vsock MMIO handler for guest CID {guest_cid} region id={region_id}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VsockMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRegion { source, .. } => Some(source),
            Self::BuildHandler { source, .. } => Some(source),
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
        }
    }
}

pub fn virtio_vsock_mmio_handler(
    guest_cid: u32,
) -> Result<VirtioVsockMmioHandler, VirtioMmioRegisterHandlerError> {
    let config = VirtioVsockConfigSpace::new(u64::from(guest_cid));
    VirtioMmioRegisterHandler::with_device_config_and_activation(
        VIRTIO_VSOCK_DEVICE_ID,
        config.available_features(),
        &VIRTIO_VSOCK_QUEUE_SIZES,
        config,
        VirtioVsockDevice::with_guest_cid(guest_cid),
    )
}

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

fn config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: MmioHandlerError::new(format!("virtio-vsock config access bytes failed: {source}")),
    }
}

fn has_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::fs;
    use std::io;
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::interrupt::DeviceInterruptKind;
    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};
    use crate::mmio::{
        MmioAccess, MmioAccessBytes, MmioBus, MmioDispatchOutcome, MmioDispatcher, MmioOperation,
        MmioRegionId,
    };
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation,
        VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
        VirtioMmioDeviceRegisters, VirtioMmioQueueRegisters, VirtioMmioRegister,
        VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
        VirtqueueDescriptorChain, read_descriptor_chain,
    };

    use super::{
        MIN_GUEST_CID, PreparedVsockDevice, VIRTIO_FEATURE_IN_ORDER, VIRTIO_FEATURE_VERSION_1,
        VIRTIO_RING_FEATURE_EVENT_IDX, VIRTIO_VSOCK_CONFIG_GUEST_CID_SIZE,
        VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE, VIRTIO_VSOCK_DEVICE_ID,
        VIRTIO_VSOCK_EVENT_QUEUE_INDEX, VIRTIO_VSOCK_FLAGS_SHUTDOWN_RCV,
        VIRTIO_VSOCK_FLAGS_SHUTDOWN_SEND, VIRTIO_VSOCK_HOST_CID,
        VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE, VIRTIO_VSOCK_OP_CREDIT_REQUEST,
        VIRTIO_VSOCK_OP_CREDIT_UPDATE, VIRTIO_VSOCK_OP_REQUEST, VIRTIO_VSOCK_OP_RESPONSE,
        VIRTIO_VSOCK_OP_RST, VIRTIO_VSOCK_OP_RW, VIRTIO_VSOCK_OP_SHUTDOWN,
        VIRTIO_VSOCK_PACKET_HEADER_SIZE, VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64,
        VIRTIO_VSOCK_PACKET_TYPE_STREAM, VIRTIO_VSOCK_QUEUE_COUNT, VIRTIO_VSOCK_QUEUE_SIZE,
        VIRTIO_VSOCK_QUEUE_SIZES, VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX,
        VSOCK_HOST_CONNECT_REQUEST_MAX_LEN, VSOCK_HOST_LOCAL_PORT_BASE,
        VSOCK_HOST_LOCAL_PORT_CAPACITY, VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE,
        VirtioVsockConfigSpace, VirtioVsockDevice, VirtioVsockDeviceActivationError,
        VirtioVsockGuestRequestDispatch, VirtioVsockGuestResetDispatch,
        VirtioVsockGuestResponseDispatch, VirtioVsockGuestRwDispatch,
        VirtioVsockHostRequestDispatch, VirtioVsockMmioHandler, VirtioVsockPacketHeader,
        VirtioVsockPacketLengthError, VirtioVsockQueueBuildError, VirtioVsockRxBufferParseError,
        VirtioVsockRxPacketKind, VirtioVsockRxQueue, VirtioVsockRxQueueDispatchError,
        VirtioVsockTxPacket, VirtioVsockTxPacketParseError, VirtioVsockTxQueue,
        VirtioVsockTxQueueDispatchError, VsockConfigError, VsockConfigInput,
        VsockGuestConnectionKey, VsockHostConnectHandshakeError, VsockHostConnectRequest,
        VsockHostConnectRequestError, VsockHostConnectionKey, VsockHostConnectionTable,
        VsockHostConnectionTableError, VsockHostLocalPort, VsockHostLocalPortAllocator,
        VsockHostLocalPortAllocatorError, VsockHostLocalPortError, VsockHostSocketAcceptError,
        VsockHostSocketOwner, VsockHostSocketOwnerError, VsockMmioDevice, VsockMmioLayout,
        VsockMmioRegistrationError, is_transient_host_socket_accept_error,
        is_transient_host_socket_read_error, parse_vsock_host_connect_request,
        virtio_vsock_mmio_handler,
    };

    static NEXT_TEST_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

    const TEST_MMIO_BASE: u64 = 0x1000_0000;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    const TEST_QUEUE_ADDRESS_BASE: u64 = 0x1000;
    const TEST_QUEUE_ADDRESS_STRIDE: u64 = 0x1000;
    const TEST_VSOCK_TX_MEMORY_SIZE: u64 = 0x20_000;
    const TEST_VSOCK_TX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x2000);
    const TEST_VSOCK_TX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x2200);
    const TEST_VSOCK_TX_USED_RING: GuestAddress = GuestAddress::new(0x2400);
    const TEST_VSOCK_TX_UNMAPPED_USED_RING: GuestAddress = GuestAddress::new(0x30_000);
    const TEST_VSOCK_RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x1000);
    const TEST_VSOCK_RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x1200);
    const TEST_VSOCK_RX_USED_RING: GuestAddress = GuestAddress::new(0x1400);
    const TEST_VSOCK_RX_UNMAPPED_USED_RING: GuestAddress = GuestAddress::new(0x30_000);
    const TEST_VSOCK_RX_BUFFER: GuestAddress = GuestAddress::new(0x9000);
    const TEST_VSOCK_RX_SECOND_BUFFER: GuestAddress = GuestAddress::new(0xa000);
    const TEST_VSOCK_RX_UNMAPPED_BUFFER: GuestAddress = GuestAddress::new(0x30_000);
    const TEST_VSOCK_HEADER: GuestAddress = GuestAddress::new(0x4000);
    const TEST_VSOCK_SECOND_HEADER: GuestAddress = GuestAddress::new(0x5000);
    const TEST_VSOCK_PAYLOAD: GuestAddress = GuestAddress::new(0x6000);
    const TEST_VSOCK_SECOND_PAYLOAD: GuestAddress = GuestAddress::new(0x7000);
    const TEST_VSOCK_QUEUE_SIZE: u16 = 8;

    fn validate(input: VsockConfigInput) -> Result<super::VsockConfig, VsockConfigError> {
        input.validate()
    }

    fn valid_vsock_config(guest_cid: u32, uds_path: impl Into<String>) -> super::VsockConfig {
        validate(VsockConfigInput::new(guest_cid, uds_path)).expect("valid config")
    }

    fn prepared_vsock_device(guest_cid: u32, uds_path: impl Into<String>) -> PreparedVsockDevice {
        PreparedVsockDevice::from_config(&valid_vsock_config(guest_cid, uds_path))
    }

    fn unique_socket_path(name: &str) -> PathBuf {
        let id = NEXT_TEST_SOCKET_ID.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after Unix epoch")
            .as_nanos();
        let short_name: String = name.chars().take(12).collect();
        PathBuf::from("/tmp").join(format!(
            "bb-vsock-{short_name}-{now:x}-{}-{id:x}.sock",
            std::process::id()
        ))
    }

    fn accepted_host_connection(name: &str) -> (super::VsockHostAcceptedConnection, UnixStream) {
        let path = unique_socket_path(name);
        let owner = VsockHostSocketOwner::bind(&path).expect("host socket should bind");
        let client = UnixStream::connect(&path).expect("client should connect");
        let accepted = owner
            .accept_host_connection()
            .expect("pending connection should accept")
            .expect("accepted connection should be present");

        drop(owner);
        assert!(!path.exists());

        (accepted, client)
    }

    fn host_socket_device(guest_cid: u32, name: &str) -> (VirtioVsockDevice, PathBuf) {
        let path = unique_socket_path(name);
        let owner = VsockHostSocketOwner::bind(&path).expect("host socket should bind");
        (
            VirtioVsockDevice::with_host_socket_owner(guest_cid, owner),
            path,
        )
    }

    fn virtio_vsock_mmio_handler_with_host_socket(
        guest_cid: u32,
        path: &Path,
    ) -> VirtioVsockMmioHandler {
        let config = VirtioVsockConfigSpace::new(u64::from(guest_cid));
        let owner = VsockHostSocketOwner::bind(path).expect("host socket should bind");
        VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_VSOCK_DEVICE_ID,
            config.available_features(),
            &VIRTIO_VSOCK_QUEUE_SIZES,
            config,
            VirtioVsockDevice::with_host_socket_owner(guest_cid, owner),
        )
        .expect("vsock handler with host socket should build")
    }

    fn assert_empty_host_request_dispatch(dispatch: &VirtioVsockHostRequestDispatch) {
        assert_eq!(dispatch.accepted_connections(), 0);
        assert_eq!(dispatch.completed_requests(), 0);
        assert_eq!(dispatch.dropped_connections(), 0);
        assert_eq!(dispatch.pending_connections(), 0);
    }

    fn assert_empty_guest_response_dispatch(dispatch: &VirtioVsockGuestResponseDispatch) {
        assert_eq!(dispatch.response_packets(), 0);
        assert_eq!(dispatch.acknowledged_responses(), 0);
        assert_eq!(dispatch.ignored_responses(), 0);
        assert_eq!(dispatch.dropped_connections(), 0);
    }

    fn assert_empty_guest_request_dispatch(dispatch: &VirtioVsockGuestRequestDispatch) {
        assert_eq!(dispatch.request_packets(), 0);
        assert_eq!(dispatch.retained_requests(), 0);
        assert_eq!(dispatch.ignored_requests(), 0);
        assert_eq!(dispatch.dropped_requests(), 0);
    }

    fn assert_empty_guest_rw_dispatch(dispatch: &VirtioVsockGuestRwDispatch) {
        assert_eq!(dispatch.rw_packets(), 0);
        assert_eq!(dispatch.forwarded_packets(), 0);
        assert_eq!(dispatch.forwarded_bytes(), 0);
        assert_eq!(dispatch.ignored_packets(), 0);
        assert_eq!(dispatch.dropped_connections(), 0);
    }

    fn assert_empty_guest_reset_dispatch(dispatch: &VirtioVsockGuestResetDispatch) {
        assert_eq!(dispatch.reset_candidates(), 0);
        assert_eq!(dispatch.queued_resets(), 0);
        assert_eq!(dispatch.dropped_resets(), 0);
    }

    fn host_connect_request(peer_port: u32) -> VsockHostConnectRequest {
        let request = format!("CONNECT {peer_port}\n");
        VsockHostConnectRequest::parse(request.as_bytes())
            .expect("test host CONNECT request should parse")
    }

    fn accepted_host_connection_with_request(
        name: &str,
        peer_port: u32,
    ) -> (
        super::VsockHostAcceptedConnection,
        UnixStream,
        VsockHostConnectRequest,
    ) {
        let (accepted, client) = accepted_host_connection(name);
        (accepted, client, host_connect_request(peer_port))
    }

    fn insert_accepted_host_connection_for_test(
        table: &mut VsockHostConnectionTable,
        name: &str,
        peer_port: u32,
    ) -> (VsockHostConnectionKey, UnixStream) {
        let (accepted, client, request) = accepted_host_connection_with_request(name, peer_port);
        let key = table
            .insert_accepted_host_connection(accepted, request)
            .expect("accepted host connection should insert");

        (key, client)
    }

    fn assert_stream_closed(stream: &mut UnixStream, context: &str) {
        stream
            .set_nonblocking(true)
            .expect("test stream should switch to nonblocking mode");
        let mut closed = [0; 1];

        assert_eq!(stream.read(&mut closed).expect(context), 0, "{context}");
    }

    fn assert_host_connection_request_header(
        header: VirtioVsockPacketHeader,
        guest_cid: u32,
        local_port: VsockHostLocalPort,
        peer_port: u32,
    ) {
        assert_eq!(header.src_cid(), VIRTIO_VSOCK_HOST_CID);
        assert_eq!(header.dst_cid(), u64::from(guest_cid));
        assert_eq!(header.src_port(), local_port.raw());
        assert_eq!(header.dst_port(), peer_port);
        assert_eq!(header.payload_len(), 0);
        assert_eq!(header.packet_type(), VIRTIO_VSOCK_PACKET_TYPE_STREAM);
        assert_eq!(header.operation(), VIRTIO_VSOCK_OP_REQUEST);
        assert_eq!(header.flags(), 0);
        assert_eq!(
            header.buffer_allocation(),
            VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE
        );
        assert_eq!(header.forwarded_count(), 0);
    }

    fn assert_guest_reset_packet_header(
        header: VirtioVsockPacketHeader,
        guest_cid: u32,
        host_port: u32,
        guest_port: u32,
    ) {
        assert_eq!(header.src_cid(), VIRTIO_VSOCK_HOST_CID);
        assert_eq!(header.dst_cid(), u64::from(guest_cid));
        assert_eq!(header.src_port(), host_port);
        assert_eq!(header.dst_port(), guest_port);
        assert_eq!(header.payload_len(), 0);
        assert_eq!(header.packet_type(), VIRTIO_VSOCK_PACKET_TYPE_STREAM);
        assert_eq!(header.operation(), VIRTIO_VSOCK_OP_RST);
        assert_eq!(header.flags(), 0);
        assert_eq!(
            header.buffer_allocation(),
            VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE
        );
        assert_eq!(header.forwarded_count(), 0);
    }

    fn assert_guest_response_packet_header(
        header: VirtioVsockPacketHeader,
        guest_cid: u32,
        host_port: u32,
        guest_port: u32,
    ) {
        assert_eq!(header.src_cid(), VIRTIO_VSOCK_HOST_CID);
        assert_eq!(header.dst_cid(), u64::from(guest_cid));
        assert_eq!(header.src_port(), host_port);
        assert_eq!(header.dst_port(), guest_port);
        assert_eq!(header.payload_len(), 0);
        assert_eq!(header.packet_type(), VIRTIO_VSOCK_PACKET_TYPE_STREAM);
        assert_eq!(header.operation(), VIRTIO_VSOCK_OP_RESPONSE);
        assert_eq!(header.flags(), 0);
        assert_eq!(
            header.buffer_allocation(),
            VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE
        );
        assert_eq!(header.forwarded_count(), 0);
    }

    fn assert_host_rw_packet_header(
        header: VirtioVsockPacketHeader,
        guest_cid: u32,
        host_port: u32,
        guest_port: u32,
        payload_len: usize,
    ) {
        assert_eq!(header.src_cid(), VIRTIO_VSOCK_HOST_CID);
        assert_eq!(header.dst_cid(), u64::from(guest_cid));
        assert_eq!(header.src_port(), host_port);
        assert_eq!(header.dst_port(), guest_port);
        assert_eq!(
            header.payload_len(),
            u32::try_from(payload_len).expect("test payload length should fit")
        );
        assert_eq!(header.packet_type(), VIRTIO_VSOCK_PACKET_TYPE_STREAM);
        assert_eq!(header.operation(), VIRTIO_VSOCK_OP_RW);
        assert_eq!(header.flags(), 0);
        assert_eq!(
            header.buffer_allocation(),
            VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE
        );
        assert_eq!(header.forwarded_count(), 0);
    }

    struct TestUnixListener {
        listener: UnixListener,
        path: PathBuf,
    }

    impl TestUnixListener {
        fn bind(path: PathBuf) -> Self {
            let listener = UnixListener::bind(&path).expect("test listener should bind");
            listener
                .set_nonblocking(true)
                .expect("test listener should switch to nonblocking mode");
            Self { listener, path }
        }

        fn accept(&self) -> UnixStream {
            self.listener
                .accept()
                .expect("test listener should have a pending connection")
                .0
        }
    }

    impl Drop for TestUnixListener {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn guest_connection_listener(base: &Path, port: u32) -> TestUnixListener {
        TestUnixListener::bind(super::guest_connection_socket_path(base, port))
    }

    #[test]
    fn nonblocking_unix_stream_connect_rejects_too_long_path() {
        let max_path_len = super::unix_socket_address(Path::new("x"))
            .expect("single-byte path should fit")
            .address
            .sun_path
            .len();
        let path = PathBuf::from("x".repeat(max_path_len));

        let err = super::nonblocking_unix_stream_connect(&path)
            .expect_err("path at sun_path capacity should be rejected");

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    fn guest_request_tx_packet(
        guest_cid: u32,
        host_port: u32,
        guest_port: u32,
    ) -> VirtioVsockTxPacket {
        guest_tx_packet_with_header(
            VirtioVsockPacketHeader::new()
                .with_src_cid(u64::from(guest_cid))
                .with_dst_cid(VIRTIO_VSOCK_HOST_CID)
                .with_src_port(guest_port)
                .with_dst_port(host_port)
                .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
                .with_operation(VIRTIO_VSOCK_OP_REQUEST),
        )
    }

    fn guest_rw_tx_packet(guest_cid: u32, host_port: u32, guest_port: u32) -> VirtioVsockTxPacket {
        guest_tx_packet_with_header(
            VirtioVsockPacketHeader::new()
                .with_src_cid(u64::from(guest_cid))
                .with_dst_cid(VIRTIO_VSOCK_HOST_CID)
                .with_src_port(guest_port)
                .with_dst_port(host_port)
                .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
                .with_operation(VIRTIO_VSOCK_OP_RW),
        )
    }

    fn guest_response_tx_packet(
        guest_cid: u32,
        local_port: VsockHostLocalPort,
        peer_port: u32,
    ) -> VirtioVsockTxPacket {
        guest_response_tx_packet_with_header(
            VirtioVsockPacketHeader::new()
                .with_src_cid(u64::from(guest_cid))
                .with_dst_cid(VIRTIO_VSOCK_HOST_CID)
                .with_src_port(peer_port)
                .with_dst_port(local_port.raw())
                .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
                .with_operation(VIRTIO_VSOCK_OP_RESPONSE),
        )
    }

    fn guest_response_tx_packet_with_header(
        header: VirtioVsockPacketHeader,
    ) -> VirtioVsockTxPacket {
        guest_tx_packet_with_header(header)
    }

    fn guest_tx_packet_with_header(header: VirtioVsockPacketHeader) -> VirtioVsockTxPacket {
        VirtioVsockTxPacket {
            descriptor_head: 0,
            header,
            payload_segments: Vec::new(),
        }
    }

    fn established_guest_connection_for_test(
        name: &str,
        host_port: u32,
        guest_port: u32,
    ) -> (GuestMemory, VirtioVsockMmioHandler, UnixStream) {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path(name);
        let listener = guest_connection_listener(&path, host_port);
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);
        let request = guest_request_tx_packet(42, host_port, guest_port).header();

        activate_vsock_handler(&mut handler);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("guest request should connect and deliver response");

        assert_eq!(notification.guest_request_dispatch().retained_requests(), 1);
        assert_empty_guest_rw_dispatch(notification.guest_rw_dispatch());
        assert_empty_guest_reset_dispatch(notification.guest_reset_dispatch());
        let rx = notification
            .rx_queue_dispatch()
            .expect("guest response should dispatch");
        assert_eq!(rx.delivered_responses(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);

        (memory, handler, listener.accept())
    }

    fn established_host_connection_for_test(
        name: &str,
        peer_port: u32,
    ) -> (
        GuestMemory,
        VirtioVsockMmioHandler,
        UnixStream,
        VsockHostConnectionKey,
    ) {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");
        let (accepted, mut client, request) =
            accepted_host_connection_with_request(name, peer_port);

        activate_vsock_handler(&mut handler);
        let key = handler
            .activation_handler_mut()
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);
        let rx_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("host request should deliver to guest");
        assert_eq!(
            rx_notification
                .rx_queue_dispatch()
                .expect("RX dispatch summary should be present")
                .delivered_requests(),
            1
        );

        let response_header = guest_response_tx_packet(42, key.local_port(), peer_port).header();
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, response_header);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);
        let tx_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("guest response should acknowledge host connection");
        assert_eq!(
            tx_notification
                .guest_response_dispatch()
                .acknowledged_responses(),
            1
        );
        assert_host_ok_message(&mut client, key.local_port());

        (memory, handler, client, key)
    }

    fn assert_host_ok_message(stream: &mut UnixStream, local_port: VsockHostLocalPort) {
        let expected = format!("OK {}\n", local_port.raw());
        let mut buffer = vec![0; expected.len()];
        stream
            .read_exact(&mut buffer)
            .expect("host stream should receive OK message");

        assert_eq!(buffer, expected.as_bytes());
    }

    fn assert_no_host_message(stream: &mut UnixStream) {
        stream
            .set_nonblocking(true)
            .expect("test stream should switch to nonblocking mode");
        let mut buffer = [0; 1];
        let err = stream
            .read(&mut buffer)
            .expect_err("host stream should not have readable bytes");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    }

    fn assert_host_payload(stream: &mut UnixStream, expected: &[u8]) {
        stream
            .set_nonblocking(true)
            .expect("test stream should switch to nonblocking mode");
        let mut buffer = vec![0; expected.len()];
        let mut offset = 0;

        while offset < buffer.len() {
            match stream.read(&mut buffer[offset..]) {
                Ok(0) => panic!("host stream closed before expected payload"),
                Ok(read) => {
                    offset += read;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    panic!("host stream did not have expected payload")
                }
                Err(error) => panic!("host stream payload read failed: {error}"),
            }
        }

        assert_eq!(buffer, expected);
    }

    fn assert_guest_response_ignored(
        outcome: Option<super::VsockHostGuestResponseOutcome>,
        reason: super::VsockHostGuestResponseIgnoreReason,
    ) {
        assert_eq!(
            outcome,
            Some(super::VsockHostGuestResponseOutcome::Ignored { reason })
        );
    }

    fn unique_missing_socket_path() -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after Unix epoch")
            .as_nanos();
        format!(
            "bangbang-vsock-missing-parent-{}-{unique}/v.sock",
            std::process::id(),
        )
    }

    fn vsock_mmio_layout() -> VsockMmioLayout {
        VsockMmioLayout::new(GuestAddress::new(TEST_MMIO_BASE), MmioRegionId::new(2))
    }

    fn read_registered_config(device: &mut VsockMmioDevice, offset: u64, len: u64) -> Vec<u8> {
        let address = device
            .registration()
            .address()
            .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset)
            .expect("test config address should not overflow");
        let access = device
            .dispatcher()
            .lookup(address, len)
            .expect("registered config access should resolve");
        let operation = MmioOperation::read(access).expect("registered config read should build");
        let outcome = device
            .dispatcher_mut()
            .dispatch(operation)
            .expect("registered config read should dispatch");
        let MmioDispatchOutcome::Read { data } = outcome else {
            panic!("read operation should return read outcome");
        };

        data.as_slice().to_vec()
    }

    fn virtio_mmio_access(offset: u64, len: u64) -> MmioAccess {
        let mut bus = MmioBus::new();
        bus.insert(
            MmioRegionId::new(1),
            GuestAddress::new(TEST_MMIO_BASE),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("virtio-mmio region should insert");
        bus.lookup(GuestAddress::new(TEST_MMIO_BASE + offset), len)
            .expect("virtio-mmio access should resolve")
    }

    fn device_config_access(offset: u64, len: u64) -> MmioAccess {
        virtio_mmio_access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset, len)
    }

    fn read_config(handler: &VirtioVsockMmioHandler, offset: u64, len: u64) -> Vec<u8> {
        handler
            .read_access(device_config_access(offset, len))
            .expect("vsock config read should succeed")
            .as_slice()
            .to_vec()
    }

    fn read_interrupt_status(handler: &VirtioVsockMmioHandler) -> u32 {
        handler
            .read_register(VirtioMmioRegister::InterruptStatus)
            .expect("interrupt status should read")
    }

    fn advance_handler_to_features_ok(handler: &mut VirtioVsockMmioHandler) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("status should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("status should accept DRIVER");
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("status should accept FEATURES_OK");
    }

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test guest address should fit in low half")
    }

    fn queue_base(queue_index: u32) -> u64 {
        TEST_QUEUE_ADDRESS_BASE + u64::from(queue_index) * TEST_QUEUE_ADDRESS_STRIDE
    }

    fn configure_vsock_queues(handler: &mut VirtioVsockMmioHandler) {
        configure_vsock_queues_with_size(handler, VIRTIO_VSOCK_QUEUE_SIZE);
    }

    fn configure_vsock_queues_with_size(handler: &mut VirtioVsockMmioHandler, queue_size: u16) {
        for queue_index in 0..VIRTIO_VSOCK_QUEUE_COUNT {
            let queue_index_u32 = u32::try_from(queue_index).expect("queue index should fit");
            let base = queue_base(queue_index_u32);
            handler
                .write_register(VirtioMmioRegister::QueueSel, queue_index_u32)
                .expect("queue select should write");
            handler
                .write_register(VirtioMmioRegister::QueueNum, u32::from(queue_size))
                .expect("queue size should write");
            handler
                .write_register(
                    VirtioMmioRegister::QueueDescLow,
                    guest_address_low(GuestAddress::new(base)),
                )
                .expect("queue descriptor table should write");
            handler
                .write_register(
                    VirtioMmioRegister::QueueDriverLow,
                    guest_address_low(GuestAddress::new(base + 0x200)),
                )
                .expect("queue driver ring should write");
            handler
                .write_register(
                    VirtioMmioRegister::QueueDeviceLow,
                    guest_address_low(GuestAddress::new(base + 0x400)),
                )
                .expect("queue device ring should write");
            handler
                .write_register(VirtioMmioRegister::QueueReady, 1)
                .expect("queue ready should write");
        }
    }

    fn activate_vsock_handler(handler: &mut VirtioVsockMmioHandler) {
        advance_handler_to_features_ok(handler);
        configure_vsock_queues(handler);
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("DRIVER_OK should activate vsock device");
    }

    fn notify_vsock_queue(handler: &mut VirtioVsockMmioHandler, queue_index: usize) {
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                u32::try_from(queue_index).expect("queue index should fit"),
            )
            .expect("queue notification should write");
    }

    fn configured_vsock_queue_registers(
        queue_size: Option<u16>,
        ready: bool,
    ) -> VirtioMmioQueueRegisters {
        configured_vsock_queue_registers_from_specs([(queue_size, ready); VIRTIO_VSOCK_QUEUE_COUNT])
    }

    fn configured_vsock_queue_registers_with_rx_device_ring(
        device_ring: GuestAddress,
    ) -> VirtioMmioQueueRegisters {
        let mut queues = configured_vsock_queue_registers(Some(VIRTIO_VSOCK_QUEUE_SIZE), true);
        queues
            .write_register(
                VirtioMmioRegister::QueueSel,
                u32::try_from(VIRTIO_VSOCK_RX_QUEUE_INDEX).expect("RX queue index should fit"),
                QUEUE_CONFIG_STATUS,
            )
            .expect("RX queue select should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                guest_address_low(device_ring),
                QUEUE_CONFIG_STATUS,
            )
            .expect("RX device ring should write");
        queues
    }

    fn configured_vsock_queue_registers_from_specs(
        specs: [(Option<u16>, bool); VIRTIO_VSOCK_QUEUE_COUNT],
    ) -> VirtioMmioQueueRegisters {
        let mut queues = VirtioMmioQueueRegisters::new(&VIRTIO_VSOCK_QUEUE_SIZES)
            .expect("queue table should build");
        for (queue_index, (queue_size, ready)) in specs.into_iter().enumerate() {
            let queue_index_u32 = u32::try_from(queue_index).expect("queue index should fit");
            let base = queue_base(queue_index_u32);
            queues
                .write_register(
                    VirtioMmioRegister::QueueSel,
                    queue_index_u32,
                    QUEUE_CONFIG_STATUS,
                )
                .expect("queue select should write");
            if let Some(queue_size) = queue_size {
                queues
                    .write_register(
                        VirtioMmioRegister::QueueNum,
                        u32::from(queue_size),
                        QUEUE_CONFIG_STATUS,
                    )
                    .expect("queue size should write");
            }
            queues
                .write_register(
                    VirtioMmioRegister::QueueDescLow,
                    guest_address_low(GuestAddress::new(base)),
                    QUEUE_CONFIG_STATUS,
                )
                .expect("queue descriptor table should write");
            queues
                .write_register(
                    VirtioMmioRegister::QueueDriverLow,
                    guest_address_low(GuestAddress::new(base + 0x200)),
                    QUEUE_CONFIG_STATUS,
                )
                .expect("queue driver ring should write");
            queues
                .write_register(
                    VirtioMmioRegister::QueueDeviceLow,
                    guest_address_low(GuestAddress::new(base + 0x400)),
                    QUEUE_CONFIG_STATUS,
                )
                .expect("queue device ring should write");
            if ready {
                queues
                    .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
                    .expect("queue ready should write");
            }
        }

        queues
    }

    fn vsock_queue_registers_with_count(queue_count: usize) -> VirtioMmioQueueRegisters {
        let queue_sizes = vec![VIRTIO_VSOCK_QUEUE_SIZE; queue_count];
        VirtioMmioQueueRegisters::new(&queue_sizes).expect("queue table should build")
    }

    fn vsock_device_registers() -> VirtioMmioDeviceRegisters {
        VirtioMmioDeviceRegisters::new(VIRTIO_VSOCK_DEVICE_ID, 0)
    }

    fn vsock_handler_for_config(config: VirtioVsockConfigSpace) -> VirtioVsockMmioHandler {
        VirtioVsockMmioHandler::with_device_config_and_activation(
            VIRTIO_VSOCK_DEVICE_ID,
            config.available_features(),
            &VIRTIO_VSOCK_QUEUE_SIZES,
            config,
            VirtioVsockDevice::new(),
        )
        .expect("vsock handler should build")
    }

    fn test_vsock_packet_header() -> VirtioVsockPacketHeader {
        VirtioVsockPacketHeader::new()
            .with_src_cid(0x0102_0304_0506_0708)
            .with_dst_cid(0x1112_1314_1516_1718)
            .with_src_port(0x2122_2324)
            .with_dst_port(0x3132_3334)
            .with_payload_len(0x1000)
            .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
            .with_operation(VIRTIO_VSOCK_OP_RW)
            .with_flags(VIRTIO_VSOCK_FLAGS_SHUTDOWN_RCV | VIRTIO_VSOCK_FLAGS_SHUTDOWN_SEND)
            .with_buffer_allocation(0x4142_4344)
            .with_forwarded_count(0x5152_5354)
    }

    #[derive(Clone, Copy)]
    struct TestDescriptor {
        address: GuestAddress,
        len: u32,
        flags: u16,
        next: u16,
    }

    impl TestDescriptor {
        const fn readable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            Self {
                address,
                len,
                flags: descriptor_flags(next),
                next: descriptor_next(next),
            }
        }

        const fn writable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            Self {
                address,
                len,
                flags: descriptor_flags(next) | VIRTQUEUE_DESC_F_WRITE,
                next: descriptor_next(next),
            }
        }
    }

    const fn descriptor_flags(next: Option<u16>) -> u16 {
        if next.is_some() {
            VIRTQUEUE_DESC_F_NEXT
        } else {
            0
        }
    }

    const fn descriptor_next(next: Option<u16>) -> u16 {
        match next {
            Some(next) => next,
            None => 0,
        }
    }

    fn vsock_tx_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_VSOCK_TX_MEMORY_SIZE)
                .expect("test range should be valid"),
        ])
        .expect("test memory layout should be valid");
        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    fn write_vsock_tx_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_VSOCK_TX_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("vsock TX descriptor should write");
    }

    fn write_vsock_tx_available_index(memory: &mut GuestMemory, index: u16) {
        let index_address = TEST_VSOCK_TX_AVAILABLE_RING
            .checked_add(2)
            .expect("available index address should not overflow");
        memory
            .write_slice(&index.to_le_bytes(), index_address)
            .expect("vsock TX available index should write");
    }

    fn write_vsock_tx_available_entry(memory: &mut GuestMemory, slot: usize, head: u16) {
        let slot_offset = u64::try_from(slot).expect("available slot should fit") * 2;
        let entry_address = TEST_VSOCK_TX_AVAILABLE_RING
            .checked_add(4 + slot_offset)
            .expect("available entry address should not overflow");
        memory
            .write_slice(&head.to_le_bytes(), entry_address)
            .expect("vsock TX available entry should write");
    }

    fn write_vsock_tx_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (slot, head) in heads.iter().copied().enumerate() {
            write_vsock_tx_available_entry(memory, slot, head);
        }
        write_vsock_tx_available_index(
            memory,
            u16::try_from(heads.len()).expect("available head count should fit"),
        );
    }

    fn append_vsock_tx_available_head(
        memory: &mut GuestMemory,
        slot: usize,
        head: u16,
        next_available: u16,
    ) {
        write_vsock_tx_available_entry(memory, slot, head);
        write_vsock_tx_available_index(memory, next_available);
    }

    fn vsock_tx_used_ring_idx_address() -> GuestAddress {
        TEST_VSOCK_TX_USED_RING
            .checked_add(2)
            .expect("vsock TX used ring idx address should not overflow")
    }

    fn vsock_tx_used_ring_entry_address(index: usize) -> GuestAddress {
        TEST_VSOCK_TX_USED_RING
            .checked_add(4 + u64::try_from(index).expect("used ring index should fit") * 8)
            .expect("vsock TX used ring entry address should not overflow")
    }

    fn read_vsock_tx_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, vsock_tx_used_ring_idx_address())
    }

    fn read_vsock_tx_used_element(memory: &GuestMemory, index: usize) -> (u32, u32) {
        let address = vsock_tx_used_ring_entry_address(index);
        let descriptor_head = read_guest_u32(memory, address);
        let len = read_guest_u32(
            memory,
            address
                .checked_add(4)
                .expect("vsock TX used ring len address should not overflow"),
        );
        (descriptor_head, len)
    }

    fn vsock_tx_descriptor_chain(
        memory: &mut GuestMemory,
        descriptors: &[TestDescriptor],
    ) -> VirtqueueDescriptorChain {
        for (index, descriptor) in descriptors.iter().copied().enumerate() {
            write_vsock_tx_descriptor(
                memory,
                u16::try_from(index).expect("test descriptor index should fit"),
                descriptor,
            );
        }

        read_descriptor_chain(
            memory,
            TEST_VSOCK_TX_DESCRIPTOR_TABLE,
            TEST_VSOCK_QUEUE_SIZE,
            0,
        )
        .expect("vsock TX descriptor chain should read")
    }

    fn write_vsock_packet_header(
        memory: &mut GuestMemory,
        address: GuestAddress,
        header: VirtioVsockPacketHeader,
    ) {
        memory
            .write_slice(&header.to_bytes(), address)
            .expect("vsock packet header should write");
    }

    fn write_vsock_tx_packet_with_payload(
        memory: &mut GuestMemory,
        descriptor_index: u16,
        address: GuestAddress,
        header: VirtioVsockPacketHeader,
        payload: &[u8],
    ) {
        let payload_len = u32::try_from(payload.len()).expect("test payload length should fit");
        write_vsock_packet_header(memory, address, header.with_payload_len(payload_len));
        write_guest_bytes(memory, vsock_payload_address_after_header(address), payload);
        write_vsock_tx_descriptor(
            memory,
            descriptor_index,
            TestDescriptor::readable(
                address,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + payload_len,
                None,
            ),
        );
    }

    fn vsock_packet_len_with_payload(payload: &[u8]) -> u32 {
        VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32
            + u32::try_from(payload.len()).expect("test payload length should fit")
    }

    fn write_guest_bytes(memory: &mut GuestMemory, address: GuestAddress, bytes: &[u8]) {
        memory
            .write_slice(bytes, address)
            .expect("test guest bytes should write");
    }

    fn read_guest_bytes(memory: &GuestMemory, address: GuestAddress, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        memory
            .read_slice(&mut bytes, address)
            .expect("test guest bytes should read");
        bytes
    }

    fn read_guest_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("test guest u16 should read");
        u16::from_le_bytes(bytes)
    }

    fn read_guest_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("test guest u32 should read");
        u32::from_le_bytes(bytes)
    }

    fn vsock_payload_address_after_header(header_address: GuestAddress) -> GuestAddress {
        header_address
            .checked_add(VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64)
            .expect("test vsock payload address should not overflow")
    }

    fn parse_vsock_tx_packet(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<VirtioVsockTxPacket, VirtioVsockTxPacketParseError> {
        VirtioVsockTxPacket::parse(memory, chain)
    }

    fn vsock_tx_queue() -> VirtioVsockTxQueue {
        vsock_tx_queue_with_used_ring(TEST_VSOCK_TX_USED_RING)
    }

    fn vsock_tx_queue_with_used_ring(device_ring: GuestAddress) -> VirtioVsockTxQueue {
        let mut queues = VirtioMmioQueueRegisters::new(&VIRTIO_VSOCK_QUEUE_SIZES)
            .expect("queue table should build");
        let queue_index =
            u32::try_from(VIRTIO_VSOCK_TX_QUEUE_INDEX).expect("TX queue index should fit");
        queues
            .write_register(
                VirtioMmioRegister::QueueSel,
                queue_index,
                QUEUE_CONFIG_STATUS,
            )
            .expect("TX queue select should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueNum,
                u32::from(TEST_VSOCK_QUEUE_SIZE),
                QUEUE_CONFIG_STATUS,
            )
            .expect("TX queue size should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(TEST_VSOCK_TX_DESCRIPTOR_TABLE),
                QUEUE_CONFIG_STATUS,
            )
            .expect("TX descriptor table should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(TEST_VSOCK_TX_AVAILABLE_RING),
                QUEUE_CONFIG_STATUS,
            )
            .expect("TX available ring should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                guest_address_low(device_ring),
                QUEUE_CONFIG_STATUS,
            )
            .expect("TX used ring should write");
        queues
            .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
            .expect("TX queue ready should write");
        let queue_state = *queues
            .queue(queue_index)
            .expect("TX queue state should be configured");
        VirtioVsockTxQueue::from_mmio_queue_state(queue_state).expect("TX queue should build")
    }

    fn write_vsock_rx_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_VSOCK_RX_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("vsock RX descriptor should write");
    }

    fn write_vsock_rx_available_index(memory: &mut GuestMemory, index: u16) {
        let index_address = TEST_VSOCK_RX_AVAILABLE_RING
            .checked_add(2)
            .expect("available index address should not overflow");
        memory
            .write_slice(&index.to_le_bytes(), index_address)
            .expect("vsock RX available index should write");
    }

    fn write_vsock_rx_available_entry(memory: &mut GuestMemory, slot: usize, head: u16) {
        let slot_offset = u64::try_from(slot).expect("available slot should fit") * 2;
        let entry_address = TEST_VSOCK_RX_AVAILABLE_RING
            .checked_add(4 + slot_offset)
            .expect("available entry address should not overflow");
        memory
            .write_slice(&head.to_le_bytes(), entry_address)
            .expect("vsock RX available entry should write");
    }

    fn write_vsock_rx_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (slot, head) in heads.iter().copied().enumerate() {
            write_vsock_rx_available_entry(memory, slot, head);
        }
        write_vsock_rx_available_index(
            memory,
            u16::try_from(heads.len()).expect("available head count should fit"),
        );
    }

    fn append_vsock_rx_available_head(
        memory: &mut GuestMemory,
        slot: usize,
        head: u16,
        next_available: u16,
    ) {
        write_vsock_rx_available_entry(memory, slot, head);
        write_vsock_rx_available_index(memory, next_available);
    }

    fn vsock_rx_used_ring_idx_address() -> GuestAddress {
        TEST_VSOCK_RX_USED_RING
            .checked_add(2)
            .expect("vsock RX used ring idx address should not overflow")
    }

    fn vsock_rx_used_ring_entry_address(index: usize) -> GuestAddress {
        TEST_VSOCK_RX_USED_RING
            .checked_add(4 + u64::try_from(index).expect("used ring index should fit") * 8)
            .expect("vsock RX used ring entry address should not overflow")
    }

    fn read_vsock_rx_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, vsock_rx_used_ring_idx_address())
    }

    fn read_vsock_rx_used_element(memory: &GuestMemory, index: usize) -> (u32, u32) {
        let address = vsock_rx_used_ring_entry_address(index);
        let descriptor_head = read_guest_u32(memory, address);
        let len = read_guest_u32(
            memory,
            address
                .checked_add(4)
                .expect("vsock RX used ring len address should not overflow"),
        );
        (descriptor_head, len)
    }

    fn read_vsock_packet_header(
        memory: &GuestMemory,
        address: GuestAddress,
    ) -> VirtioVsockPacketHeader {
        let mut bytes = [0; VIRTIO_VSOCK_PACKET_HEADER_SIZE];
        memory
            .read_slice(&mut bytes, address)
            .expect("vsock packet header should read");
        VirtioVsockPacketHeader::try_from_bytes(bytes).expect("vsock packet header should parse")
    }

    fn vsock_rx_queue() -> VirtioVsockRxQueue {
        vsock_rx_queue_with_used_ring(TEST_VSOCK_RX_USED_RING)
    }

    fn vsock_rx_queue_with_used_ring(device_ring: GuestAddress) -> VirtioVsockRxQueue {
        let mut queues = VirtioMmioQueueRegisters::new(&VIRTIO_VSOCK_QUEUE_SIZES)
            .expect("queue table should build");
        let queue_index =
            u32::try_from(VIRTIO_VSOCK_RX_QUEUE_INDEX).expect("RX queue index should fit");
        queues
            .write_register(
                VirtioMmioRegister::QueueSel,
                queue_index,
                QUEUE_CONFIG_STATUS,
            )
            .expect("RX queue select should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueNum,
                u32::from(TEST_VSOCK_QUEUE_SIZE),
                QUEUE_CONFIG_STATUS,
            )
            .expect("RX queue size should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(TEST_VSOCK_RX_DESCRIPTOR_TABLE),
                QUEUE_CONFIG_STATUS,
            )
            .expect("RX descriptor table should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(TEST_VSOCK_RX_AVAILABLE_RING),
                QUEUE_CONFIG_STATUS,
            )
            .expect("RX available ring should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                guest_address_low(device_ring),
                QUEUE_CONFIG_STATUS,
            )
            .expect("RX used ring should write");
        queues
            .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
            .expect("RX queue ready should write");
        let queue_state = *queues
            .queue(queue_index)
            .expect("RX queue state should be configured");
        VirtioVsockRxQueue::from_mmio_queue_state(queue_state).expect("RX queue should build")
    }

    #[test]
    fn accepts_minimal_config() {
        let config =
            validate(VsockConfigInput::new(MIN_GUEST_CID, "./v.sock")).expect("valid config");

        assert_eq!(config.vsock_id(), None);
        assert_eq!(config.guest_cid(), MIN_GUEST_CID);
        assert_eq!(config.uds_path(), Path::new("./v.sock"));
    }

    #[test]
    fn accepts_optional_deprecated_vsock_id() {
        let config = validate(VsockConfigInput::new(42, "/tmp/v.sock").with_vsock_id("vsock_0"))
            .expect("valid config");

        assert_eq!(config.vsock_id(), Some("vsock_0"));
        assert_eq!(config.guest_cid(), 42);
        assert_eq!(config.uds_path(), Path::new("/tmp/v.sock"));
    }

    #[test]
    fn rejects_guest_cid_below_firecracker_minimum() {
        let err = validate(VsockConfigInput::new(2, "/tmp/v.sock"))
            .expect_err("small guest cid should fail");

        assert_eq!(
            err,
            VsockConfigError::GuestCidTooSmall {
                guest_cid: 2,
                min: MIN_GUEST_CID,
            }
        );
        assert_eq!(err.to_string(), "vsock guest_cid 2 is below minimum 3");
    }

    #[test]
    fn rejects_empty_vsock_id() {
        let err = validate(VsockConfigInput::new(3, "/tmp/v.sock").with_vsock_id(""))
            .expect_err("empty id should fail");

        assert_eq!(err, VsockConfigError::EmptyVsockId);
        assert_eq!(err.to_string(), "vsock_id must not be empty");
    }

    #[test]
    fn rejects_control_character_vsock_id_without_echoing_it() {
        let invalid = "id\nsecret";
        let err = validate(VsockConfigInput::new(3, "/tmp/v.sock").with_vsock_id(invalid))
            .expect_err("control character id should fail");

        assert_eq!(
            err,
            VsockConfigError::InvalidVsockId {
                vsock_id: invalid.to_string(),
            }
        );
        assert_eq!(
            err.to_string(),
            "vsock_id must not contain control characters"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn rejects_empty_socket_path() {
        let err =
            validate(VsockConfigInput::new(3, "")).expect_err("empty socket path should fail");

        assert_eq!(err, VsockConfigError::EmptySocketPath);
        assert_eq!(err.to_string(), "vsock uds_path must not be empty");
    }

    #[test]
    fn rejects_control_character_socket_path_without_echoing_it() {
        let invalid = "/tmp/v.sock\nsecret";
        let err = validate(VsockConfigInput::new(3, invalid))
            .expect_err("control character socket path should fail");

        assert_eq!(err, VsockConfigError::InvalidSocketPath);
        assert_eq!(
            err.to_string(),
            "vsock uds_path must not contain control characters"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn errors_have_no_sources() {
        assert!(VsockConfigError::EmptySocketPath.source().is_none());
    }

    #[test]
    fn parses_host_connect_request() {
        let request = parse_vsock_host_connect_request(b"CONNECT 1024\n")
            .expect("host CONNECT request should parse");

        assert_eq!(request.guest_port(), 1024);
    }

    #[test]
    fn host_connect_request_parse_method_delegates_to_parser() {
        let request = VsockHostConnectRequest::parse(b"CONNECT 2048\n")
            .expect("host CONNECT request should parse");

        assert_eq!(request.guest_port(), 2048);
    }

    #[test]
    fn parses_host_connect_request_case_insensitive_command() {
        let request = parse_vsock_host_connect_request(b"cOnNeCt 52\n")
            .expect("host CONNECT request should parse");

        assert_eq!(request.guest_port(), 52);
    }

    #[test]
    fn parses_host_connect_request_with_whitespace() {
        let request = parse_vsock_host_connect_request(b"  CONNECT\t52  \n")
            .expect("host CONNECT request should parse");

        assert_eq!(request.guest_port(), 52);
    }

    #[test]
    fn parses_host_connect_request_port_boundaries() {
        let zero = parse_vsock_host_connect_request(b"CONNECT 0\n")
            .expect("zero port should parse like Firecracker");
        let max = parse_vsock_host_connect_request(b"CONNECT 4294967295\n")
            .expect("u32 max port should parse");

        assert_eq!(zero.guest_port(), 0);
        assert_eq!(max.guest_port(), u32::MAX);
    }

    #[test]
    fn rejects_empty_host_connect_request() {
        let err = parse_vsock_host_connect_request(b"")
            .expect_err("empty host CONNECT request should fail");

        assert_eq!(err, VsockHostConnectRequestError::Empty);
        assert_eq!(
            err.to_string(),
            "vsock host CONNECT request must not be empty"
        );
    }

    #[test]
    fn rejects_overlong_host_connect_request_without_echoing_it() {
        let input = [b'x'; VSOCK_HOST_CONNECT_REQUEST_MAX_LEN + 1];
        let err = parse_vsock_host_connect_request(&input)
            .expect_err("overlong host CONNECT request should fail");

        assert_eq!(
            err,
            VsockHostConnectRequestError::TooLong {
                len: VSOCK_HOST_CONNECT_REQUEST_MAX_LEN + 1,
                max: VSOCK_HOST_CONNECT_REQUEST_MAX_LEN,
            }
        );
        assert!(!err.to_string().contains("xxx"));
    }

    #[test]
    fn rejects_host_connect_request_missing_newline() {
        let err = parse_vsock_host_connect_request(b"CONNECT 42")
            .expect_err("host CONNECT request without newline should fail");

        assert_eq!(err, VsockHostConnectRequestError::MissingNewline);
    }

    #[test]
    fn rejects_host_connect_request_trailing_data_after_newline() {
        let err = parse_vsock_host_connect_request(b"CONNECT 42\npayload")
            .expect_err("host CONNECT request with trailing data should fail");

        assert_eq!(err, VsockHostConnectRequestError::TrailingData);
    }

    #[test]
    fn rejects_host_connect_request_invalid_utf8() {
        let err = parse_vsock_host_connect_request(b"CONNECT \xff\n")
            .expect_err("non UTF-8 host CONNECT request should fail");

        assert_eq!(err, VsockHostConnectRequestError::InvalidUtf8);
    }

    #[test]
    fn rejects_host_connect_request_missing_command() {
        let err = parse_vsock_host_connect_request(b"\n")
            .expect_err("host CONNECT request without command should fail");

        assert_eq!(err, VsockHostConnectRequestError::MissingCommand);
    }

    #[test]
    fn rejects_host_connect_request_invalid_command() {
        let err = parse_vsock_host_connect_request(b"OPEN 42\n")
            .expect_err("host CONNECT request with wrong command should fail");

        assert_eq!(err, VsockHostConnectRequestError::InvalidCommand);
    }

    #[test]
    fn rejects_host_connect_request_missing_port() {
        let err = parse_vsock_host_connect_request(b"CONNECT\n")
            .expect_err("host CONNECT request without port should fail");

        assert_eq!(err, VsockHostConnectRequestError::MissingPort);
    }

    #[test]
    fn rejects_host_connect_request_invalid_port() {
        for input in [
            &b"CONNECT port\n"[..],
            &b"CONNECT -1\n"[..],
            &b"CONNECT +1\n"[..],
            &b"CONNECT 4294967296\n"[..],
        ] {
            let err = parse_vsock_host_connect_request(input)
                .expect_err("host CONNECT request with invalid port should fail");

            assert_eq!(err, VsockHostConnectRequestError::InvalidPort);
        }
    }

    #[test]
    fn rejects_host_connect_request_extra_tokens() {
        let err = parse_vsock_host_connect_request(b"CONNECT 42 extra\n")
            .expect_err("host CONNECT request with extra tokens should fail");

        assert_eq!(err, VsockHostConnectRequestError::ExtraTokens);
    }

    #[test]
    fn host_connect_errors_have_no_sources() {
        assert!(VsockHostConnectRequestError::InvalidPort.source().is_none());
    }

    #[test]
    fn host_connect_handshake_reads_complete_request() {
        let (mut accepted, mut client) = accepted_host_connection("connect-ok");
        client
            .write_all(b"CONNECT 52\n")
            .expect("client should write handshake");

        let request = accepted
            .read_connect_request()
            .expect("handshake read should succeed")
            .expect("handshake should be complete");

        assert_eq!(request.guest_port(), 52);
    }

    #[test]
    fn host_connect_handshake_caches_complete_request_without_consuming_payload() {
        let (mut accepted, mut client) = accepted_host_connection("connect-cached");
        client
            .write_all(b"CONNECT 52\npayload")
            .expect("client should write handshake and payload");

        let request = accepted
            .read_connect_request()
            .expect("handshake read should succeed")
            .expect("handshake should be complete");
        assert_eq!(request.guest_port(), 52);

        let repeated = accepted
            .read_connect_request()
            .expect("cached handshake should succeed")
            .expect("cached handshake should be complete");
        assert_eq!(repeated, request);

        let mut payload = [0; 7];
        accepted
            .into_stream()
            .read_exact(&mut payload)
            .expect("payload should remain unread after handshake");
        assert_eq!(&payload, b"payload");
    }

    #[test]
    fn host_connect_handshake_empty_nonblocking_stream_returns_none() {
        let (mut accepted, _client) = accepted_host_connection("connect-empty");

        let request = accepted
            .read_connect_request()
            .expect("empty nonblocking read should not fail");

        assert!(request.is_none());
    }

    #[test]
    fn host_connect_handshake_preserves_partial_request_across_reads() {
        let (mut accepted, mut client) = accepted_host_connection("connect-split");
        client
            .write_all(b"CONN")
            .expect("client should write partial handshake");

        let partial = accepted
            .read_connect_request()
            .expect("partial nonblocking read should not fail");
        assert!(partial.is_none());

        client
            .write_all(b"ECT 4096\n")
            .expect("client should finish handshake");
        let request = accepted
            .read_connect_request()
            .expect("completed handshake read should succeed")
            .expect("handshake should be complete");

        assert_eq!(request.guest_port(), 4096);
    }

    #[test]
    fn host_connect_handshake_accepts_exact_max_length_request() {
        let (mut accepted, mut client) = accepted_host_connection("connect-max");
        let exact_max = b"CONNECT 4294967295             \n";
        assert_eq!(exact_max.len(), VSOCK_HOST_CONNECT_REQUEST_MAX_LEN);
        client
            .write_all(exact_max)
            .expect("client should write exact maximum handshake");

        let request = accepted
            .read_connect_request()
            .expect("maximum handshake read should succeed")
            .expect("maximum handshake should be complete");

        assert_eq!(request.guest_port(), u32::MAX);
    }

    #[test]
    fn host_connect_handshake_rejects_overlong_request_without_unbounded_buffering() {
        let (mut accepted, mut client) = accepted_host_connection("connect-long");
        let overlong = [b'x'; VSOCK_HOST_CONNECT_REQUEST_MAX_LEN + 1];
        client
            .write_all(&overlong)
            .expect("client should write overlong handshake");

        let err = accepted
            .read_connect_request()
            .expect_err("overlong handshake should fail");

        assert_eq!(
            err,
            VsockHostConnectHandshakeError::TooLong {
                max: VSOCK_HOST_CONNECT_REQUEST_MAX_LEN,
            }
        );
    }

    #[test]
    fn host_connect_handshake_propagates_parse_error_source() {
        let (mut accepted, mut client) = accepted_host_connection("connect-bad");
        client
            .write_all(b"OPEN 52\n")
            .expect("client should write invalid handshake");

        let err = accepted
            .read_connect_request()
            .expect_err("invalid handshake should fail");

        assert!(matches!(
            err,
            VsockHostConnectHandshakeError::Parse {
                source: VsockHostConnectRequestError::InvalidCommand
            }
        ));
        assert!(err.source().is_some());
        assert_eq!(
            err.to_string(),
            "invalid vsock host CONNECT request: vsock host CONNECT request command is invalid"
        );
    }

    #[test]
    fn host_connect_handshake_reports_closed_stream_before_request() {
        let (mut accepted, client) = accepted_host_connection("connect-eof");

        drop(client);

        let err = accepted
            .read_connect_request()
            .expect_err("closed stream before handshake should fail");

        assert_eq!(err, VsockHostConnectHandshakeError::Closed);
    }

    #[test]
    fn host_connect_handshake_reports_closed_stream_after_partial_request() {
        let (mut accepted, mut client) = accepted_host_connection("connect-partial-eof");
        client
            .write_all(b"CONN")
            .expect("client should write partial handshake");
        drop(client);

        let err = accepted
            .read_connect_request()
            .expect_err("closed stream after partial handshake should fail");

        assert_eq!(err, VsockHostConnectHandshakeError::Closed);
    }

    #[test]
    fn host_connect_handshake_errors_have_expected_sources_and_no_paths() {
        let read = VsockHostConnectHandshakeError::Read(std::io::ErrorKind::PermissionDenied);
        let closed = VsockHostConnectHandshakeError::Closed;
        let too_long = VsockHostConnectHandshakeError::TooLong {
            max: VSOCK_HOST_CONNECT_REQUEST_MAX_LEN,
        };
        let parse = VsockHostConnectHandshakeError::Parse {
            source: VsockHostConnectRequestError::MissingPort,
        };

        assert!(read.source().is_none());
        assert!(closed.source().is_none());
        assert!(too_long.source().is_none());
        assert!(parse.source().is_some());
        assert_eq!(
            read.to_string(),
            "failed to read vsock host CONNECT request: PermissionDenied"
        );
        assert_eq!(
            closed.to_string(),
            "vsock host connection closed before CONNECT request"
        );
        assert_eq!(
            too_long.to_string(),
            "vsock host CONNECT request exceeds maximum length 32"
        );
        assert_eq!(
            parse.to_string(),
            "invalid vsock host CONNECT request: vsock host CONNECT request is missing port"
        );
    }

    #[test]
    fn host_connect_handshake_classifies_transient_read_errors() {
        assert!(is_transient_host_socket_read_error(
            std::io::ErrorKind::WouldBlock
        ));
        assert!(is_transient_host_socket_read_error(
            std::io::ErrorKind::Interrupted
        ));
        assert!(!is_transient_host_socket_read_error(
            std::io::ErrorKind::ConnectionReset
        ));
    }

    #[test]
    fn host_local_port_accepts_firecracker_range_boundaries() {
        let first = VsockHostLocalPort::try_from_raw(VSOCK_HOST_LOCAL_PORT_BASE)
            .expect("port should parse");
        let last = VsockHostLocalPort::try_from_raw(VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE - 1)
            .expect("port should parse");

        assert_eq!(first.raw(), VSOCK_HOST_LOCAL_PORT_BASE);
        assert_eq!(last.raw(), VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE - 1);
        assert_eq!(VSOCK_HOST_LOCAL_PORT_CAPACITY, 1_u32 << 30);
    }

    #[test]
    fn host_local_port_rejects_raw_values_outside_firecracker_range() {
        for raw in [
            0,
            VSOCK_HOST_LOCAL_PORT_BASE - 1,
            VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE,
            u32::MAX,
        ] {
            let err = VsockHostLocalPort::try_from_raw(raw)
                .expect_err("raw host local port outside Firecracker range should fail");

            assert_eq!(
                err,
                VsockHostLocalPortError::InvalidRawPort {
                    raw,
                    min: VSOCK_HOST_LOCAL_PORT_BASE,
                    max_exclusive: VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE,
                }
            );
        }
    }

    #[test]
    fn host_local_port_allocator_allocates_first_and_sequential_ports() {
        let mut allocator = VsockHostLocalPortAllocator::new();

        let first = allocator.allocate().expect("first port should allocate");
        let second = allocator.allocate().expect("second port should allocate");
        let third = allocator.allocate().expect("third port should allocate");

        assert_eq!(first.raw(), VSOCK_HOST_LOCAL_PORT_BASE);
        assert_eq!(second.raw(), VSOCK_HOST_LOCAL_PORT_BASE + 1);
        assert_eq!(third.raw(), VSOCK_HOST_LOCAL_PORT_BASE + 2);
    }

    #[test]
    fn host_local_port_allocator_ports_have_firecracker_shape() {
        let mut allocator = VsockHostLocalPortAllocator::with_capacity(3);

        for _ in 0..3 {
            let port = allocator.allocate().expect("port should allocate").raw();

            assert_ne!(port & VSOCK_HOST_LOCAL_PORT_BASE, 0);
            assert_eq!(port & VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE, 0);
        }
    }

    #[test]
    fn host_local_port_allocator_reports_exhaustion_without_scanning() {
        let mut allocator = VsockHostLocalPortAllocator::with_capacity(0);

        let err = allocator
            .allocate()
            .expect_err("zero-capacity allocator should be exhausted");

        assert_eq!(err, VsockHostLocalPortAllocatorError::Exhausted);
        assert_eq!(
            err.to_string(),
            "vsock host local port allocation exhausted"
        );
    }

    #[test]
    fn host_local_port_allocator_reuses_freed_ports_after_range_exhaustion() {
        let mut allocator = VsockHostLocalPortAllocator::with_capacity(3);
        let first = allocator.allocate().expect("first port should allocate");
        let second = allocator.allocate().expect("second port should allocate");
        let third = allocator.allocate().expect("third port should allocate");

        assert!(matches!(
            allocator.allocate(),
            Err(VsockHostLocalPortAllocatorError::Exhausted)
        ));

        assert!(allocator.free(second));
        let reused_second = allocator
            .allocate()
            .expect("freed port should allocate after exhaustion");
        assert_eq!(reused_second, second);

        assert!(allocator.free(first));
        let reused_first = allocator
            .allocate()
            .expect("freed port should allocate after exhaustion");
        assert_eq!(reused_first, first);

        assert!(allocator.free(third));
        let reused_third = allocator
            .allocate()
            .expect("freed port should allocate after exhaustion");
        assert_eq!(reused_third, third);
    }

    #[test]
    fn host_local_port_allocator_ignores_unknown_and_duplicate_free() {
        let mut allocator = VsockHostLocalPortAllocator::with_capacity(2);
        let first = allocator.allocate().expect("first port should allocate");
        let second = allocator.allocate().expect("second port should allocate");

        assert!(allocator.free(first));
        assert!(!allocator.free(first));
        assert!(
            !allocator.free(
                VsockHostLocalPort::try_from_raw(VSOCK_HOST_LOCAL_PORT_BASE + 7)
                    .expect("test port should be in Firecracker range")
            )
        );
        assert_eq!(
            allocator
                .allocate()
                .expect("freed port should allocate after exhaustion"),
            first
        );
        assert!(allocator.free(second));
    }

    #[test]
    fn host_local_port_allocator_ignores_never_allocated_ports_within_capacity() {
        let mut allocator = VsockHostLocalPortAllocator::with_capacity(3);
        let first = allocator.allocate().expect("first port should allocate");
        let second = allocator.allocate().expect("second port should allocate");
        let third = VsockHostLocalPort::try_from_raw(VSOCK_HOST_LOCAL_PORT_BASE + 2)
            .expect("test port should be in Firecracker range");

        assert!(!allocator.free(third));
        assert_eq!(
            allocator.allocate().expect("third port should allocate"),
            third
        );
        assert!(matches!(
            allocator.allocate(),
            Err(VsockHostLocalPortAllocatorError::Exhausted)
        ));
        assert!(allocator.free(first));
        assert!(allocator.free(second));
        assert!(allocator.free(third));
    }

    #[test]
    fn host_local_port_allocator_ignores_ports_outside_allocator_capacity() {
        let mut allocator = VsockHostLocalPortAllocator::with_capacity(1);
        let first = allocator.allocate().expect("first port should allocate");
        let outside_capacity = VsockHostLocalPort::try_from_raw(VSOCK_HOST_LOCAL_PORT_BASE + 1)
            .expect("test port should be in Firecracker range");

        assert!(!allocator.free(outside_capacity));
        assert!(allocator.free(first));
    }

    #[test]
    fn host_local_port_errors_have_no_sources() {
        assert!(
            VsockHostLocalPortError::InvalidRawPort {
                raw: 0,
                min: VSOCK_HOST_LOCAL_PORT_BASE,
                max_exclusive: VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE,
            }
            .source()
            .is_none()
        );
        assert!(
            VsockHostLocalPortAllocatorError::Exhausted
                .source()
                .is_none()
        );
    }

    #[test]
    fn vsock_host_connection_table_inserts_first_connection_with_firecracker_local_port() {
        let mut table = VsockHostConnectionTable::new();

        let (key, _client) =
            insert_accepted_host_connection_for_test(&mut table, "table-first", 1024);

        assert_eq!(key.local_port().raw(), VSOCK_HOST_LOCAL_PORT_BASE);
        assert_eq!(key.peer_port(), 1024);
        assert!(table.contains(key));
        assert!(table.get(key).is_some());
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());
    }

    #[test]
    fn vsock_host_connection_table_retains_accepted_stream() {
        let mut table = VsockHostConnectionTable::new();
        let (key, mut client) =
            insert_accepted_host_connection_for_test(&mut table, "table-retain", 1024);

        client
            .write_all(b"payload")
            .expect("client should write retained payload");

        let mut retained_stream = table
            .get(key)
            .expect("retained connection should exist")
            .stream();
        let mut payload = [0; 7];
        retained_stream
            .read_exact(&mut payload)
            .expect("retained stream should receive payload");

        assert_eq!(&payload, b"payload");
    }

    #[test]
    fn vsock_host_connection_table_takes_pending_request_packet_header() {
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) =
            insert_accepted_host_connection_for_test(&mut table, "table-request", 1024);

        assert!(
            table
                .get(key)
                .expect("retained connection should exist")
                .has_pending_request_packet()
        );
        let header = table
            .take_pending_request_packet_header(key, 42)
            .expect("pending request packet header should exist");

        assert_host_connection_request_header(header, 42, key.local_port(), 1024);
        assert!(
            !table
                .get(key)
                .expect("retained connection should still exist")
                .has_pending_request_packet()
        );
        assert!(table.take_pending_request_packet_header(key, 42).is_none());
        assert!(table.contains(key));
    }

    #[test]
    fn vsock_host_connection_table_acknowledges_guest_response() {
        let mut table = VsockHostConnectionTable::new();
        let (key, mut client) =
            insert_accepted_host_connection_for_test(&mut table, "table-response", 4000);
        let request = table
            .take_pending_request_packet_header(key, 42)
            .expect("pending request should be delivered before response");
        assert_host_connection_request_header(request, 42, key.local_port(), 4000);

        let outcome = table.acknowledge_guest_response_packet(
            &guest_response_tx_packet(42, key.local_port(), 4000),
            42,
        );

        assert_eq!(
            outcome,
            Some(super::VsockHostGuestResponseOutcome::Acknowledged { key })
        );
        assert!(
            table
                .get(key)
                .expect("acknowledged connection should stay retained")
                .host_ack_sent()
        );
        assert_host_ok_message(&mut client, key.local_port());
    }

    #[test]
    fn vsock_host_connection_table_ignores_guest_response_before_request_delivery() {
        let mut table = VsockHostConnectionTable::new();
        let (key, mut client) =
            insert_accepted_host_connection_for_test(&mut table, "table-pending-response", 4000);

        let outcome = table.acknowledge_guest_response_packet(
            &guest_response_tx_packet(42, key.local_port(), 4000),
            42,
        );

        assert_guest_response_ignored(
            outcome,
            super::VsockHostGuestResponseIgnoreReason::RequestStillPending,
        );
        assert!(
            !table
                .get(key)
                .expect("ignored response should keep connection")
                .host_ack_sent()
        );
        assert_no_host_message(&mut client);
    }

    #[test]
    fn vsock_host_connection_table_ignores_duplicate_guest_response() {
        let mut table = VsockHostConnectionTable::new();
        let (key, mut client) =
            insert_accepted_host_connection_for_test(&mut table, "table-duplicate-response", 4000);
        let _request = table
            .take_pending_request_packet_header(key, 42)
            .expect("pending request should be delivered before response");
        let packet = guest_response_tx_packet(42, key.local_port(), 4000);

        assert_eq!(
            table.acknowledge_guest_response_packet(&packet, 42),
            Some(super::VsockHostGuestResponseOutcome::Acknowledged { key })
        );
        assert_host_ok_message(&mut client, key.local_port());
        assert_guest_response_ignored(
            table.acknowledge_guest_response_packet(&packet, 42),
            super::VsockHostGuestResponseIgnoreReason::AlreadyAcknowledged,
        );
        assert_no_host_message(&mut client);
        assert!(table.contains(key));
    }

    #[test]
    fn vsock_host_connection_table_acknowledges_only_matching_guest_response() {
        let mut table = VsockHostConnectionTable::new();
        let (first_key, mut first_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-first-response", 4000);
        let (second_key, mut second_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-second-response", 5000);
        let _first_request = table
            .take_pending_request_packet_header(first_key, 42)
            .expect("first pending request should be delivered before response");
        let _second_request = table
            .take_pending_request_packet_header(second_key, 42)
            .expect("second pending request should be delivered before response");

        let outcome = table.acknowledge_guest_response_packet(
            &guest_response_tx_packet(42, second_key.local_port(), 5000),
            42,
        );

        assert_eq!(
            outcome,
            Some(super::VsockHostGuestResponseOutcome::Acknowledged { key: second_key })
        );
        assert!(
            !table
                .get(first_key)
                .expect("unmatched connection should stay retained")
                .host_ack_sent()
        );
        assert!(
            table
                .get(second_key)
                .expect("matched connection should stay retained")
                .host_ack_sent()
        );
        assert_no_host_message(&mut first_client);
        assert_host_ok_message(&mut second_client, second_key.local_port());
    }

    #[test]
    fn vsock_host_connection_table_ignores_unroutable_guest_responses() {
        let mut table = VsockHostConnectionTable::new();
        let (key, mut client) =
            insert_accepted_host_connection_for_test(&mut table, "table-ignore-response", 4000);
        let _request = table
            .take_pending_request_packet_header(key, 42)
            .expect("pending request should be delivered before response");
        let valid_header = guest_response_tx_packet(42, key.local_port(), 4000).header();

        assert_guest_response_ignored(
            table.acknowledge_guest_response_packet(
                &guest_response_tx_packet_with_header(valid_header.with_packet_type(0)),
                42,
            ),
            super::VsockHostGuestResponseIgnoreReason::UnsupportedPacketType,
        );
        assert_guest_response_ignored(
            table.acknowledge_guest_response_packet(
                &guest_response_tx_packet_with_header(valid_header.with_payload_len(1)),
                42,
            ),
            super::VsockHostGuestResponseIgnoreReason::PayloadPresent,
        );
        assert_guest_response_ignored(
            table.acknowledge_guest_response_packet(
                &guest_response_tx_packet_with_header(valid_header.with_src_cid(43)),
                42,
            ),
            super::VsockHostGuestResponseIgnoreReason::WrongSourceCid,
        );
        assert_guest_response_ignored(
            table.acknowledge_guest_response_packet(
                &guest_response_tx_packet_with_header(valid_header.with_dst_cid(3)),
                42,
            ),
            super::VsockHostGuestResponseIgnoreReason::WrongDestinationCid,
        );
        assert_guest_response_ignored(
            table.acknowledge_guest_response_packet(
                &guest_response_tx_packet_with_header(valid_header.with_dst_port(1)),
                42,
            ),
            super::VsockHostGuestResponseIgnoreReason::InvalidLocalPort,
        );
        assert_guest_response_ignored(
            table.acknowledge_guest_response_packet(
                &guest_response_tx_packet(42, key.local_port(), 4001),
                42,
            ),
            super::VsockHostGuestResponseIgnoreReason::MissingConnection,
        );

        assert!(
            !table
                .get(key)
                .expect("ignored responses should keep connection")
                .host_ack_sent()
        );
        assert_no_host_message(&mut client);
    }

    #[test]
    fn vsock_host_connection_table_drops_connection_when_guest_response_ack_fails() {
        let mut table = VsockHostConnectionTable::with_local_port_capacity(1);
        let (key, client) =
            insert_accepted_host_connection_for_test(&mut table, "table-response-fail", 4000);
        let _request = table
            .take_pending_request_packet_header(key, 42)
            .expect("pending request should be delivered before response");
        drop(client);

        let outcome = table.acknowledge_guest_response_packet(
            &guest_response_tx_packet(42, key.local_port(), 4000),
            42,
        );

        assert!(matches!(
            outcome,
            Some(super::VsockHostGuestResponseOutcome::Dropped {
                key: dropped_key,
                ..
            }) if dropped_key == key
        ));
        assert!(!table.contains(key));
        let (accepted, mut next_client, request) =
            accepted_host_connection_with_request("table-response-reuse", 4001);
        let next_key = table
            .insert_accepted_host_connection(accepted, request)
            .expect("local port should be reusable after failed response ack");
        assert_eq!(next_key.local_port(), key.local_port());

        drop(table);
        assert_stream_closed(
            &mut next_client,
            "dropped table should drop reused connection stream",
        );
    }

    #[test]
    fn vsock_host_connection_table_selects_smallest_pending_request_key() {
        let mut table = VsockHostConnectionTable::new();
        let (first_key, _first_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-pending-first", 4000);
        let (second_key, _second_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-pending-second", 3000);

        assert_eq!(table.first_pending_request_packet_key(), Some(first_key));

        let first_header = table
            .take_pending_request_packet_header(first_key, 42)
            .expect("first pending request should exist");

        assert_host_connection_request_header(first_header, 42, first_key.local_port(), 4000);
        assert_eq!(table.first_pending_request_packet_key(), Some(second_key));

        let second_header = table
            .take_pending_request_packet_header(second_key, 42)
            .expect("second pending request should exist");

        assert_host_connection_request_header(second_header, 42, second_key.local_port(), 3000);
        assert_eq!(table.first_pending_request_packet_key(), None);
    }

    #[test]
    fn vsock_host_connection_table_request_packet_header_accepts_boundaries() {
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) = insert_accepted_host_connection_for_test(
            &mut table,
            "table-request-boundary",
            u32::MAX,
        );

        let header = table
            .take_pending_request_packet_header(key, u32::MAX)
            .expect("boundary request packet header should exist");

        assert_host_connection_request_header(header, u32::MAX, key.local_port(), u32::MAX);
    }

    #[test]
    fn vsock_host_connection_table_missing_request_packet_header_is_noop() {
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) =
            insert_accepted_host_connection_for_test(&mut table, "table-request-missing", 3000);
        let missing = VsockHostConnectionKey::new(key.local_port(), 3001);

        assert!(
            table
                .take_pending_request_packet_header(missing, 42)
                .is_none()
        );
        assert!(
            table
                .get(key)
                .expect("retained connection should still exist")
                .has_pending_request_packet()
        );

        let header = table
            .take_pending_request_packet_header(key, 42)
            .expect("active connection request packet header should still exist");
        assert_host_connection_request_header(header, 42, key.local_port(), 3000);
    }

    #[test]
    fn vsock_host_connection_table_reused_key_gets_fresh_request_packet_header() {
        let mut table = VsockHostConnectionTable::with_local_port_capacity(1);
        let (first, mut first_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-request-reused-a", 4000);

        assert!(
            table
                .take_pending_request_packet_header(first, 42)
                .is_some()
        );
        assert!(table.remove(first));
        assert_stream_closed(
            &mut first_client,
            "removed connection should drop retained stream",
        );

        let (second, _second_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-request-reused-b", 4000);

        assert_eq!(second, first);
        let header = table
            .take_pending_request_packet_header(second, 42)
            .expect("reused key should have a fresh request packet header");
        assert_host_connection_request_header(header, 42, second.local_port(), 4000);
    }

    #[test]
    fn vsock_host_connection_table_allows_same_peer_port_with_distinct_local_ports() {
        let mut table = VsockHostConnectionTable::new();

        let (first, _first_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-same-first", 2048);
        let (second, _second_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-same-second", 2048);

        assert_ne!(first, second);
        assert_eq!(first.peer_port(), second.peer_port());
        assert_eq!(first.local_port().raw(), VSOCK_HOST_LOCAL_PORT_BASE);
        assert_eq!(second.local_port().raw(), VSOCK_HOST_LOCAL_PORT_BASE + 1);
        assert!(table.contains(first));
        assert!(table.contains(second));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn vsock_host_connection_tables_keep_local_ports_independent() {
        let mut first_table = VsockHostConnectionTable::new();
        let mut second_table = VsockHostConnectionTable::new();

        let (first, _first_client) =
            insert_accepted_host_connection_for_test(&mut first_table, "table-independent-a", 2048);
        let (second, _second_client) = insert_accepted_host_connection_for_test(
            &mut second_table,
            "table-independent-b",
            2048,
        );

        assert_eq!(first.local_port().raw(), VSOCK_HOST_LOCAL_PORT_BASE);
        assert_eq!(second.local_port().raw(), VSOCK_HOST_LOCAL_PORT_BASE);
        assert!(first_table.contains(first));
        assert!(second_table.contains(second));
    }

    #[test]
    fn vsock_host_connection_table_accepts_peer_port_boundaries() {
        let mut table = VsockHostConnectionTable::with_local_port_capacity(2);

        let (zero, _zero_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-zero", 0);
        let (max, _max_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-max", u32::MAX);

        assert_eq!(zero.peer_port(), 0);
        assert_eq!(max.peer_port(), u32::MAX);
        assert_ne!(zero.local_port(), max.local_port());
    }

    #[test]
    fn vsock_host_connection_table_missing_remove_does_not_free_active_local_port() {
        let mut table = VsockHostConnectionTable::with_local_port_capacity(1);
        let (active, _active_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-missing", 3000);
        let missing = VsockHostConnectionKey::new(active.local_port(), 3001);
        let (accepted, mut client, request) =
            accepted_host_connection_with_request("table-missing-exhausted", 3002);

        assert!(!table.remove(missing));
        assert!(table.contains(active));
        assert_eq!(table.len(), 1);
        assert!(matches!(
            table.insert_accepted_host_connection(accepted, request),
            Err(VsockHostConnectionTableError::LocalPort(
                VsockHostLocalPortAllocatorError::Exhausted
            ))
        ));

        assert_stream_closed(&mut client, "failed insertion should drop supplied stream");
    }

    #[test]
    fn vsock_host_connection_table_remove_releases_local_port_for_reuse() {
        let mut table = VsockHostConnectionTable::with_local_port_capacity(1);
        let (first, mut first_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-remove-first", 4000);
        let (accepted, mut exhausted_client, request) =
            accepted_host_connection_with_request("table-remove-exhausted", 4001);

        assert!(matches!(
            table.insert_accepted_host_connection(accepted, request),
            Err(VsockHostConnectionTableError::LocalPort(
                VsockHostLocalPortAllocatorError::Exhausted
            ))
        ));
        assert!(table.remove(first));
        assert!(table.is_empty());
        assert!(table.get(first).is_none());

        assert_stream_closed(
            &mut first_client,
            "removed connection should drop retained stream",
        );
        assert_stream_closed(
            &mut exhausted_client,
            "exhausted insertion should drop supplied stream",
        );

        let (second, _second_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-remove-second", 4002);

        assert_eq!(second.local_port(), first.local_port());
        assert_eq!(second.peer_port(), 4002);
    }

    #[test]
    fn vsock_host_connection_table_drop_releases_retained_streams() {
        let mut client = {
            let mut table = VsockHostConnectionTable::new();
            let (_key, client) =
                insert_accepted_host_connection_for_test(&mut table, "table-drop", 4500);
            client
        };

        assert_stream_closed(&mut client, "dropped table should drop retained stream");
    }

    #[test]
    fn vsock_host_connection_table_propagates_local_port_exhaustion() {
        let mut table = VsockHostConnectionTable::with_local_port_capacity(0);
        let (accepted, mut client, request) =
            accepted_host_connection_with_request("table-exhausted", 5000);

        let err = table
            .insert_accepted_host_connection(accepted, request)
            .expect_err("zero-capacity table should be exhausted");

        assert_eq!(
            err,
            VsockHostConnectionTableError::LocalPort(VsockHostLocalPortAllocatorError::Exhausted)
        );
        assert_eq!(
            err.to_string(),
            "failed to allocate vsock host local port: vsock host local port allocation exhausted"
        );
        assert!(table.is_empty());

        assert_stream_closed(
            &mut client,
            "exhausted insertion should drop supplied stream",
        );
    }

    #[test]
    fn vsock_host_connection_table_rejects_duplicate_allocated_key() {
        let mut table = VsockHostConnectionTable::new();
        let (key, _active_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-duplicate", 6000);
        let (accepted, mut duplicate_client) = accepted_host_connection("table-duplicate-new");
        let duplicate = super::VsockHostConnection::from_accepted(accepted);

        let err = table
            .insert_allocated_connection_for_test(key, duplicate)
            .expect_err("duplicate key should fail");

        assert_eq!(err, VsockHostConnectionTableError::DuplicateKey { key });
        assert_eq!(
            err.to_string(),
            format!(
                "vsock host connection already exists for local port {} and peer port 6000",
                VSOCK_HOST_LOCAL_PORT_BASE
            )
        );
        assert!(table.contains(key));
        assert_eq!(table.len(), 1);

        assert_stream_closed(
            &mut duplicate_client,
            "duplicate insertion should drop supplied stream",
        );
    }

    #[test]
    fn vsock_host_connection_table_duplicate_key_does_not_release_active_local_port() {
        let mut table = VsockHostConnectionTable::with_local_port_capacity(1);
        let (key, _active_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-dupe-release", 6500);
        let (duplicate_accepted, mut duplicate_client) =
            accepted_host_connection("table-dupe-release-new");
        let duplicate = super::VsockHostConnection::from_accepted(duplicate_accepted);

        assert!(matches!(
            table.insert_allocated_connection_for_test(key, duplicate),
            Err(VsockHostConnectionTableError::DuplicateKey { .. })
        ));
        let (accepted, mut exhausted_client, request) =
            accepted_host_connection_with_request("table-dupe-exhausted", 6501);
        assert!(matches!(
            table.insert_accepted_host_connection(accepted, request),
            Err(VsockHostConnectionTableError::LocalPort(
                VsockHostLocalPortAllocatorError::Exhausted
            ))
        ));
        assert!(table.remove(key));

        assert_stream_closed(
            &mut duplicate_client,
            "duplicate insertion should drop supplied stream",
        );
        assert_stream_closed(
            &mut exhausted_client,
            "exhausted insertion should drop supplied stream",
        );

        let (reused, _reused_client) =
            insert_accepted_host_connection_for_test(&mut table, "table-dupe-reused", 6502);
        assert_eq!(reused.local_port(), key.local_port());
    }

    #[test]
    fn vsock_host_connection_table_errors_report_sources() {
        let local_port_err =
            VsockHostConnectionTableError::LocalPort(VsockHostLocalPortAllocatorError::Exhausted);
        let key = VsockHostConnectionKey::new(
            VsockHostLocalPort::try_from_raw(VSOCK_HOST_LOCAL_PORT_BASE)
                .expect("test local port should parse"),
            7000,
        );

        assert!(local_port_err.source().is_some());
        assert!(
            VsockHostConnectionTableError::DuplicateKey { key }
                .source()
                .is_none()
        );
    }

    #[test]
    fn host_socket_owner_binds_nonblocking_listener_and_cleans_up() {
        let path = unique_socket_path("owner");

        let owner = VsockHostSocketOwner::bind(&path).expect("host socket should bind");

        assert_eq!(owner.path(), path.as_path());
        assert!(path.exists());
        let err = owner
            .listener()
            .accept()
            .expect_err("idle nonblocking listener should not block");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);

        drop(owner);

        assert!(!path.exists());
    }

    #[test]
    fn host_socket_owner_accept_without_pending_connection_returns_none() {
        let path = unique_socket_path("accept-none");
        let owner = VsockHostSocketOwner::bind(&path).expect("host socket should bind");

        let accepted = owner
            .accept_host_connection()
            .expect("idle accept should not fail");

        assert!(accepted.is_none());
    }

    #[test]
    fn host_socket_owner_accepts_pending_connection_as_nonblocking_stream() {
        let path = unique_socket_path("accept-one");
        let owner = VsockHostSocketOwner::bind(&path).expect("host socket should bind");
        let _client = UnixStream::connect(&path).expect("client should connect");

        let accepted = owner
            .accept_host_connection()
            .expect("pending connection should accept")
            .expect("accepted connection should be present");

        let mut buffer = [0; 1];
        let mut accepted_stream = accepted.stream();
        let err = accepted_stream
            .read(&mut buffer)
            .expect_err("idle accepted stream should not block");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        assert!(
            owner
                .accept_host_connection()
                .expect("second idle accept should not fail")
                .is_none()
        );
    }

    #[test]
    fn host_socket_owner_drop_keeps_accepted_connection_usable() {
        let path = unique_socket_path("accept-drop");
        let owner = VsockHostSocketOwner::bind(&path).expect("host socket should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let accepted = owner
            .accept_host_connection()
            .expect("pending connection should accept")
            .expect("accepted connection should be present");

        drop(owner);

        assert!(!path.exists());
        let mut accepted_stream = accepted.into_stream();
        accepted_stream
            .set_nonblocking(false)
            .expect("test stream should switch to blocking mode");
        client
            .write_all(b"x")
            .expect("client should write to accepted stream");
        let mut buffer = [0; 1];
        accepted_stream
            .read_exact(&mut buffer)
            .expect("accepted stream should remain usable after listener drop");
        assert_eq!(buffer, [b'x']);
    }

    #[test]
    fn host_socket_accept_errors_have_no_sources_or_paths() {
        let not_attached = VsockHostSocketAcceptError::HostSocketNotAttached;
        let accept = VsockHostSocketAcceptError::Accept(std::io::ErrorKind::PermissionDenied);
        let nonblocking =
            VsockHostSocketAcceptError::SetNonblocking(std::io::ErrorKind::PermissionDenied);

        assert!(not_attached.source().is_none());
        assert!(accept.source().is_none());
        assert!(nonblocking.source().is_none());
        assert_eq!(
            not_attached.to_string(),
            "vsock host socket is not attached"
        );
        assert_eq!(
            accept.to_string(),
            "failed to accept vsock host connection: PermissionDenied"
        );
        assert_eq!(
            nonblocking.to_string(),
            "failed to set accepted vsock host connection nonblocking: PermissionDenied"
        );
    }

    #[test]
    fn host_socket_accept_classifies_transient_errors() {
        assert!(is_transient_host_socket_accept_error(
            std::io::ErrorKind::WouldBlock
        ));
        assert!(is_transient_host_socket_accept_error(
            std::io::ErrorKind::Interrupted
        ));
        assert!(is_transient_host_socket_accept_error(
            std::io::ErrorKind::ConnectionAborted
        ));
        assert!(!is_transient_host_socket_accept_error(
            std::io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn host_socket_owner_rejects_existing_file_without_unlinking() {
        let path = unique_socket_path("existing-file");
        fs::write(&path, "existing file").expect("fixture file should be written");

        let err =
            VsockHostSocketOwner::bind(&path).expect_err("existing file path should be rejected");

        assert_eq!(err, VsockHostSocketOwnerError::SocketPathExists);
        assert_eq!(
            fs::read_to_string(&path).expect("existing file should remain"),
            "existing file"
        );

        fs::remove_file(path).expect("fixture file should clean up");
    }

    #[test]
    fn host_socket_owner_rejects_existing_socket_without_unlinking() {
        let path = unique_socket_path("existing-socket");
        let listener = UnixListener::bind(&path).expect("fixture socket should bind");

        let err =
            VsockHostSocketOwner::bind(&path).expect_err("existing socket path should be rejected");

        assert_eq!(err, VsockHostSocketOwnerError::SocketPathExists);
        assert!(path.exists());

        drop(listener);
        fs::remove_file(path).expect("fixture socket should clean up");
    }

    #[test]
    fn host_socket_owner_rejects_broken_symlink_without_unlinking() {
        let path = unique_socket_path("existing-symlink");
        let target = unique_socket_path("missing-symlink-target");
        std::os::unix::fs::symlink(&target, &path).expect("fixture symlink should be created");

        let err = VsockHostSocketOwner::bind(&path)
            .expect_err("existing symlink path should be rejected");

        assert_eq!(err, VsockHostSocketOwnerError::SocketPathExists);
        assert!(
            fs::symlink_metadata(&path)
                .expect("symlink should remain")
                .file_type()
                .is_symlink()
        );

        fs::remove_file(path).expect("fixture symlink should clean up");
    }

    #[test]
    fn host_socket_owner_drop_keeps_replaced_path() {
        let path = unique_socket_path("replaced");
        let owner = VsockHostSocketOwner::bind(&path).expect("host socket should bind");

        fs::remove_file(&path).expect("owned socket path should be removable");
        fs::write(&path, "replacement").expect("replacement file should be written");

        drop(owner);

        assert_eq!(
            fs::read_to_string(&path).expect("replacement file should remain"),
            "replacement"
        );

        fs::remove_file(path).expect("replacement file should clean up");
    }

    #[test]
    fn host_socket_owner_allows_multiple_distinct_paths() {
        let first_path = unique_socket_path("multi-first");
        let second_path = unique_socket_path("multi-second");

        let first = VsockHostSocketOwner::bind(&first_path).expect("first socket should bind");
        let second = VsockHostSocketOwner::bind(&second_path).expect("second socket should bind");

        assert!(first_path.exists());
        assert!(second_path.exists());

        drop(first);

        assert!(!first_path.exists());
        assert!(second_path.exists());

        drop(second);

        assert!(!second_path.exists());
    }

    #[test]
    fn prepared_vsock_device_with_host_socket_owns_and_cleans_socket() {
        let path = unique_socket_path("prepared-owner");
        let config = valid_vsock_config(9, path.to_string_lossy());

        let prepared = PreparedVsockDevice::from_config_with_host_socket(&config)
            .expect("prepared vsock should bind host socket");

        assert!(path.exists());

        drop(prepared);

        assert!(!path.exists());
    }

    #[test]
    fn prepared_vsock_device_accepts_host_connection_through_device() {
        let path = unique_socket_path("prepared-accept");
        let config = valid_vsock_config(9, path.to_string_lossy());

        let prepared = PreparedVsockDevice::from_config_with_host_socket(&config)
            .expect("prepared vsock should bind host socket");
        let _client = UnixStream::connect(&path).expect("client should connect");

        let accepted = prepared
            .device()
            .accept_host_connection()
            .expect("pending connection should accept")
            .expect("accepted connection should be present");

        let mut buffer = [0; 1];
        let mut accepted_stream = accepted.stream();
        let err = accepted_stream
            .read(&mut buffer)
            .expect_err("idle accepted stream should not block");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn virtio_vsock_device_accept_without_host_socket_fails() {
        let err = VirtioVsockDevice::new()
            .accept_host_connection()
            .expect_err("missing host socket owner should fail");

        assert_eq!(err, VsockHostSocketAcceptError::HostSocketNotAttached);
    }

    #[test]
    fn virtio_vsock_host_request_poll_without_host_socket_is_noop() {
        let mut device = VirtioVsockDevice::with_guest_cid(42);

        let dispatch = device.poll_host_request_connections();

        assert_empty_host_request_dispatch(&dispatch);
        assert_eq!(device.pending_host_connection_count(), 0);
        assert!(device.host_connections.is_empty());
    }

    #[test]
    fn virtio_vsock_host_request_poll_retains_partial_connect_handshake() {
        let (mut device, path) = host_socket_device(42, "poll-partial");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        client
            .write_all(b"CONN")
            .expect("partial CONNECT should write");
        let first = device.poll_host_request_connections();

        assert_eq!(first.accepted_connections(), 1);
        assert_eq!(first.completed_requests(), 0);
        assert_eq!(first.dropped_connections(), 0);
        assert_eq!(first.pending_connections(), 1);
        assert_eq!(device.pending_host_connection_count(), 1);
        assert!(device.host_connections.is_empty());

        client
            .write_all(b"ECT 4000\n")
            .expect("remaining CONNECT should write");
        let second = device.poll_host_request_connections();

        assert_eq!(second.accepted_connections(), 0);
        assert_eq!(second.completed_requests(), 1);
        assert_eq!(second.dropped_connections(), 0);
        assert_eq!(second.pending_connections(), 0);
        assert_eq!(device.pending_host_connection_count(), 0);
        let key = device
            .host_connections
            .first_pending_request_packet_key()
            .expect("completed host request should be pending");
        assert_eq!(key.peer_port(), 4000);
    }

    #[test]
    fn virtio_vsock_host_request_poll_drops_malformed_connect_handshake() {
        let (mut device, path) = host_socket_device(42, "poll-bad");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        client
            .write_all(b"BAD 4000\n")
            .expect("malformed CONNECT should write");
        let dispatch = device.poll_host_request_connections();

        assert_eq!(dispatch.accepted_connections(), 1);
        assert_eq!(dispatch.completed_requests(), 0);
        assert_eq!(dispatch.dropped_connections(), 1);
        assert_eq!(dispatch.pending_connections(), 0);
        assert_eq!(device.pending_host_connection_count(), 0);
        assert!(device.host_connections.is_empty());
        assert_stream_closed(&mut client, "malformed host stream should be dropped");
    }

    #[test]
    fn virtio_vsock_host_request_poll_respects_pending_handshake_limit() {
        let (mut device, path) = host_socket_device(42, "poll-limit");
        device.set_host_connection_limit(1);
        let mut first_client = UnixStream::connect(&path).expect("first client should connect");
        let _second_client = UnixStream::connect(&path).expect("second client should connect");

        let first = device.poll_host_request_connections();

        assert_eq!(first.accepted_connections(), 1);
        assert_eq!(first.pending_connections(), 1);

        let capped = device.poll_host_request_connections();

        assert_eq!(capped.accepted_connections(), 0);
        assert_eq!(capped.completed_requests(), 0);
        assert_eq!(capped.dropped_connections(), 0);
        assert_eq!(capped.pending_connections(), 1);

        first_client
            .write_all(b"CONNECT 4000\n")
            .expect("first CONNECT should write");
        let completed = device.poll_host_request_connections();

        assert_eq!(completed.accepted_connections(), 0);
        assert_eq!(completed.completed_requests(), 1);
        assert_eq!(completed.pending_connections(), 0);
        let key = device
            .host_connections
            .first_pending_request_packet_key()
            .expect("completed host request should be pending");
        assert!(device.host_connections.remove(key));

        let next = device.poll_host_request_connections();

        assert_eq!(next.accepted_connections(), 1);
        assert_eq!(next.completed_requests(), 0);
        assert_eq!(next.pending_connections(), 1);
    }

    #[test]
    fn virtio_vsock_host_request_poll_drops_completed_handshake_when_connection_limit_is_full() {
        let (mut device, path) = host_socket_device(42, "poll-full");
        device.set_host_connection_limit(1);
        let mut pending_client = UnixStream::connect(&path).expect("pending client should connect");

        let pending = device.poll_host_request_connections();

        assert_eq!(pending.accepted_connections(), 1);
        assert_eq!(pending.completed_requests(), 0);
        assert_eq!(pending.pending_connections(), 1);

        let (active, _active_client, active_request) =
            accepted_host_connection_with_request("poll-full-active", 5000);
        device
            .insert_accepted_host_connection(active, active_request)
            .expect("test active host connection should insert");

        pending_client
            .write_all(b"CONNECT 4000\n")
            .expect("pending CONNECT should write");
        let dropped = device.poll_host_request_connections();

        assert_eq!(dropped.accepted_connections(), 0);
        assert_eq!(dropped.completed_requests(), 0);
        assert_eq!(dropped.dropped_connections(), 1);
        assert_eq!(dropped.pending_connections(), 0);
        assert_eq!(device.pending_host_connection_count(), 0);
        assert_eq!(device.host_connections.len(), 1);
        assert_stream_closed(
            &mut pending_client,
            "completed pending stream should close when connection limit is full",
        );
    }

    #[test]
    fn virtio_vsock_device_reset_drops_pending_host_handshakes() {
        let (mut device, path) = host_socket_device(42, "poll-reset");
        let _client = UnixStream::connect(&path).expect("client should connect");

        let dispatch = device.poll_host_request_connections();

        assert_eq!(dispatch.accepted_connections(), 1);
        assert_eq!(dispatch.pending_connections(), 1);
        assert_eq!(device.pending_host_connection_count(), 1);

        device.reset();

        assert_eq!(device.pending_host_connection_count(), 0);
        assert!(device.host_connections.is_empty());
        assert!(path.exists());
        drop(device);
        assert!(!path.exists());
    }

    #[test]
    fn prepared_vsock_device_host_socket_error_does_not_leak_path() {
        let path = unique_socket_path("secret-prepared-owner");
        fs::write(&path, "existing file").expect("fixture file should be written");
        let config = valid_vsock_config(10, path.to_string_lossy());

        let err = PreparedVsockDevice::from_config_with_host_socket(&config)
            .expect_err("existing host socket path should fail");

        assert!(matches!(
            err,
            super::PreparedVsockDeviceError::HostSocket {
                guest_cid: 10,
                source: VsockHostSocketOwnerError::SocketPathExists
            }
        ));
        assert!(err.source().is_some());
        assert!(!err.to_string().contains("secret-prepared-owner"));

        fs::remove_file(path).expect("fixture file should clean up");
    }

    #[test]
    fn prepared_vsock_device_preserves_config_and_inactive_device() {
        let config = valid_vsock_config(u32::MAX, "./relative-vsock.sock");
        let prepared = PreparedVsockDevice::from_config(&config);

        assert_eq!(prepared.guest_cid(), u32::MAX);
        assert_eq!(prepared.uds_path(), Path::new("./relative-vsock.sock"));
        assert_eq!(prepared.config_space().guest_cid(), u64::from(u32::MAX));
        assert!(!prepared.device().is_activated());
    }

    #[test]
    fn prepared_vsock_device_into_parts_consumes_owned_resource() {
        let config = valid_vsock_config(7, "relative-vsock.sock");
        let prepared = PreparedVsockDevice::from_config(&config);

        let (guest_cid, uds_path, config_space, device) = prepared.into_parts();

        assert_eq!(guest_cid, 7);
        assert_eq!(uds_path.as_path(), Path::new("relative-vsock.sock"));
        assert_eq!(config_space.guest_cid(), 7);
        assert!(!device.is_activated());
    }

    #[test]
    fn prepared_vsock_device_does_not_touch_missing_socket_path() {
        let socket_path = unique_missing_socket_path();
        let path = Path::new(&socket_path);
        let config = valid_vsock_config(8, socket_path.clone());

        assert!(!path.exists());

        let prepared = PreparedVsockDevice::from_config(&config);

        assert_eq!(prepared.uds_path(), path);
        assert!(!path.exists());
    }

    #[test]
    fn prepared_vsock_device_registers_mmio_in_fresh_dispatcher() {
        let mut device = prepared_vsock_device(42, "./v.sock")
            .register_mmio(vsock_mmio_layout())
            .expect("vsock device should register");
        let registration = device.registration();

        assert_eq!(registration.guest_cid(), 42);
        assert_eq!(registration.uds_path(), Path::new("./v.sock"));
        assert_eq!(registration.region_id(), MmioRegionId::new(2));
        assert_eq!(registration.address(), GuestAddress::new(TEST_MMIO_BASE));
        assert_eq!(
            registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(device.dispatcher().regions(), &[registration.region()]);

        let region_id = registration.region_id();
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioVsockMmioHandler>(region_id)
            .expect("registered vsock handler should be present");
        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
    }

    #[test]
    fn prepared_vsock_device_registers_mmio_in_existing_dispatcher() {
        let mut dispatcher = MmioDispatcher::new();
        let existing_region = dispatcher
            .insert_region(
                MmioRegionId::new(1),
                GuestAddress::new(TEST_MMIO_BASE - VIRTIO_MMIO_DEVICE_WINDOW_SIZE),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("existing region should insert");
        let device = prepared_vsock_device(9, "./v.sock")
            .register_mmio_with_dispatcher(vsock_mmio_layout(), dispatcher)
            .expect("vsock device should register with existing dispatcher");

        assert_eq!(device.dispatcher().regions().len(), 2);
        assert!(device.dispatcher().regions().contains(&existing_region));
        assert!(
            device
                .dispatcher()
                .regions()
                .contains(&device.registration().region())
        );
    }

    #[test]
    fn registered_vsock_mmio_dispatch_reads_guest_cid_config() {
        let mut device = prepared_vsock_device(0x1122_3344, "./v.sock")
            .register_mmio(vsock_mmio_layout())
            .expect("vsock device should register");

        assert_eq!(
            read_registered_config(&mut device, 0, 8),
            u64::from(0x1122_3344_u32).to_le_bytes().to_vec()
        );
        assert_eq!(
            read_registered_config(&mut device, 0, 4),
            0x1122_3344_u32.to_le_bytes().to_vec()
        );
        assert_eq!(
            read_registered_config(&mut device, 4, 4),
            0_u32.to_le_bytes().to_vec()
        );
    }

    #[test]
    fn registered_vsock_mmio_into_parts_consumes_owned_resource() {
        let device = prepared_vsock_device(11, "relative-vsock.sock")
            .register_mmio(vsock_mmio_layout())
            .expect("vsock device should register");

        let (dispatcher, registration) = device.into_parts();

        assert_eq!(dispatcher.regions(), &[registration.region()]);
        assert_eq!(registration.guest_cid(), 11);
        assert_eq!(registration.uds_path(), Path::new("relative-vsock.sock"));
    }

    #[test]
    fn prepared_vsock_device_rejects_overlapping_mmio_registration_without_path_leak() {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(1),
                GuestAddress::new(TEST_MMIO_BASE),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("existing region should insert");

        let layout = VsockMmioLayout::new(
            GuestAddress::new(TEST_MMIO_BASE + 0x100),
            MmioRegionId::new(3),
        );
        let err = prepared_vsock_device(12, "secret-vsock-path.sock")
            .register_mmio_with_dispatcher(layout, dispatcher)
            .expect_err("overlapping region should fail");

        assert!(matches!(
            err,
            VsockMmioRegistrationError::InsertRegion {
                guest_cid: 12,
                region_id,
                ..
            } if region_id == MmioRegionId::new(3)
        ));
        assert!(err.source().is_some());
        assert!(!err.to_string().contains("secret-vsock-path"));
    }

    #[test]
    fn prepared_vsock_device_rejects_invalid_mmio_layout_without_path_leak() {
        let layout = VsockMmioLayout::new(GuestAddress::new(u64::MAX), MmioRegionId::new(4));
        let err = prepared_vsock_device(13, "secret-vsock-path.sock")
            .register_mmio(layout)
            .expect_err("overflowing region should fail");

        assert!(matches!(
            err,
            VsockMmioRegistrationError::InvalidRegion {
                region_id,
                address,
                ..
            } if region_id == MmioRegionId::new(4) && address == GuestAddress::new(u64::MAX)
        ));
        assert!(err.source().is_some());
        assert!(!err.to_string().contains("secret-vsock-path"));
    }

    #[test]
    fn prepared_vsock_device_rejects_duplicate_mmio_handler_without_path_leak() {
        let layout = vsock_mmio_layout();
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .register_handler(
                layout.region_id(),
                virtio_vsock_mmio_handler(3).expect("existing handler should build"),
            )
            .expect("existing handler should register");

        let err = prepared_vsock_device(14, "secret-vsock-path.sock")
            .register_mmio_with_dispatcher(layout, dispatcher)
            .expect_err("duplicate handler should fail");

        assert!(matches!(
            err,
            VsockMmioRegistrationError::RegisterHandler {
                guest_cid: 14,
                region_id,
                ..
            } if region_id == layout.region_id()
        ));
        assert!(err.source().is_some());
        assert!(!err.to_string().contains("secret-vsock-path"));
    }

    #[test]
    fn prepared_vsock_device_mmio_registration_does_not_touch_missing_socket_path() {
        let socket_path = unique_missing_socket_path();
        let path = Path::new(&socket_path);

        assert!(!path.exists());

        let device = prepared_vsock_device(13, socket_path.clone())
            .register_mmio(vsock_mmio_layout())
            .expect("vsock device should register");

        assert_eq!(device.registration().uds_path(), path);
        assert!(!path.exists());
    }

    #[test]
    fn virtio_vsock_constants_match_firecracker_shape() {
        assert_eq!(VIRTIO_VSOCK_DEVICE_ID, 19);
        assert_eq!(VIRTIO_VSOCK_RX_QUEUE_INDEX, 0);
        assert_eq!(VIRTIO_VSOCK_TX_QUEUE_INDEX, 1);
        assert_eq!(VIRTIO_VSOCK_EVENT_QUEUE_INDEX, 2);
        assert_eq!(VIRTIO_VSOCK_QUEUE_COUNT, 3);
        assert_eq!(VIRTIO_VSOCK_QUEUE_SIZE, 256);
        assert_eq!(VIRTIO_VSOCK_QUEUE_SIZES, [256, 256, 256]);
        assert_eq!(VIRTIO_VSOCK_CONFIG_GUEST_CID_SIZE, 8);
    }

    #[test]
    fn virtio_vsock_packet_constants_match_firecracker_shape() {
        assert_eq!(VIRTIO_VSOCK_PACKET_HEADER_SIZE, 44);
        assert_eq!(VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE, 64 * 1024);
        assert_eq!(VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE, 64 * 1024);
        assert_eq!(VIRTIO_VSOCK_HOST_CID, 2);
        assert_eq!(VIRTIO_VSOCK_PACKET_TYPE_STREAM, 1);
        assert_eq!(VIRTIO_VSOCK_OP_REQUEST, 1);
        assert_eq!(VIRTIO_VSOCK_OP_RESPONSE, 2);
        assert_eq!(VIRTIO_VSOCK_OP_RST, 3);
        assert_eq!(VIRTIO_VSOCK_OP_SHUTDOWN, 4);
        assert_eq!(VIRTIO_VSOCK_OP_RW, 5);
        assert_eq!(VIRTIO_VSOCK_OP_CREDIT_UPDATE, 6);
        assert_eq!(VIRTIO_VSOCK_OP_CREDIT_REQUEST, 7);
        assert_eq!(VIRTIO_VSOCK_FLAGS_SHUTDOWN_RCV, 1);
        assert_eq!(VIRTIO_VSOCK_FLAGS_SHUTDOWN_SEND, 2);
    }

    #[test]
    fn virtio_vsock_packet_header_serializes_little_endian_layout() {
        let header = test_vsock_packet_header();

        assert_eq!(
            header.to_bytes(),
            [
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13,
                0x12, 0x11, 0x24, 0x23, 0x22, 0x21, 0x34, 0x33, 0x32, 0x31, 0x00, 0x10, 0x00, 0x00,
                0x01, 0x00, 0x05, 0x00, 0x03, 0x00, 0x00, 0x00, 0x44, 0x43, 0x42, 0x41, 0x54, 0x53,
                0x52, 0x51,
            ]
        );
    }

    #[test]
    fn virtio_vsock_packet_header_parses_little_endian_layout() {
        let header = VirtioVsockPacketHeader::try_from_bytes([
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13,
            0x12, 0x11, 0x24, 0x23, 0x22, 0x21, 0x34, 0x33, 0x32, 0x31, 0x00, 0x10, 0x00, 0x00,
            0x01, 0x00, 0x05, 0x00, 0x03, 0x00, 0x00, 0x00, 0x44, 0x43, 0x42, 0x41, 0x54, 0x53,
            0x52, 0x51,
        ])
        .expect("valid header bytes should parse");

        assert_eq!(header, test_vsock_packet_header());
        assert_eq!(header.src_cid(), 0x0102_0304_0506_0708);
        assert_eq!(header.dst_cid(), 0x1112_1314_1516_1718);
        assert_eq!(header.src_port(), 0x2122_2324);
        assert_eq!(header.dst_port(), 0x3132_3334);
        assert_eq!(header.payload_len(), 0x1000);
        assert_eq!(header.packet_type(), VIRTIO_VSOCK_PACKET_TYPE_STREAM);
        assert_eq!(header.operation(), VIRTIO_VSOCK_OP_RW);
        assert_eq!(
            header.flags(),
            VIRTIO_VSOCK_FLAGS_SHUTDOWN_RCV | VIRTIO_VSOCK_FLAGS_SHUTDOWN_SEND
        );
        assert_eq!(header.buffer_allocation(), 0x4142_4344);
        assert_eq!(header.forwarded_count(), 0x5152_5354);
    }

    #[test]
    fn virtio_vsock_packet_header_accepts_zero_and_max_payload_len() {
        VirtioVsockPacketHeader::new()
            .with_payload_len(0)
            .validate_payload_len()
            .expect("zero payload length should be accepted");
        VirtioVsockPacketHeader::new()
            .with_payload_len(VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE)
            .validate_payload_len()
            .expect("maximum payload length should be accepted");
    }

    #[test]
    fn virtio_vsock_packet_header_rejects_payload_len_over_maximum() {
        let err = VirtioVsockPacketHeader::new()
            .with_payload_len(VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE + 1)
            .validate_payload_len()
            .expect_err("payload length above maximum should fail");

        assert_eq!(
            err,
            VirtioVsockPacketLengthError {
                payload_len: VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE + 1,
                max_payload_len: VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE,
            }
        );
        assert_eq!(
            err.to_string(),
            "virtio-vsock packet payload length 65537 exceeds maximum 65536"
        );
    }

    #[test]
    fn virtio_vsock_packet_header_rejects_payload_len_over_maximum_from_bytes() {
        let err = VirtioVsockPacketHeader::try_from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ])
        .expect_err("oversized payload length should fail");

        assert_eq!(err.payload_len(), VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE + 1);
        assert_eq!(err.max_payload_len(), VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE);
    }

    #[test]
    fn virtio_vsock_guest_reset_header_swaps_guest_tx_addressing() {
        let packet = guest_request_tx_packet(42, 52, 4000);

        let header = super::guest_reset_packet_header_for_tx_packet(&packet, 42)
            .expect("host-destined guest request should produce RST header");

        assert_guest_reset_packet_header(header, 42, 52, 4000);
    }

    #[test]
    fn virtio_vsock_guest_reset_header_ignores_wrong_cid_and_reset_packets() {
        assert!(
            super::guest_reset_packet_header_for_tx_packet(
                &guest_request_tx_packet(43, 52, 4000),
                42
            )
            .is_none()
        );

        let wrong_destination = guest_request_tx_packet(42, 52, 4000)
            .header()
            .with_dst_cid(VIRTIO_VSOCK_HOST_CID + 1);
        assert!(
            super::guest_reset_packet_header_for_tx_packet(
                &guest_tx_packet_with_header(wrong_destination),
                42,
            )
            .is_none()
        );

        let guest_reset = guest_request_tx_packet(42, 52, 4000)
            .header()
            .with_operation(VIRTIO_VSOCK_OP_RST);
        assert!(
            super::guest_reset_packet_header_for_tx_packet(
                &guest_tx_packet_with_header(guest_reset),
                42,
            )
            .is_none()
        );
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatches_host_request_packet() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue();
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) =
            insert_accepted_host_connection_for_test(&mut table, "rx-request", 4000);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let dispatch = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect("pending host request should dispatch");

        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_requests(), 1);
        assert_eq!(dispatch.delivered_reset_packets(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 0);
        assert_eq!(dispatch.buffer_too_small_failures(), 0);
        assert!(dispatch.first_buffer_parse_failure().is_none());
        assert!(dispatch.first_buffer_too_small().is_none());
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(dispatch.deliveries().len(), 1);
        assert_eq!(
            dispatch.deliveries()[0].packet_kind(),
            VirtioVsockRxPacketKind::HostRequest
        );
        assert_eq!(dispatch.deliveries()[0].descriptor_head(), 0);
        assert_eq!(
            dispatch.deliveries()[0].bytes_written_to_guest(),
            VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32
        );
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 0),
            (0, VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32)
        );
        assert_host_connection_request_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            key.local_port(),
            4000,
        );
        assert!(!table.has_pending_request_packet(key));

        let second_dispatch = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect("consumed host request should be a no-op");
        assert_eq!(second_dispatch.processed_buffers(), 0);
        assert!(!second_dispatch.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatch_without_pending_request_is_noop() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue();
        let mut table = VsockHostConnectionTable::new();
        let key = VsockHostConnectionKey::new(
            VsockHostLocalPort::try_from_raw(VSOCK_HOST_LOCAL_PORT_BASE)
                .expect("test local port should be valid"),
            4000,
        );
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let dispatch = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect("missing host request should be a no-op");

        assert_eq!(dispatch.processed_buffers(), 0);
        assert_eq!(dispatch.delivered_requests(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 0);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatch_without_available_descriptor_keeps_request_pending() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue();
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) =
            insert_accepted_host_connection_for_test(&mut table, "rx-no-descriptor", 4001);

        let dispatch = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect("empty RX queue should dispatch as no work");

        assert_eq!(dispatch.processed_buffers(), 0);
        assert_eq!(dispatch.delivered_requests(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert!(table.has_pending_request_packet(key));
        assert_eq!(queue.available_ring().next_avail(), 0);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatch_rejects_read_only_buffer_without_consuming_request() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue();
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) =
            insert_accepted_host_connection_for_test(&mut table, "rx-read-only", 4002);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let dispatch = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect("read-only RX descriptor should be recorded");

        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_requests(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 1);
        assert!(matches!(
            dispatch.first_buffer_parse_failure(),
            Some(VirtioVsockRxBufferParseError::BufferDescriptorReadOnly { index: 0 })
        ));
        assert!(dispatch.needs_queue_interrupt());
        assert!(table.has_pending_request_packet(key));
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(read_vsock_rx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatch_rejects_empty_buffer_without_consuming_request() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue();
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) = insert_accepted_host_connection_for_test(&mut table, "rx-empty", 4003);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(TEST_VSOCK_RX_BUFFER, 0, None),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let dispatch = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect("empty RX descriptor should be recorded");

        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_requests(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 1);
        assert!(matches!(
            dispatch.first_buffer_parse_failure(),
            Some(VirtioVsockRxBufferParseError::BufferDescriptorEmpty { index: 0 })
        ));
        assert!(dispatch.needs_queue_interrupt());
        assert!(table.has_pending_request_packet(key));
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(read_vsock_rx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatch_rejects_small_buffer_without_consuming_request() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue();
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) = insert_accepted_host_connection_for_test(&mut table, "rx-small", 4004);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 - 1,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let dispatch = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect("small RX buffer should be recorded");

        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_requests(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 0);
        assert_eq!(dispatch.buffer_too_small_failures(), 1);
        let too_small = dispatch
            .first_buffer_too_small()
            .expect("too-small metadata should exist");
        assert_eq!(too_small.descriptor_head(), 0);
        assert_eq!(
            too_small.buffer_len(),
            VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64 - 1
        );
        assert_eq!(
            too_small.required_len(),
            VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64
        );
        assert!(dispatch.needs_queue_interrupt());
        assert!(table.has_pending_request_packet(key));
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(read_vsock_rx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatch_rejects_unmapped_buffer_without_consuming_request() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue();
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) =
            insert_accepted_host_connection_for_test(&mut table, "rx-unmapped", 4005);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_UNMAPPED_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let dispatch = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect("unmapped RX buffer should be recorded");

        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_requests(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 1);
        assert!(matches!(
            dispatch.first_buffer_parse_failure(),
            Some(VirtioVsockRxBufferParseError::BufferDescriptorAccess { index: 0, .. })
        ));
        assert!(dispatch.needs_queue_interrupt());
        assert!(table.has_pending_request_packet(key));
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(read_vsock_rx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatch_preserves_request_on_available_ring_failure() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue();
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) =
            insert_accepted_host_connection_for_test(&mut table, "rx-available", 4006);
        write_vsock_rx_available_heads(&mut memory, &[TEST_VSOCK_QUEUE_SIZE]);

        let error = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect_err("invalid RX available head should fail");

        assert!(matches!(
            error,
            VirtioVsockRxQueueDispatchError::AvailableRing { .. }
        ));
        let completed = error
            .completed_dispatch()
            .expect("completed dispatch metadata should be preserved");
        assert_eq!(completed.processed_buffers(), 0);
        assert_eq!(completed.delivered_requests(), 0);
        assert_eq!(completed.buffer_parse_failures(), 0);
        assert_eq!(completed.buffer_too_small_failures(), 0);
        assert!(!completed.needs_queue_interrupt());
        assert!(table.has_pending_request_packet(key));
        assert_eq!(queue.available_ring().next_avail(), 0);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatch_writes_split_host_request_header() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue();
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) = insert_accepted_host_connection_for_test(&mut table, "rx-split", 4006);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(TEST_VSOCK_RX_BUFFER, 8, Some(1)),
        );
        write_vsock_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                TEST_VSOCK_RX_SECOND_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 - 8,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let dispatch = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect("split RX buffer should dispatch");

        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_requests(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 0),
            (0, VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32)
        );
        let mut header_bytes = read_guest_bytes(&memory, TEST_VSOCK_RX_BUFFER, 8);
        header_bytes.extend(read_guest_bytes(
            &memory,
            TEST_VSOCK_RX_SECOND_BUFFER,
            VIRTIO_VSOCK_PACKET_HEADER_SIZE - 8,
        ));
        let header = VirtioVsockPacketHeader::try_from_bytes(
            header_bytes
                .try_into()
                .expect("split header bytes should be complete"),
        )
        .expect("split header should parse");
        assert_host_connection_request_header(header, 42, key.local_port(), 4006);
        assert!(!table.has_pending_request_packet(key));
    }

    #[test]
    fn virtio_vsock_rx_queue_dispatch_preserves_request_on_used_ring_failure() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_rx_queue_with_used_ring(TEST_VSOCK_RX_UNMAPPED_USED_RING);
        let mut table = VsockHostConnectionTable::new();
        let (key, _client) =
            insert_accepted_host_connection_for_test(&mut table, "rx-used-ring", 4007);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let error = queue
            .dispatch_host_request(&mut memory, &mut table, key, 42)
            .expect_err("unmapped RX used ring should fail");

        assert!(matches!(
            error,
            VirtioVsockRxQueueDispatchError::UsedRing {
                descriptor_head: 0,
                bytes_written_to_guest,
                ..
            } if bytes_written_to_guest == VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32
        ));
        let completed = error
            .completed_dispatch()
            .expect("completed dispatch metadata should be preserved");
        assert_eq!(completed.processed_buffers(), 0);
        assert_eq!(completed.delivered_requests(), 0);
        assert_eq!(completed.buffer_parse_failures(), 0);
        assert_eq!(completed.buffer_too_small_failures(), 0);
        assert!(!completed.needs_queue_interrupt());
        assert!(table.has_pending_request_packet(key));
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_host_connection_request_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            key.local_port(),
            4007,
        );
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_accepts_single_descriptor_packet() {
        let mut memory = vsock_tx_memory();
        let header = test_vsock_packet_header().with_payload_len(4);
        let payload_address = vsock_payload_address_after_header(TEST_VSOCK_HEADER);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_guest_bytes(&mut memory, payload_address, &[0xde, 0xad, 0xbe, 0xef]);
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + 4,
                None,
            )],
        );

        let packet =
            parse_vsock_tx_packet(&memory, &chain).expect("single-descriptor packet should parse");

        assert_eq!(packet.descriptor_head(), 0);
        assert_eq!(packet.header(), header);
        assert_eq!(packet.payload_len(), 4);
        assert_eq!(packet.packet_len(), VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64 + 4);
        assert_eq!(packet.payload_segments().len(), 1);
        let segment = packet
            .payload_segments()
            .first()
            .expect("payload segment should be present");
        assert_eq!(segment.descriptor_index(), 0);
        assert_eq!(segment.address(), payload_address);
        assert_eq!(segment.len(), 4);
        assert!(!segment.is_empty());
        assert_eq!(
            read_guest_bytes(
                &memory,
                segment.address(),
                usize::try_from(segment.len()).expect("segment length should fit")
            ),
            [0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_accepts_split_header_packet() {
        let mut memory = vsock_tx_memory();
        let header = test_vsock_packet_header().with_payload_len(3);
        let header_bytes = header.to_bytes();
        let (first_header, second_header_and_payload) = header_bytes.split_at(16);
        let payload_address = TEST_VSOCK_SECOND_HEADER
            .checked_add(
                u64::try_from(second_header_and_payload.len())
                    .expect("second header length should fit"),
            )
            .expect("payload address should not overflow");
        write_guest_bytes(&mut memory, TEST_VSOCK_HEADER, first_header);
        write_guest_bytes(
            &mut memory,
            TEST_VSOCK_SECOND_HEADER,
            second_header_and_payload,
        );
        write_guest_bytes(&mut memory, payload_address, &[0xaa, 0xbb, 0xcc]);
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_VSOCK_HEADER, 16, Some(1)),
                TestDescriptor::readable(
                    TEST_VSOCK_SECOND_HEADER,
                    u32::try_from(second_header_and_payload.len() + 3)
                        .expect("descriptor length should fit"),
                    None,
                ),
            ],
        );

        let packet =
            parse_vsock_tx_packet(&memory, &chain).expect("split-header packet should parse");

        assert_eq!(packet.header(), header);
        assert_eq!(packet.payload_len(), 3);
        assert_eq!(packet.payload_segments().len(), 1);
        let segment = packet
            .payload_segments()
            .first()
            .expect("payload segment should be present");
        assert_eq!(segment.descriptor_index(), 1);
        assert_eq!(segment.address(), payload_address);
        assert_eq!(segment.len(), 3);
        assert_eq!(
            read_guest_bytes(
                &memory,
                segment.address(),
                usize::try_from(segment.len()).expect("segment length should fit")
            ),
            [0xaa, 0xbb, 0xcc]
        );
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_accepts_split_payload_packet() {
        let mut memory = vsock_tx_memory();
        let header = test_vsock_packet_header().with_payload_len(5);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_guest_bytes(&mut memory, TEST_VSOCK_PAYLOAD, &[0x10, 0x11]);
        write_guest_bytes(
            &mut memory,
            TEST_VSOCK_SECOND_PAYLOAD,
            &[0x12, 0x13, 0x14, 0xff],
        );
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_VSOCK_HEADER,
                    VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                    Some(1),
                ),
                TestDescriptor::readable(TEST_VSOCK_PAYLOAD, 2, Some(2)),
                TestDescriptor::readable(TEST_VSOCK_SECOND_PAYLOAD, 4, None),
            ],
        );

        let packet =
            parse_vsock_tx_packet(&memory, &chain).expect("split-payload packet should parse");

        assert_eq!(packet.header(), header);
        assert_eq!(packet.payload_len(), 5);
        assert_eq!(packet.payload_segments().len(), 2);
        let first = packet
            .payload_segments()
            .first()
            .expect("first payload segment should be present");
        assert_eq!(first.descriptor_index(), 1);
        assert_eq!(first.address(), TEST_VSOCK_PAYLOAD);
        assert_eq!(first.len(), 2);
        assert_eq!(
            read_guest_bytes(
                &memory,
                first.address(),
                usize::try_from(first.len()).expect("segment length should fit")
            ),
            [0x10, 0x11]
        );
        let second = packet
            .payload_segments()
            .get(1)
            .expect("second payload segment should be present");
        assert_eq!(second.descriptor_index(), 2);
        assert_eq!(second.address(), TEST_VSOCK_SECOND_PAYLOAD);
        assert_eq!(second.len(), 3);
        assert_eq!(
            read_guest_bytes(
                &memory,
                second.address(),
                usize::try_from(second.len()).expect("segment length should fit")
            ),
            [0x12, 0x13, 0x14]
        );
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_accepts_max_payload_len_packet() {
        let mut memory = vsock_tx_memory();
        let header =
            test_vsock_packet_header().with_payload_len(VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE);
        let payload_address = vsock_payload_address_after_header(TEST_VSOCK_HEADER);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE,
                None,
            )],
        );

        let packet =
            parse_vsock_tx_packet(&memory, &chain).expect("max-payload packet should parse");

        assert_eq!(packet.header(), header);
        assert_eq!(packet.payload_len(), VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE);
        assert_eq!(
            packet.packet_len(),
            VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64 + u64::from(VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE)
        );
        assert_eq!(packet.payload_segments().len(), 1);
        let segment = packet
            .payload_segments()
            .first()
            .expect("payload segment should be present");
        assert_eq!(segment.descriptor_index(), 0);
        assert_eq!(segment.address(), payload_address);
        assert_eq!(segment.len(), VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE);
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_accepts_zero_payload_header_only() {
        let mut memory = vsock_tx_memory();
        let header = test_vsock_packet_header().with_payload_len(0);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );

        let packet =
            parse_vsock_tx_packet(&memory, &chain).expect("zero-payload packet should parse");

        assert_eq!(packet.header(), header);
        assert_eq!(packet.payload_len(), 0);
        assert_eq!(packet.packet_len(), VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64);
        assert!(packet.payload_segments().is_empty());
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_rejects_write_only_descriptor() {
        let mut memory = vsock_tx_memory();
        let header = test_vsock_packet_header().with_payload_len(0);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );

        let err =
            parse_vsock_tx_packet(&memory, &chain).expect_err("write-only descriptor should fail");

        assert!(matches!(
            err,
            VirtioVsockTxPacketParseError::DescriptorWriteOnly { index: 0 }
        ));
        assert_eq!(
            err.to_string(),
            "virtio-vsock TX descriptor 0 is write-only"
        );
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_rejects_chain_shorter_than_header() {
        let mut memory = vsock_tx_memory();
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 - 1,
                None,
            )],
        );

        let err = parse_vsock_tx_packet(&memory, &chain).expect_err("short header should fail");

        assert!(matches!(
            err,
            VirtioVsockTxPacketParseError::HeaderTooShort {
                descriptor_head: 0,
                actual: 43,
                min: VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64,
            }
        ));
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_rejects_header_payload_len_over_maximum() {
        let mut memory = vsock_tx_memory();
        let header =
            test_vsock_packet_header().with_payload_len(VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE + 1);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );

        let err =
            parse_vsock_tx_packet(&memory, &chain).expect_err("oversized payload should fail");

        let VirtioVsockTxPacketParseError::InvalidHeaderLength { source } = err else {
            panic!("expected invalid header length, got {err:?}");
        };
        assert_eq!(
            source.payload_len(),
            VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE + 1
        );
        assert_eq!(
            source.max_payload_len(),
            VIRTIO_VSOCK_MAX_PACKET_BUFFER_SIZE
        );
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_rejects_payload_len_beyond_descriptor_bytes() {
        let mut memory = vsock_tx_memory();
        let header = test_vsock_packet_header().with_payload_len(8);
        let payload_address = vsock_payload_address_after_header(TEST_VSOCK_HEADER);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_guest_bytes(&mut memory, payload_address, &[0xde, 0xad, 0xbe, 0xef]);
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + 4,
                None,
            )],
        );

        let err = parse_vsock_tx_packet(&memory, &chain).expect_err("short payload should fail");

        assert!(matches!(
            err,
            VirtioVsockTxPacketParseError::PayloadTooShort {
                descriptor_head: 0,
                required: 8,
                available: 4,
            }
        ));
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_rejects_unmapped_header_descriptor() {
        let mut memory = vsock_tx_memory();
        let unmapped = GuestAddress::new(TEST_VSOCK_TX_MEMORY_SIZE + 0x1000);
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                unmapped,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );

        let err = parse_vsock_tx_packet(&memory, &chain).expect_err("unmapped header should fail");

        match err {
            VirtioVsockTxPacketParseError::DescriptorAccess {
                index,
                address,
                len,
                ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(address, unmapped);
                assert_eq!(len, VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32);
            }
            other => panic!("expected descriptor access error, got {other:?}"),
        }
    }

    #[test]
    fn virtio_vsock_tx_packet_parser_rejects_unmapped_payload_descriptor() {
        let mut memory = vsock_tx_memory();
        let header = test_vsock_packet_header().with_payload_len(4);
        let unmapped = GuestAddress::new(TEST_VSOCK_TX_MEMORY_SIZE + 0x1000);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        let chain = vsock_tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_VSOCK_HEADER,
                    VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                    Some(1),
                ),
                TestDescriptor::readable(unmapped, 4, None),
            ],
        );

        let err = parse_vsock_tx_packet(&memory, &chain).expect_err("unmapped payload should fail");

        match err {
            VirtioVsockTxPacketParseError::DescriptorAccess {
                index,
                address,
                len,
                ..
            } => {
                assert_eq!(index, 1);
                assert_eq!(address, unmapped);
                assert_eq!(len, 4);
            }
            other => panic!("expected descriptor access error, got {other:?}"),
        }
    }

    #[test]
    fn virtio_vsock_tx_queue_dispatch_empty_available_ring_is_noop() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_tx_queue();

        let dispatch = queue
            .dispatch(&mut memory)
            .expect("empty TX queue should dispatch");

        assert_eq!(dispatch.processed_packets(), 0);
        assert_eq!(dispatch.successful_packets(), 0);
        assert_eq!(dispatch.parse_failures(), 0);
        assert!(dispatch.packets().is_empty());
        assert!(dispatch.first_parse_failure().is_none());
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 0);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_eq!(read_vsock_tx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_vsock_tx_queue_dispatch_parses_single_packet() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_tx_queue();
        let header = test_vsock_packet_header().with_payload_len(4);
        let payload_address = vsock_payload_address_after_header(TEST_VSOCK_HEADER);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_guest_bytes(&mut memory, payload_address, &[0xa0, 0xa1, 0xa2, 0xa3]);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + 4,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);

        let dispatch = queue
            .dispatch(&mut memory)
            .expect("single TX packet should dispatch");

        assert_eq!(dispatch.processed_packets(), 1);
        assert_eq!(dispatch.successful_packets(), 1);
        assert_eq!(dispatch.parse_failures(), 0);
        assert!(dispatch.first_parse_failure().is_none());
        assert!(dispatch.needs_queue_interrupt());
        let packet = dispatch
            .packets()
            .first()
            .expect("parsed packet should be recorded");
        assert_eq!(packet.descriptor_head(), 0);
        assert_eq!(packet.header(), header);
        assert_eq!(packet.payload_len(), 4);
        assert_eq!(packet.payload_segments().len(), 1);
        assert_eq!(
            packet
                .payload_segments()
                .first()
                .expect("payload segment should exist")
                .address(),
            payload_address
        );
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        assert_eq!(read_vsock_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_tx_queue_dispatch_preserves_next_avail_across_calls() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_tx_queue();
        let first_header = test_vsock_packet_header().with_payload_len(1);
        let second_header = test_vsock_packet_header().with_payload_len(2);
        let third_header = test_vsock_packet_header().with_payload_len(3);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, first_header);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_SECOND_HEADER, second_header);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_PAYLOAD, third_header);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + 1,
                None,
            ),
        );
        write_vsock_tx_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_VSOCK_SECOND_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + 2,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0, 1]);

        let first_dispatch = queue
            .dispatch(&mut memory)
            .expect("first TX dispatch should drain two packets");

        assert_eq!(first_dispatch.processed_packets(), 2);
        assert_eq!(first_dispatch.successful_packets(), 2);
        assert!(first_dispatch.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 2);
        assert_eq!(queue.used_ring().next_used(), 2);
        assert_eq!(first_dispatch.packets()[0].descriptor_head(), 0);
        assert_eq!(first_dispatch.packets()[1].descriptor_head(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 2);
        assert_eq!(read_vsock_tx_used_element(&memory, 0), (0, 0));
        assert_eq!(read_vsock_tx_used_element(&memory, 1), (1, 0));

        write_vsock_tx_descriptor(
            &mut memory,
            2,
            TestDescriptor::readable(
                TEST_VSOCK_PAYLOAD,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + 3,
                None,
            ),
        );
        write_vsock_tx_available_entry(&mut memory, 2, 2);
        write_vsock_tx_available_index(&mut memory, 3);

        let second_dispatch = queue
            .dispatch(&mut memory)
            .expect("second TX dispatch should drain only the new packet");

        assert_eq!(second_dispatch.processed_packets(), 1);
        assert_eq!(second_dispatch.successful_packets(), 1);
        assert!(second_dispatch.needs_queue_interrupt());
        assert_eq!(second_dispatch.packets()[0].descriptor_head(), 2);
        assert_eq!(queue.available_ring().next_avail(), 3);
        assert_eq!(queue.used_ring().next_used(), 3);
        assert_eq!(read_vsock_tx_used_index(&memory), 3);
        assert_eq!(read_vsock_tx_used_element(&memory, 2), (2, 0));
    }

    #[test]
    fn virtio_vsock_tx_queue_dispatch_records_parse_failure() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_tx_queue();
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 - 1,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);

        let dispatch = queue
            .dispatch(&mut memory)
            .expect("malformed TX packet should be recorded");

        assert_eq!(dispatch.processed_packets(), 1);
        assert_eq!(dispatch.successful_packets(), 0);
        assert_eq!(dispatch.parse_failures(), 1);
        assert!(dispatch.packets().is_empty());
        assert!(matches!(
            dispatch.first_parse_failure(),
            Some(VirtioVsockTxPacketParseError::HeaderTooShort {
                descriptor_head: 0,
                actual: 43,
                min: VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64,
            })
        ));
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        assert_eq!(read_vsock_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_tx_queue_dispatch_preserves_partial_available_ring_failure() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_tx_queue();
        let header = test_vsock_packet_header().with_payload_len(0);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0, TEST_VSOCK_QUEUE_SIZE]);

        let error = queue
            .dispatch(&mut memory)
            .expect_err("invalid second TX head should fail after partial dispatch");

        assert!(matches!(
            error,
            VirtioVsockTxQueueDispatchError::AvailableRing { .. }
        ));
        let completed = error
            .completed_dispatch()
            .expect("partial dispatch metadata should be preserved");
        assert_eq!(completed.processed_packets(), 1);
        assert_eq!(completed.successful_packets(), 1);
        assert_eq!(completed.parse_failures(), 0);
        assert_eq!(completed.packets().len(), 1);
        assert_eq!(completed.packets()[0].descriptor_head(), 0);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        assert_eq!(read_vsock_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_tx_queue_dispatch_preserves_used_ring_failure() {
        let mut memory = vsock_tx_memory();
        let mut queue = vsock_tx_queue_with_used_ring(TEST_VSOCK_TX_UNMAPPED_USED_RING);
        let header = test_vsock_packet_header().with_payload_len(0);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);

        let error = queue
            .dispatch(&mut memory)
            .expect_err("unmapped TX used ring should fail");

        assert!(matches!(
            error,
            VirtioVsockTxQueueDispatchError::UsedRing {
                descriptor_head: 0,
                bytes_written_to_guest: 0,
                ..
            }
        ));
        let completed = error
            .completed_dispatch()
            .expect("completed dispatch metadata should be preserved");
        assert_eq!(completed.processed_packets(), 0);
        assert_eq!(completed.successful_packets(), 0);
        assert_eq!(completed.parse_failures(), 0);
        assert!(!completed.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_eq!(read_vsock_tx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_vsock_config_space_reports_firecracker_feature_bits() {
        let config = VirtioVsockConfigSpace::new(3);
        let features = config.available_features();
        let expected_features = (1_u64 << VIRTIO_FEATURE_VERSION_1)
            | (1_u64 << VIRTIO_FEATURE_IN_ORDER)
            | (1_u64 << VIRTIO_RING_FEATURE_EVENT_IDX);

        assert_eq!(config.guest_cid(), 3);
        assert_eq!(features, expected_features);
    }

    #[test]
    fn virtio_vsock_config_space_reads_guest_cid_as_u64() {
        let config = VirtioVsockConfigSpace::new(0x1122_3344_5566_7788);
        let handler = vsock_handler_for_config(config);

        assert_eq!(
            read_config(&handler, 0, 8),
            0x1122_3344_5566_7788_u64.to_le_bytes().to_vec()
        );
    }

    #[test]
    fn virtio_vsock_config_space_reads_guest_cid_halves() {
        let config = VirtioVsockConfigSpace::new(0x1122_3344_5566_7788);
        let handler = vsock_handler_for_config(config);

        assert_eq!(
            read_config(&handler, 0, 4),
            0x5566_7788_u32.to_le_bytes().to_vec()
        );
        assert_eq!(
            read_config(&handler, 4, 4),
            0x1122_3344_u32.to_le_bytes().to_vec()
        );
    }

    #[test]
    fn virtio_vsock_config_space_rejects_unsupported_reads() {
        let config = VirtioVsockConfigSpace::new(3);
        let handler = vsock_handler_for_config(config);

        let err = handler
            .read_access(device_config_access(2, 4))
            .expect_err("unsupported config read should fail");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 2, len: 4 }
        ));

        let err = handler
            .read_access(device_config_access(8, 1))
            .expect_err("past-end config read should fail");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 8, len: 1 }
        ));

        let err = handler
            .read_access(device_config_access(4, 8))
            .expect_err("straddling config read should fail");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 4, len: 8 }
        ));
    }

    #[test]
    fn virtio_vsock_config_space_rejects_writes() {
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("status should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("status should accept DRIVER");

        let err = handler
            .write_access(
                device_config_access(0, 4),
                MmioAccessBytes::new(&0_u32.to_le_bytes()).expect("test bytes should fit"),
            )
            .expect_err("vsock config write should fail");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite { offset: 0, len: 4 }
        ));
    }

    #[test]
    fn virtio_vsock_mmio_handler_uses_device_id_and_queue_shape() {
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");

        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::DeviceId)
                .expect("device id should read"),
            VIRTIO_VSOCK_DEVICE_ID
        );
        assert_eq!(
            handler.queue_registers().queue_count(),
            VIRTIO_VSOCK_QUEUE_COUNT
        );

        for queue_index in 0..VIRTIO_VSOCK_QUEUE_COUNT {
            handler
                .write_register(
                    VirtioMmioRegister::QueueSel,
                    u32::try_from(queue_index).expect("queue index should fit"),
                )
                .expect("queue select should write");
            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::QueueNumMax)
                    .expect("queue max should read"),
                u32::from(VIRTIO_VSOCK_QUEUE_SIZE)
            );
        }
    }

    #[test]
    fn virtio_vsock_device_activation_retains_active_queue_metadata() {
        let registers = vsock_device_registers();
        let queues = configured_vsock_queue_registers(Some(4), true);
        let mut device = VirtioVsockDevice::new();

        device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect("vsock device should activate");

        assert!(device.is_activated());
        let rx_queue = device
            .active_rx_queue()
            .expect("active RX queue should be present");
        let tx_queue = device
            .active_tx_queue()
            .expect("active TX queue should be present");
        let event_queue = device
            .active_event_queue()
            .expect("active event queue should be present");

        assert_eq!(rx_queue.size(), 4);
        assert_eq!(tx_queue.size(), 4);
        assert_eq!(event_queue.size(), 4);
        assert_eq!(
            rx_queue.descriptor_table(),
            GuestAddress::new(queue_base(0))
        );
        assert_eq!(
            tx_queue.descriptor_table(),
            GuestAddress::new(queue_base(1))
        );
        assert_eq!(
            event_queue.descriptor_table(),
            GuestAddress::new(queue_base(2))
        );
        assert_eq!(
            device
                .active_rx_dispatch_queue()
                .expect("active RX dispatch queue should be present")
                .queue_state(),
            rx_queue
        );
        assert_eq!(
            device
                .active_tx_dispatch_queue()
                .expect("active TX dispatch queue should be present")
                .queue_state(),
            tx_queue
        );
        assert_eq!(
            device
                .active_event_dispatch_queue()
                .expect("active event dispatch queue should be present")
                .queue_state(),
            event_queue
        );
    }

    #[test]
    fn virtio_vsock_device_rejects_duplicate_activation_without_replacing_queues() {
        let registers = vsock_device_registers();
        let first_queues = configured_vsock_queue_registers(Some(4), true);
        let second_queues = configured_vsock_queue_registers(Some(8), true);
        let mut device = VirtioVsockDevice::new();

        device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &first_queues))
            .expect("first activation should succeed");

        let error = device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &second_queues))
            .expect_err("duplicate activation should fail");

        assert!(matches!(
            &error,
            VirtioVsockDeviceActivationError::AlreadyActive
        ));
        assert!(error.source().is_none());
        assert_eq!(error.to_string(), "virtio-vsock device is already active");
        assert_eq!(
            device
                .active_rx_queue()
                .expect("original RX queue should remain active")
                .size(),
            4
        );
    }

    #[test]
    fn virtio_vsock_device_activation_reset_clears_queues_and_allows_retry() {
        let registers = vsock_device_registers();
        let first_queues = configured_vsock_queue_registers(Some(4), true);
        let second_queues = configured_vsock_queue_registers(Some(8), true);
        let mut device = VirtioVsockDevice::new();

        device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &first_queues))
            .expect("first activation should succeed");
        assert!(device.is_activated());

        VirtioMmioDeviceActivationHandler::reset(&mut device);

        assert!(!device.is_activated());
        assert!(device.active_rx_queue().is_none());
        assert!(device.active_tx_queue().is_none());
        assert!(device.active_event_queue().is_none());

        device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &second_queues))
            .expect("second activation should succeed after reset");

        assert_eq!(
            device
                .active_event_queue()
                .expect("new event queue should be active")
                .size(),
            8
        );
    }

    #[test]
    fn virtio_vsock_device_reset_drops_retained_host_connections() {
        let mut device = VirtioVsockDevice::new();
        let (accepted, mut client, request) =
            accepted_host_connection_with_request("device-reset-connection", 4000);
        let key = device
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");

        assert!(device.has_pending_host_request_packet(key));

        VirtioMmioDeviceActivationHandler::reset(&mut device);

        assert!(!device.has_pending_host_request_packet(key));
        assert_stream_closed(&mut client, "reset should close retained host stream");
    }

    #[test]
    fn virtio_vsock_device_reset_drops_guest_connections() {
        let (mut device, path) = host_socket_device(42, "device-reset-guest-connection");
        let listener = guest_connection_listener(&path, 52);
        let key = VsockGuestConnectionKey::new(52, 4000);

        assert!(matches!(
            device.connect_guest_connection_request_packet(&guest_request_tx_packet(42, 52, 4000)),
            Some(super::VsockGuestConnectionRequestOutcome::Retained { key: retained })
                if retained == key
        ));
        assert!(device.has_guest_connection(key));
        let mut accepted = listener.accept();

        VirtioMmioDeviceActivationHandler::reset(&mut device);

        assert_eq!(device.pending_guest_connection_count(), 0);
        assert!(!device.has_guest_connection(key));
        assert_stream_closed(&mut accepted, "reset should close guest-initiated stream");
    }

    #[test]
    fn virtio_vsock_device_rejects_not_ready_queue_without_partial_activation() {
        let registers = vsock_device_registers();
        let queues = configured_vsock_queue_registers(Some(4), false);
        let mut device = VirtioVsockDevice::new();

        let error = device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("not-ready queue should fail activation");

        assert!(matches!(
            &error,
            VirtioVsockDeviceActivationError::RxQueueBuild {
                queue_index: 0,
                source: VirtioVsockQueueBuildError::QueueNotReady
            }
        ));
        assert!(error.source().is_some());
        assert_eq!(
            error.to_string(),
            "failed to activate virtio-vsock RX queue 0: virtio-vsock queue is not ready"
        );
        assert!(!device.is_activated());
        assert!(device.active_rx_queue().is_none());
        assert!(device.active_tx_queue().is_none());
        assert!(device.active_event_queue().is_none());
    }

    #[test]
    fn virtio_vsock_device_rejects_not_ready_tx_queue_without_partial_activation() {
        let registers = vsock_device_registers();
        let queues = configured_vsock_queue_registers_from_specs([
            (Some(4), true),
            (Some(4), false),
            (Some(4), true),
        ]);
        let mut device = VirtioVsockDevice::new();

        let error = device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("not-ready TX queue should fail activation");

        assert!(matches!(
            &error,
            VirtioVsockDeviceActivationError::TxQueueBuild {
                queue_index: 1,
                source: VirtioVsockQueueBuildError::QueueNotReady
            }
        ));
        assert_eq!(
            error.to_string(),
            "failed to activate virtio-vsock TX queue 1: virtio-vsock queue is not ready"
        );
        assert!(!device.is_activated());
    }

    #[test]
    fn virtio_vsock_device_rejects_ready_queue_without_size() {
        let registers = vsock_device_registers();
        let queues = configured_vsock_queue_registers(None, true);
        let mut device = VirtioVsockDevice::new();

        let error = device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("missing queue size should fail activation");

        assert!(matches!(
            &error,
            VirtioVsockDeviceActivationError::RxQueueBuild {
                queue_index: 0,
                source: VirtioVsockQueueBuildError::QueueSizeNotConfigured
            }
        ));
        assert_eq!(
            error.to_string(),
            "failed to activate virtio-vsock RX queue 0: virtio-vsock queue size is not configured"
        );
        assert!(!device.is_activated());
    }

    #[test]
    fn virtio_vsock_device_rejects_ready_event_queue_without_size() {
        let registers = vsock_device_registers();
        let queues = configured_vsock_queue_registers_from_specs([
            (Some(4), true),
            (Some(4), true),
            (None, true),
        ]);
        let mut device = VirtioVsockDevice::new();

        let error = device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("missing event queue size should fail activation");

        assert!(matches!(
            &error,
            VirtioVsockDeviceActivationError::EventQueueBuild {
                queue_index: 2,
                source: VirtioVsockQueueBuildError::QueueSizeNotConfigured
            }
        ));
        assert_eq!(
            error.to_string(),
            "failed to activate virtio-vsock event queue 2: virtio-vsock queue size is not configured"
        );
        assert!(!device.is_activated());
    }

    #[test]
    fn virtio_vsock_device_activation_trait_error_is_generic_handler_error() {
        let registers = vsock_device_registers();
        let queues = configured_vsock_queue_registers(Some(4), false);
        let mut device = VirtioVsockDevice::new();

        let error = VirtioMmioDeviceActivationHandler::activate(
            &mut device,
            VirtioMmioDeviceActivation::new(&registers, &queues),
        )
        .expect_err("trait activation should fail with generic handler error");

        match error {
            VirtioMmioDeviceActivationError::Handler { source } => {
                assert_eq!(
                    source.to_string(),
                    "failed to activate virtio-vsock RX queue 0: virtio-vsock queue is not ready"
                );
            }
        }
        assert!(!device.is_activated());
    }

    #[test]
    fn virtio_vsock_device_activates_and_resets_through_handler() {
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");
        let reset = super::guest_reset_packet_header_for_tx_packet(
            &guest_request_tx_packet(3, 52, 4000),
            3,
        )
        .expect("guest request should produce reset header");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());

        advance_handler_to_features_ok(&mut handler);
        configure_vsock_queues(&mut handler);
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("DRIVER_OK should activate vsock device");

        assert!(handler.is_device_activated());
        assert!(handler.activation_handler().is_activated());
        assert!(handler.activation_handler().active_rx_queue().is_some());
        assert!(handler.activation_handler().active_tx_queue().is_some());
        assert!(handler.activation_handler().active_event_queue().is_some());
        assert!(
            handler
                .activation_handler_mut()
                .queue_guest_reset_packet(reset)
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            1
        );

        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_INIT)
            .expect("status reset should succeed");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
        assert!(handler.activation_handler().active_rx_queue().is_none());
        assert!(handler.activation_handler().active_tx_queue().is_none());
        assert!(handler.activation_handler().active_event_queue().is_none());
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            0
        );
    }

    #[test]
    fn virtio_vsock_notifications_without_pending_work_are_noop() {
        let mut memory = vsock_tx_memory();
        let mut device = VirtioVsockDevice::new();

        let dispatch = device
            .dispatch_drained_queue_notifications(&mut memory, Vec::new())
            .expect("empty notification drain should be a no-op");

        assert_eq!(dispatch.drained_notifications(), &[]);
        assert_empty_host_request_dispatch(dispatch.host_request_dispatch());
        assert_empty_guest_response_dispatch(dispatch.guest_response_dispatch());
        assert!(dispatch.rx_queue_dispatch().is_none());
        assert!(dispatch.tx_queue_dispatch().is_none());
        assert!(!dispatch.needs_queue_interrupt());
    }

    #[test]
    fn virtio_vsock_notifications_reject_inactive_device_with_drained_metadata() {
        let mut memory = vsock_tx_memory();
        let mut device = VirtioVsockDevice::new();

        let error = device
            .dispatch_drained_queue_notifications(&mut memory, vec![VIRTIO_VSOCK_TX_QUEUE_INDEX])
            .expect_err("notification before activation should fail");

        assert!(matches!(
            error,
            super::VirtioVsockDeviceNotificationError::Inactive { .. }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        assert_eq!(
            error.to_string(),
            "virtio-vsock queue notification cannot be dispatched before activation"
        );
        assert!(error.completed_tx_dispatch().is_none());
        assert!(error.completed_rx_dispatch().is_none());
        assert!(error.source().is_none());
    }

    #[test]
    fn virtio_vsock_notifications_dispatch_rx_queue_without_pending_request_as_noop() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("RX notification without pending request should be a no-op");

        assert_eq!(
            notification.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX]
        );
        assert_empty_host_request_dispatch(notification.host_request_dispatch());
        assert_empty_guest_response_dispatch(notification.guest_response_dispatch());
        assert!(!notification.needs_queue_interrupt());
        assert!(notification.tx_queue_dispatch().is_none());
        let rx = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.processed_buffers(), 0);
        assert_eq!(rx.delivered_requests(), 0);
        assert!(!rx.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        let active_rx_queue = handler
            .activation_handler()
            .active_rx_dispatch_queue()
            .expect("RX dispatch queue should remain active");
        assert_eq!(active_rx_queue.available_ring().next_avail(), 0);
        assert_eq!(active_rx_queue.used_ring().next_used(), 0);
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_vsock_notifications_dispatch_late_host_connect_without_second_rx_notify() {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path("notify-late");
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);

        activate_vsock_handler(&mut handler);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let first = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("RX notification without host request should be a no-op");

        assert_eq!(
            first.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX]
        );
        assert_empty_host_request_dispatch(first.host_request_dispatch());
        let first_rx = first
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(first_rx.processed_buffers(), 0);
        assert_eq!(first_rx.delivered_requests(), 0);
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
        assert!(handler.pending_queue_notifications().is_empty());

        let mut client = UnixStream::connect(&path).expect("host client should connect");
        client
            .write_all(b"CONNECT 4000\n")
            .expect("host CONNECT should write");

        let second = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("late host CONNECT should dispatch into available RX buffer");

        assert_eq!(second.drained_notifications(), &[]);
        assert_eq!(second.host_request_dispatch().accepted_connections(), 1);
        assert_eq!(second.host_request_dispatch().completed_requests(), 1);
        assert_eq!(second.host_request_dispatch().dropped_connections(), 0);
        assert_eq!(second.host_request_dispatch().pending_connections(), 0);
        assert!(second.needs_queue_interrupt());
        assert!(second.tx_queue_dispatch().is_none());
        let second_rx = second
            .rx_queue_dispatch()
            .expect("late host CONNECT should produce RX dispatch");
        assert_eq!(second_rx.processed_buffers(), 1);
        assert_eq!(second_rx.delivered_requests(), 1);
        assert!(second_rx.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 0),
            (0, VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32)
        );
        assert_host_connection_request_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            VsockHostLocalPort::try_from_raw(VSOCK_HOST_LOCAL_PORT_BASE)
                .expect("first host local port should be valid"),
            4000,
        );
    }

    #[test]
    fn virtio_vsock_notifications_dispatch_rx_host_request_and_mark_interrupt() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        let (accepted, _client, request) =
            accepted_host_connection_with_request("notify-rx-request", 4000);
        let key = handler
            .activation_handler_mut()
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("RX host request notification should dispatch");

        assert_eq!(
            notification.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX]
        );
        assert!(notification.needs_queue_interrupt());
        assert!(notification.tx_queue_dispatch().is_none());
        let rx = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.processed_buffers(), 1);
        assert_eq!(rx.delivered_requests(), 1);
        assert_eq!(rx.buffer_parse_failures(), 0);
        assert!(rx.needs_queue_interrupt());
        assert_eq!(rx.deliveries()[0].descriptor_head(), 0);
        assert_eq!(
            rx.deliveries()[0].bytes_written_to_guest(),
            VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32
        );
        assert!(
            !handler
                .activation_handler()
                .has_pending_host_request_packet(key)
        );
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 0),
            (0, VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32)
        );
        assert_host_connection_request_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            key.local_port(),
            4000,
        );
    }

    #[test]
    fn virtio_vsock_notifications_keep_pending_rx_request_without_available_buffer() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        let (accepted, _client, request) =
            accepted_host_connection_with_request("notify-rx-empty", 4001);
        let key = handler
            .activation_handler_mut()
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("RX notification without available buffer should be a no-op");

        assert!(!notification.needs_queue_interrupt());
        let rx = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.processed_buffers(), 0);
        assert_eq!(rx.delivered_requests(), 0);
        assert!(
            handler
                .activation_handler()
                .has_pending_host_request_packet(key)
        );
        assert_eq!(read_interrupt_status(&handler), 0);
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_vsock_notifications_reject_malformed_rx_buffer_without_consuming_request() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        let (accepted, _client, request) =
            accepted_host_connection_with_request("notify-rx-bad", 4002);
        let key = handler
            .activation_handler_mut()
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("malformed RX buffer should be completed");

        assert!(notification.needs_queue_interrupt());
        let rx = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.processed_buffers(), 1);
        assert_eq!(rx.delivered_requests(), 0);
        assert_eq!(rx.buffer_parse_failures(), 1);
        assert!(matches!(
            rx.first_buffer_parse_failure(),
            Some(VirtioVsockRxBufferParseError::BufferDescriptorReadOnly { index: 0 })
        ));
        assert!(
            handler
                .activation_handler()
                .has_pending_host_request_packet(key)
        );
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(read_vsock_rx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_notifications_preserve_pending_request_on_rx_used_ring_error() {
        let registers = vsock_device_registers();
        let queues =
            configured_vsock_queue_registers_with_rx_device_ring(TEST_VSOCK_RX_UNMAPPED_USED_RING);
        let mut memory = vsock_tx_memory();
        let mut device = VirtioVsockDevice::with_guest_cid(42);

        device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect("vsock device should activate");
        let (accepted, _client, request) =
            accepted_host_connection_with_request("notify-rx-used", 4003);
        let key = device
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let error = device
            .dispatch_drained_queue_notifications(&mut memory, vec![VIRTIO_VSOCK_RX_QUEUE_INDEX])
            .expect_err("unmapped RX used ring should fail notification dispatch");

        assert!(matches!(
            error,
            super::VirtioVsockDeviceNotificationError::RxQueueDispatch { .. }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX]
        );
        let completed = error
            .completed_rx_dispatch()
            .expect("completed RX metadata should be preserved");
        assert_eq!(completed.processed_buffers(), 0);
        assert_eq!(completed.delivered_requests(), 0);
        assert!(!completed.needs_queue_interrupt());
        assert!(error.completed_tx_dispatch().is_none());
        assert!(device.has_pending_host_request_packet(key));
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_vsock_notifications_reject_event_queue_without_dispatch() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_EVENT_QUEUE_INDEX);

        let error = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect_err("event notification should be unsupported");

        assert!(matches!(
            error,
            super::VirtioVsockDeviceNotificationError::UnsupportedQueue {
                queue_index: VIRTIO_VSOCK_EVENT_QUEUE_INDEX,
                ..
            }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_VSOCK_EVENT_QUEUE_INDEX]
        );
        assert_eq!(
            error.to_string(),
            "virtio-vsock queue notification for unsupported queue 2"
        );
        assert!(error.completed_tx_dispatch().is_none());
        assert!(error.completed_rx_dispatch().is_none());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn virtio_vsock_notifications_reject_mixed_event_queue_without_rx_dispatch() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        let (accepted, _client, request) =
            accepted_host_connection_with_request("notify-event-rx", 4005);
        let key = handler
            .activation_handler_mut()
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_EVENT_QUEUE_INDEX);

        let error = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect_err("mixed event notification should be unsupported before RX dispatch");

        assert!(matches!(
            error,
            super::VirtioVsockDeviceNotificationError::UnsupportedQueue {
                queue_index: VIRTIO_VSOCK_EVENT_QUEUE_INDEX,
                ..
            }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_EVENT_QUEUE_INDEX]
        );
        assert!(
            handler
                .activation_handler()
                .has_pending_host_request_packet(key)
        );
        assert!(error.completed_tx_dispatch().is_none());
        assert!(error.completed_rx_dispatch().is_none());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn virtio_vsock_notifications_dispatch_mixed_rx_noop_and_tx_queue() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");
        let header = test_vsock_packet_header().with_payload_len(0);

        activate_vsock_handler(&mut handler);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("mixed RX/TX notification should dispatch");

        assert_eq!(
            notification.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        assert!(notification.needs_queue_interrupt());
        let rx = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.processed_buffers(), 0);
        assert!(!rx.needs_queue_interrupt());
        let tx = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx.processed_packets(), 1);
        assert_eq!(tx.successful_packets(), 1);
        assert!(tx.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should remain active");
        assert_eq!(active_tx_queue.available_ring().next_avail(), 1);
        assert_eq!(active_tx_queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
    }

    #[test]
    fn virtio_vsock_notifications_dispatch_tx_queue_and_mark_interrupt() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");
        let header = test_vsock_packet_header().with_payload_len(4);
        let payload_address = vsock_payload_address_after_header(TEST_VSOCK_HEADER);

        activate_vsock_handler(&mut handler);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_guest_bytes(&mut memory, payload_address, &[0xa0, 0xa1, 0xa2, 0xa3]);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + 4,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("TX queue notification should dispatch");

        assert_eq!(
            notification.drained_notifications(),
            &[VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        assert!(notification.needs_queue_interrupt());
        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(dispatch.processed_packets(), 1);
        assert_eq!(dispatch.successful_packets(), 1);
        assert_eq!(dispatch.parse_failures(), 0);
        assert!(dispatch.needs_queue_interrupt());
        assert_empty_guest_response_dispatch(notification.guest_response_dispatch());
        assert_eq!(dispatch.packets()[0].descriptor_head(), 0);
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should remain active");
        assert_eq!(active_tx_queue.available_ring().next_avail(), 1);
        assert_eq!(active_tx_queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        assert_eq!(read_vsock_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_notifications_acknowledge_guest_response_to_host_stream() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        let (accepted, mut client, request) =
            accepted_host_connection_with_request("notify-response-ok", 4000);
        let key = handler
            .activation_handler_mut()
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);
        let rx_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("RX host request should dispatch");
        assert_eq!(
            rx_notification
                .rx_queue_dispatch()
                .expect("RX dispatch summary should be present")
                .delivered_requests(),
            1
        );

        let response_header = VirtioVsockPacketHeader::new()
            .with_src_cid(42)
            .with_dst_cid(VIRTIO_VSOCK_HOST_CID)
            .with_src_port(4000)
            .with_dst_port(key.local_port().raw())
            .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
            .with_operation(VIRTIO_VSOCK_OP_RESPONSE);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, response_header);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let tx_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("TX guest response should dispatch");

        assert_eq!(
            tx_notification.drained_notifications(),
            &[VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        assert!(tx_notification.needs_queue_interrupt());
        assert_empty_host_request_dispatch(tx_notification.host_request_dispatch());
        assert_eq!(
            tx_notification.guest_response_dispatch().response_packets(),
            1
        );
        assert_eq!(
            tx_notification
                .guest_response_dispatch()
                .acknowledged_responses(),
            1
        );
        assert_eq!(
            tx_notification
                .guest_response_dispatch()
                .ignored_responses(),
            0
        );
        assert_eq!(
            tx_notification
                .guest_response_dispatch()
                .dropped_connections(),
            0
        );
        assert_empty_guest_request_dispatch(tx_notification.guest_request_dispatch());
        let tx = tx_notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx.processed_packets(), 1);
        assert_eq!(tx.successful_packets(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        assert_eq!(read_vsock_tx_used_element(&memory, 0), (0, 0));
        assert_host_ok_message(&mut client, key.local_port());
    }

    #[test]
    fn virtio_vsock_notifications_connect_guest_request_and_deliver_response() {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path("guest-connect");
        let listener = guest_connection_listener(&path, 52);
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);
        let request = guest_request_tx_packet(42, 52, 4000)
            .header()
            .with_buffer_allocation(1234);

        activate_vsock_handler(&mut handler);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("TX guest request should connect and queue response");

        assert_eq!(
            notification.drained_notifications(),
            &[VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        assert!(notification.needs_queue_interrupt());
        assert_empty_host_request_dispatch(notification.host_request_dispatch());
        assert_empty_guest_response_dispatch(notification.guest_response_dispatch());
        assert_eq!(notification.guest_request_dispatch().request_packets(), 1);
        assert_eq!(notification.guest_request_dispatch().retained_requests(), 1);
        assert_eq!(notification.guest_request_dispatch().ignored_requests(), 0);
        assert_eq!(notification.guest_request_dispatch().dropped_requests(), 0);
        assert_empty_guest_reset_dispatch(notification.guest_reset_dispatch());
        let rx = notification
            .rx_queue_dispatch()
            .expect("guest response should dispatch");
        assert_eq!(rx.delivered_responses(), 1);
        assert_eq!(rx.delivered_reset_packets(), 0);
        assert_guest_response_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            52,
            4000,
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 0),
            (0, VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32)
        );
        let tx = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx.processed_packets(), 1);
        assert_eq!(tx.successful_packets(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        assert_eq!(read_vsock_tx_used_element(&memory, 0), (0, 0));
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_connection_count(),
            1
        );
        let key = VsockGuestConnectionKey::new(52, 4000);
        assert!(handler.activation_handler().has_guest_connection(key));
        assert!(
            !handler
                .activation_handler()
                .has_pending_guest_response_packet(key)
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            0
        );
        let mut accepted = listener.accept();
        drop(handler);
        assert_stream_closed(
            &mut accepted,
            "dropping handler should close retained guest stream after test",
        );
    }

    #[test]
    fn virtio_vsock_notifications_forward_guest_rw_payload_to_host_stream() {
        let (mut memory, mut handler, mut accepted) =
            established_guest_connection_for_test("guest-rw", 52, 4000);
        let payload = b"payload";
        let packet = guest_rw_tx_packet(42, 52, 4000).header();

        write_vsock_tx_packet_with_payload(
            &mut memory,
            1,
            TEST_VSOCK_SECOND_HEADER,
            packet,
            payload,
        );
        append_vsock_tx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("guest RW should forward to host stream");

        assert_empty_guest_response_dispatch(notification.guest_response_dispatch());
        assert_empty_guest_request_dispatch(notification.guest_request_dispatch());
        assert_eq!(notification.guest_rw_dispatch().rw_packets(), 1);
        assert_eq!(notification.guest_rw_dispatch().forwarded_packets(), 1);
        assert_eq!(
            notification.guest_rw_dispatch().forwarded_bytes(),
            payload.len()
        );
        assert_eq!(notification.guest_rw_dispatch().ignored_packets(), 0);
        assert_eq!(notification.guest_rw_dispatch().dropped_connections(), 0);
        assert_empty_guest_reset_dispatch(notification.guest_reset_dispatch());
        assert!(notification.rx_queue_dispatch().is_none());
        assert_eq!(read_vsock_tx_used_index(&memory), 2);
        assert_eq!(read_vsock_tx_used_element(&memory, 1), (1, 0));
        assert_host_payload(&mut accepted, payload);
        assert!(
            handler
                .activation_handler()
                .has_guest_connection(VsockGuestConnectionKey::new(52, 4000))
        );

        drop(handler);
        assert_stream_closed(
            &mut accepted,
            "dropping handler should close forwarded guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_forward_split_guest_rw_payload_to_host_stream() {
        let (mut memory, mut handler, mut accepted) =
            established_guest_connection_for_test("guest-rw-split", 52, 4000);
        let header = guest_rw_tx_packet(42, 52, 4000)
            .header()
            .with_payload_len(5);

        write_vsock_packet_header(&mut memory, TEST_VSOCK_SECOND_HEADER, header);
        write_guest_bytes(&mut memory, TEST_VSOCK_PAYLOAD, b"he");
        write_guest_bytes(&mut memory, TEST_VSOCK_SECOND_PAYLOAD, b"llo");
        write_vsock_tx_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_VSOCK_SECOND_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                Some(2),
            ),
        );
        write_vsock_tx_descriptor(
            &mut memory,
            2,
            TestDescriptor::readable(TEST_VSOCK_PAYLOAD, 2, Some(3)),
        );
        write_vsock_tx_descriptor(
            &mut memory,
            3,
            TestDescriptor::readable(TEST_VSOCK_SECOND_PAYLOAD, 3, None),
        );
        append_vsock_tx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("split guest RW should forward to host stream");

        assert_eq!(notification.guest_rw_dispatch().rw_packets(), 1);
        assert_eq!(notification.guest_rw_dispatch().forwarded_packets(), 1);
        assert_eq!(notification.guest_rw_dispatch().forwarded_bytes(), 5);
        assert_empty_guest_reset_dispatch(notification.guest_reset_dispatch());
        assert_eq!(read_vsock_tx_used_index(&memory), 2);
        assert_eq!(read_vsock_tx_used_element(&memory, 1), (1, 0));
        assert_host_payload(&mut accepted, b"hello");

        drop(handler);
        assert_stream_closed(
            &mut accepted,
            "dropping handler should close split RW guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_drop_guest_rw_connection_on_host_write_failure() {
        let (mut memory, mut handler, accepted) =
            established_guest_connection_for_test("guest-rw-fail", 52, 4000);
        let payload = b"payload";
        let packet = guest_rw_tx_packet(42, 52, 4000).header();
        drop(accepted);

        write_vsock_tx_packet_with_payload(
            &mut memory,
            1,
            TEST_VSOCK_SECOND_HEADER,
            packet,
            payload,
        );
        append_vsock_tx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("guest RW write failure should queue RST");

        assert_eq!(notification.guest_rw_dispatch().rw_packets(), 1);
        assert_eq!(notification.guest_rw_dispatch().forwarded_packets(), 0);
        assert_eq!(notification.guest_rw_dispatch().forwarded_bytes(), 0);
        assert_eq!(notification.guest_rw_dispatch().dropped_connections(), 1);
        assert_eq!(notification.guest_reset_dispatch().reset_candidates(), 1);
        assert_eq!(notification.guest_reset_dispatch().queued_resets(), 1);
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_connection_count(),
            0
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            1
        );
    }

    #[test]
    fn virtio_vsock_notifications_drop_guest_rw_before_response_delivery() {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path("guest-rw-pending");
        let listener = guest_connection_listener(&path, 52);
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);
        let request = guest_request_tx_packet(42, 52, 4000).header();
        let rw = guest_rw_tx_packet(42, 52, 4000).header();
        let payload = b"early";
        let key = VsockGuestConnectionKey::new(52, 4000);

        activate_vsock_handler(&mut handler);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let connect_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("guest request should connect without RX buffer");

        assert_eq!(
            connect_notification
                .guest_request_dispatch()
                .retained_requests(),
            1
        );
        let rx = connect_notification
            .rx_queue_dispatch()
            .expect("pending response should attempt RX dispatch");
        assert_eq!(rx.processed_buffers(), 0);
        assert!(handler.activation_handler().has_guest_connection(key));
        assert!(
            handler
                .activation_handler()
                .has_pending_guest_response_packet(key)
        );
        let mut accepted = listener.accept();

        write_vsock_tx_packet_with_payload(&mut memory, 1, TEST_VSOCK_SECOND_HEADER, rw, payload);
        append_vsock_tx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let rw_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("early guest RW should drop retained connection and queue RST");

        assert_eq!(rw_notification.guest_rw_dispatch().rw_packets(), 1);
        assert_eq!(rw_notification.guest_rw_dispatch().forwarded_packets(), 0);
        assert_eq!(rw_notification.guest_rw_dispatch().dropped_connections(), 1);
        assert_eq!(rw_notification.guest_reset_dispatch().reset_candidates(), 1);
        assert_eq!(rw_notification.guest_reset_dispatch().queued_resets(), 1);
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_connection_count(),
            0
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            1
        );
        assert_stream_closed(
            &mut accepted,
            "early RW should close the retained guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_forward_guest_rw_to_matching_connection_only() {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path("guest-rw-iso");
        let first_listener = guest_connection_listener(&path, 52);
        let second_listener = guest_connection_listener(&path, 53);
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);
        let first_request = guest_request_tx_packet(42, 52, 4000).header();
        let second_request = guest_request_tx_packet(42, 53, 4001).header();
        let first_payload = b"first";
        let second_payload = b"second";

        activate_vsock_handler(&mut handler);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                TEST_VSOCK_RX_SECOND_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0, 1]);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, first_request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_packet_header(&mut memory, TEST_VSOCK_SECOND_HEADER, second_request);
        write_vsock_tx_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_VSOCK_SECOND_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0, 1]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let connect_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("guest requests should connect");

        assert_eq!(
            connect_notification
                .guest_request_dispatch()
                .retained_requests(),
            2
        );
        assert_empty_guest_rw_dispatch(connect_notification.guest_rw_dispatch());
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);
        let response_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("second guest response should deliver");
        let rx = response_notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.delivered_responses(), 1);
        assert_eq!(read_vsock_rx_used_index(&memory), 2);
        let mut first_accepted = first_listener.accept();
        let mut second_accepted = second_listener.accept();

        write_vsock_tx_packet_with_payload(
            &mut memory,
            2,
            TEST_VSOCK_HEADER,
            guest_rw_tx_packet(42, 52, 4000).header(),
            first_payload,
        );
        write_vsock_tx_packet_with_payload(
            &mut memory,
            3,
            TEST_VSOCK_SECOND_HEADER,
            guest_rw_tx_packet(42, 53, 4001).header(),
            second_payload,
        );
        append_vsock_tx_available_head(&mut memory, 2, 2, 3);
        append_vsock_tx_available_head(&mut memory, 3, 3, 4);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let rw_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("guest RW packets should forward independently");

        assert_eq!(rw_notification.guest_rw_dispatch().rw_packets(), 2);
        assert_eq!(rw_notification.guest_rw_dispatch().forwarded_packets(), 2);
        assert_eq!(
            rw_notification.guest_rw_dispatch().forwarded_bytes(),
            first_payload.len() + second_payload.len()
        );
        assert_empty_guest_reset_dispatch(rw_notification.guest_reset_dispatch());
        assert_host_payload(&mut first_accepted, first_payload);
        assert_host_payload(&mut second_accepted, second_payload);

        drop(handler);
        assert_stream_closed(
            &mut first_accepted,
            "dropping handler should close first isolated RW stream",
        );
        assert_stream_closed(
            &mut second_accepted,
            "dropping handler should close second isolated RW stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_deliver_guest_connection_host_rw_payload_to_guest() {
        let (mut memory, mut handler, mut accepted) =
            established_guest_connection_for_test("host-rw-guest", 52, 4000);
        let payload = b"host-payload";
        accepted
            .write_all(payload)
            .expect("host payload should write");
        write_vsock_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                TEST_VSOCK_RX_SECOND_BUFFER,
                vsock_packet_len_with_payload(payload),
                None,
            ),
        );
        append_vsock_rx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("host RW should deliver to guest RX buffer");

        assert_eq!(
            notification.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX]
        );
        assert_empty_host_request_dispatch(notification.host_request_dispatch());
        assert_empty_guest_response_dispatch(notification.guest_response_dispatch());
        assert_empty_guest_request_dispatch(notification.guest_request_dispatch());
        assert_empty_guest_rw_dispatch(notification.guest_rw_dispatch());
        assert_empty_guest_reset_dispatch(notification.guest_reset_dispatch());
        let rx = notification
            .rx_queue_dispatch()
            .expect("host RW should produce RX dispatch");
        assert_eq!(rx.processed_buffers(), 1);
        assert_eq!(rx.delivered_host_rw_packets(), 1);
        assert_eq!(rx.delivered_host_rw_bytes(), payload.len());
        assert_eq!(rx.delivered_reset_packets(), 0);
        assert_eq!(rx.buffer_parse_failures(), 0);
        assert_eq!(rx.buffer_too_small_failures(), 0);
        assert_eq!(rx.deliveries().len(), 1);
        assert_eq!(
            rx.deliveries()[0].packet_kind(),
            VirtioVsockRxPacketKind::HostRw
        );
        assert_eq!(rx.deliveries()[0].descriptor_head(), 1);
        assert_eq!(
            rx.deliveries()[0].bytes_written_to_guest(),
            vsock_packet_len_with_payload(payload)
        );
        assert_eq!(
            rx.deliveries()[0].payload_bytes_written_to_guest(),
            payload.len()
        );
        assert_host_rw_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_SECOND_BUFFER),
            42,
            52,
            4000,
            payload.len(),
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                vsock_payload_address_after_header(TEST_VSOCK_RX_SECOND_BUFFER),
                payload.len(),
            ),
            payload
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 2);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 1),
            (1, vsock_packet_len_with_payload(payload))
        );
        assert!(
            handler
                .activation_handler()
                .has_guest_connection(VsockGuestConnectionKey::new(52, 4000))
        );

        drop(handler);
        assert_stream_closed(
            &mut accepted,
            "dropping handler should close host RW guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_deliver_host_connection_rw_payload_to_guest() {
        let (mut memory, mut handler, mut client, key) =
            established_host_connection_for_test("host-rw-host", 4000);
        let payload = b"host-initiated-payload";
        client
            .write_all(payload)
            .expect("host-initiated payload should write");
        write_vsock_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                TEST_VSOCK_RX_SECOND_BUFFER,
                vsock_packet_len_with_payload(payload),
                None,
            ),
        );
        append_vsock_rx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("host-initiated RW should deliver to guest RX buffer");

        assert_empty_host_request_dispatch(notification.host_request_dispatch());
        assert_empty_guest_response_dispatch(notification.guest_response_dispatch());
        assert_empty_guest_request_dispatch(notification.guest_request_dispatch());
        assert_empty_guest_rw_dispatch(notification.guest_rw_dispatch());
        assert_empty_guest_reset_dispatch(notification.guest_reset_dispatch());
        let rx = notification
            .rx_queue_dispatch()
            .expect("host-initiated RW should produce RX dispatch");
        assert_eq!(rx.processed_buffers(), 1);
        assert_eq!(rx.delivered_host_rw_packets(), 1);
        assert_eq!(rx.delivered_host_rw_bytes(), payload.len());
        assert_eq!(rx.delivered_requests(), 0);
        assert_eq!(rx.delivered_reset_packets(), 0);
        assert_host_rw_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_SECOND_BUFFER),
            42,
            key.local_port().raw(),
            4000,
            payload.len(),
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                vsock_payload_address_after_header(TEST_VSOCK_RX_SECOND_BUFFER),
                payload.len(),
            ),
            payload
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 2);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 1),
            (1, vsock_packet_len_with_payload(payload))
        );
        assert!(handler.activation_handler().has_host_connection(key));

        drop(handler);
        assert_stream_closed(
            &mut client,
            "dropping handler should close host-initiated RW stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_write_host_rw_payload_across_split_rx_buffer() {
        let (mut memory, mut handler, mut accepted) =
            established_guest_connection_for_test("host-rw-split", 52, 4000);
        let payload = b"split-host-rw";
        accepted
            .write_all(payload)
            .expect("split host payload should write");
        write_vsock_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                TEST_VSOCK_RX_SECOND_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + 5,
                Some(2),
            ),
        );
        write_vsock_rx_descriptor(
            &mut memory,
            2,
            TestDescriptor::writable(
                TEST_VSOCK_PAYLOAD,
                u32::try_from(payload.len() - 5).expect("test payload length should fit"),
                None,
            ),
        );
        append_vsock_rx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("split RX buffer should receive host RW");

        let rx = notification
            .rx_queue_dispatch()
            .expect("split host RW should produce RX dispatch");
        assert_eq!(rx.delivered_host_rw_packets(), 1);
        assert_eq!(rx.delivered_host_rw_bytes(), payload.len());
        assert_eq!(read_vsock_rx_used_index(&memory), 2);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 1),
            (1, vsock_packet_len_with_payload(payload))
        );
        assert_host_rw_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_SECOND_BUFFER),
            42,
            52,
            4000,
            payload.len(),
        );
        let mut written_payload = read_guest_bytes(
            &memory,
            vsock_payload_address_after_header(TEST_VSOCK_RX_SECOND_BUFFER),
            5,
        );
        written_payload.extend(read_guest_bytes(
            &memory,
            TEST_VSOCK_PAYLOAD,
            payload.len() - 5,
        ));
        assert_eq!(written_payload, payload);

        drop(handler);
        assert_stream_closed(
            &mut accepted,
            "dropping handler should close split host RW guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_preserve_host_rw_payload_without_rx_buffer() {
        let (mut memory, mut handler, mut accepted) =
            established_guest_connection_for_test("host-rw-late", 52, 4000);
        let payload = b"late-host-rw";
        accepted
            .write_all(payload)
            .expect("late-buffer host payload should write");

        let empty_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("host RW without RX buffer should stay pending");

        assert_empty_host_request_dispatch(empty_notification.host_request_dispatch());
        assert_empty_guest_response_dispatch(empty_notification.guest_response_dispatch());
        assert_empty_guest_request_dispatch(empty_notification.guest_request_dispatch());
        assert_empty_guest_rw_dispatch(empty_notification.guest_rw_dispatch());
        assert_empty_guest_reset_dispatch(empty_notification.guest_reset_dispatch());
        assert!(!empty_notification.needs_queue_interrupt());
        let empty_rx = empty_notification
            .rx_queue_dispatch()
            .expect("pending host RW should attempt RX dispatch");
        assert_eq!(empty_rx.processed_buffers(), 0);
        assert_eq!(empty_rx.delivered_host_rw_packets(), 0);
        assert_eq!(read_vsock_rx_used_index(&memory), 1);

        write_vsock_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                TEST_VSOCK_RX_SECOND_BUFFER,
                vsock_packet_len_with_payload(payload),
                None,
            ),
        );
        append_vsock_rx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let retry_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("late RX buffer should receive pending host RW payload");

        let retry_rx = retry_notification
            .rx_queue_dispatch()
            .expect("late RX buffer should produce RX dispatch");
        assert_eq!(retry_rx.processed_buffers(), 1);
        assert_eq!(retry_rx.delivered_host_rw_packets(), 1);
        assert_eq!(retry_rx.delivered_host_rw_bytes(), payload.len());
        assert_host_rw_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_SECOND_BUFFER),
            42,
            52,
            4000,
            payload.len(),
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                vsock_payload_address_after_header(TEST_VSOCK_RX_SECOND_BUFFER),
                payload.len(),
            ),
            payload
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 2);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 1),
            (1, vsock_packet_len_with_payload(payload))
        );

        drop(handler);
        assert_stream_closed(
            &mut accepted,
            "dropping handler should close late-buffer host RW guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_preserve_host_rw_payload_after_small_rx_buffer() {
        let (mut memory, mut handler, mut accepted) =
            established_guest_connection_for_test("host-rw-retry", 52, 4000);
        let payload = b"retry-host-rw";
        accepted
            .write_all(payload)
            .expect("retry host payload should write");
        write_vsock_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                TEST_VSOCK_RX_SECOND_BUFFER,
                vsock_packet_len_with_payload(payload) - 1,
                None,
            ),
        );
        append_vsock_rx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let small_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("small RX buffer should preserve host RW payload");

        let small_rx = small_notification
            .rx_queue_dispatch()
            .expect("small RX buffer should produce RX dispatch");
        assert_eq!(small_rx.processed_buffers(), 1);
        assert_eq!(small_rx.delivered_host_rw_packets(), 0);
        assert_eq!(small_rx.buffer_too_small_failures(), 1);
        let too_small = small_rx
            .first_buffer_too_small()
            .expect("too-small metadata should be present");
        assert_eq!(too_small.descriptor_head(), 1);
        assert_eq!(
            too_small.buffer_len(),
            u64::from(vsock_packet_len_with_payload(payload) - 1)
        );
        assert_eq!(
            too_small.required_len(),
            u64::from(vsock_packet_len_with_payload(payload))
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 2);
        assert_eq!(read_vsock_rx_used_element(&memory, 1), (1, 0));

        write_vsock_rx_descriptor(
            &mut memory,
            2,
            TestDescriptor::writable(
                TEST_VSOCK_PAYLOAD,
                vsock_packet_len_with_payload(payload),
                None,
            ),
        );
        append_vsock_rx_available_head(&mut memory, 2, 2, 3);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let retry_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("retry RX buffer should receive preserved host RW payload");

        let retry_rx = retry_notification
            .rx_queue_dispatch()
            .expect("retry RX should produce dispatch");
        assert_eq!(retry_rx.processed_buffers(), 1);
        assert_eq!(retry_rx.delivered_host_rw_packets(), 1);
        assert_eq!(retry_rx.delivered_host_rw_bytes(), payload.len());
        assert_eq!(retry_rx.buffer_too_small_failures(), 0);
        assert_host_rw_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_PAYLOAD),
            42,
            52,
            4000,
            payload.len(),
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                vsock_payload_address_after_header(TEST_VSOCK_PAYLOAD),
                payload.len(),
            ),
            payload
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 3);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 2),
            (2, vsock_packet_len_with_payload(payload))
        );

        drop(handler);
        assert_stream_closed(
            &mut accepted,
            "dropping handler should close retried host RW guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_queue_reset_when_host_rw_stream_closes() {
        let (mut memory, mut handler, accepted) =
            established_guest_connection_for_test("host-rw-eof", 52, 4000);
        drop(accepted);
        write_vsock_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                TEST_VSOCK_RX_SECOND_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        append_vsock_rx_available_head(&mut memory, 1, 1, 2);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("closed host stream should queue guest reset");

        let rx = notification
            .rx_queue_dispatch()
            .expect("queued reset should dispatch");
        assert_eq!(rx.processed_buffers(), 1);
        assert_eq!(rx.delivered_reset_packets(), 1);
        assert_eq!(rx.delivered_host_rw_packets(), 0);
        assert_guest_reset_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_SECOND_BUFFER),
            42,
            52,
            4000,
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 2);
        assert_eq!(
            read_vsock_rx_used_element(&memory, 1),
            (1, VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32)
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_connection_count(),
            0
        );
    }

    #[test]
    fn virtio_vsock_notifications_deliver_host_rw_from_matching_connection_only() {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path("host-rw-iso");
        let first_listener = guest_connection_listener(&path, 52);
        let second_listener = guest_connection_listener(&path, 53);
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);
        let first_request = guest_request_tx_packet(42, 52, 4000).header();
        let second_request = guest_request_tx_packet(42, 53, 4001).header();
        let payload = b"second-only";

        activate_vsock_handler(&mut handler);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                TEST_VSOCK_RX_SECOND_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0, 1]);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, first_request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_packet_header(&mut memory, TEST_VSOCK_SECOND_HEADER, second_request);
        write_vsock_tx_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_VSOCK_SECOND_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0, 1]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let connect_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("guest requests should connect");
        assert_eq!(
            connect_notification
                .guest_request_dispatch()
                .retained_requests(),
            2
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);
        let response_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("second guest response should deliver");
        assert_eq!(
            response_notification
                .rx_queue_dispatch()
                .expect("second response should produce RX dispatch")
                .delivered_responses(),
            1
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 2);
        let mut first_accepted = first_listener.accept();
        let mut second_accepted = second_listener.accept();
        second_accepted
            .write_all(payload)
            .expect("second host payload should write");

        write_vsock_rx_descriptor(
            &mut memory,
            2,
            TestDescriptor::writable(
                TEST_VSOCK_PAYLOAD,
                vsock_packet_len_with_payload(payload),
                None,
            ),
        );
        append_vsock_rx_available_head(&mut memory, 2, 2, 3);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let rw_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("second host RW should deliver independently");

        let rx = rw_notification
            .rx_queue_dispatch()
            .expect("second host RW should produce RX dispatch");
        assert_eq!(rx.delivered_host_rw_packets(), 1);
        assert_eq!(rx.delivered_host_rw_bytes(), payload.len());
        assert_host_rw_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_PAYLOAD),
            42,
            53,
            4001,
            payload.len(),
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                vsock_payload_address_after_header(TEST_VSOCK_PAYLOAD),
                payload.len(),
            ),
            payload
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_connection_count(),
            2
        );

        drop(handler);
        assert_stream_closed(
            &mut first_accepted,
            "dropping handler should close first isolated host RW stream",
        );
        assert_stream_closed(
            &mut second_accepted,
            "dropping handler should close second isolated host RW stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_retry_pending_guest_response_after_late_rx_buffer() {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path("guest-response-retry");
        let listener = guest_connection_listener(&path, 52);
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);
        let request = guest_request_tx_packet(42, 52, 4000).header();
        let key = VsockGuestConnectionKey::new(52, 4000);

        activate_vsock_handler(&mut handler);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let tx_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("TX guest request should connect without RX buffer");

        assert_eq!(
            tx_notification.guest_request_dispatch().retained_requests(),
            1
        );
        let rx = tx_notification
            .rx_queue_dispatch()
            .expect("pending guest response should attempt RX dispatch");
        assert_eq!(rx.processed_buffers(), 0);
        assert!(
            handler
                .activation_handler()
                .has_pending_guest_response_packet(key)
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 0);

        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let rx_notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("late RX buffer should receive pending guest response");

        let rx = rx_notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.delivered_responses(), 1);
        assert_guest_response_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            52,
            4000,
        );
        assert!(
            !handler
                .activation_handler()
                .has_pending_guest_response_packet(key)
        );

        let mut accepted = listener.accept();
        drop(handler);
        assert_stream_closed(
            &mut accepted,
            "dropping handler should close retry test guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_queue_reset_when_guest_connect_has_no_host_path() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");
        let request = guest_request_tx_packet(42, 52, 4000).header();

        activate_vsock_handler(&mut handler);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("missing host socket path should queue RST");

        assert_eq!(notification.guest_request_dispatch().request_packets(), 1);
        assert_eq!(notification.guest_request_dispatch().retained_requests(), 0);
        assert_eq!(notification.guest_request_dispatch().dropped_requests(), 1);
        assert_eq!(notification.guest_reset_dispatch().queued_resets(), 1);
        let rx = notification
            .rx_queue_dispatch()
            .expect("RST should dispatch");
        assert_eq!(rx.delivered_reset_packets(), 1);
        assert_guest_reset_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            52,
            4000,
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_connection_count(),
            0
        );
    }

    #[test]
    fn virtio_vsock_notifications_queue_reset_when_guest_connect_fails() {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path("guest-connect-fails");
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);
        let request = guest_request_tx_packet(42, 52, 4000).header();

        activate_vsock_handler(&mut handler);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("failed guest host socket connect should queue RST");

        assert_eq!(notification.guest_request_dispatch().request_packets(), 1);
        assert_eq!(notification.guest_request_dispatch().retained_requests(), 0);
        assert_eq!(notification.guest_request_dispatch().dropped_requests(), 1);
        assert_eq!(notification.guest_reset_dispatch().queued_resets(), 1);
        let rx = notification
            .rx_queue_dispatch()
            .expect("RST should dispatch");
        assert_eq!(rx.delivered_reset_packets(), 1);
        assert_guest_reset_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            52,
            4000,
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_connection_count(),
            0
        );
    }

    #[test]
    fn virtio_vsock_notifications_keep_guest_connect_paths_independent() {
        let mut first_memory = vsock_tx_memory();
        let mut second_memory = vsock_tx_memory();
        let first_path = unique_socket_path("guest-connect-isolated-a");
        let second_path = unique_socket_path("guest-connect-isolated-b");
        let second_listener = guest_connection_listener(&second_path, 52);
        let mut first_handler = virtio_vsock_mmio_handler_with_host_socket(42, &first_path);
        let mut second_handler = virtio_vsock_mmio_handler_with_host_socket(42, &second_path);
        let request = guest_request_tx_packet(42, 52, 4000).header();

        activate_vsock_handler(&mut first_handler);
        activate_vsock_handler(&mut second_handler);
        for (memory, handler) in [
            (&mut first_memory, &mut first_handler),
            (&mut second_memory, &mut second_handler),
        ] {
            write_vsock_rx_descriptor(
                memory,
                0,
                TestDescriptor::writable(
                    TEST_VSOCK_RX_BUFFER,
                    VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                    None,
                ),
            );
            write_vsock_rx_available_heads(memory, &[0]);
            write_vsock_packet_header(memory, TEST_VSOCK_HEADER, request);
            write_vsock_tx_descriptor(
                memory,
                0,
                TestDescriptor::readable(
                    TEST_VSOCK_HEADER,
                    VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                    None,
                ),
            );
            write_vsock_tx_available_heads(memory, &[0]);
            notify_vsock_queue(handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);
        }

        let first_notification = first_handler
            .dispatch_vsock_queue_notifications(&mut first_memory)
            .expect("first handler should fail only its own guest connect");
        let second_notification = second_handler
            .dispatch_vsock_queue_notifications(&mut second_memory)
            .expect("second handler should connect only its own guest socket");

        assert_eq!(
            first_notification
                .rx_queue_dispatch()
                .expect("first RX dispatch should be present")
                .delivered_reset_packets(),
            1
        );
        assert_eq!(
            second_notification
                .rx_queue_dispatch()
                .expect("second RX dispatch should be present")
                .delivered_responses(),
            1
        );
        assert_guest_reset_packet_header(
            read_vsock_packet_header(&first_memory, TEST_VSOCK_RX_BUFFER),
            42,
            52,
            4000,
        );
        assert_guest_response_packet_header(
            read_vsock_packet_header(&second_memory, TEST_VSOCK_RX_BUFFER),
            42,
            52,
            4000,
        );

        let mut accepted = second_listener.accept();
        drop(second_handler);
        assert_stream_closed(
            &mut accepted,
            "dropping second handler should close isolated guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_queue_reset_for_duplicate_guest_request() {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path("guest-duplicate");
        let listener = guest_connection_listener(&path, 52);
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);
        let request = guest_request_tx_packet(42, 52, 4000).header();

        activate_vsock_handler(&mut handler);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_packet_header(&mut memory, TEST_VSOCK_SECOND_HEADER, request);
        write_vsock_tx_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_VSOCK_SECOND_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0, 1]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("duplicate guest request should queue RST");

        assert_eq!(notification.guest_request_dispatch().request_packets(), 2);
        assert_eq!(notification.guest_request_dispatch().retained_requests(), 1);
        assert_eq!(notification.guest_request_dispatch().ignored_requests(), 0);
        assert_eq!(notification.guest_request_dispatch().dropped_requests(), 1);
        assert_eq!(notification.guest_reset_dispatch().reset_candidates(), 1);
        assert_eq!(notification.guest_reset_dispatch().queued_resets(), 1);
        assert_eq!(notification.guest_reset_dispatch().dropped_resets(), 0);
        let rx = notification
            .rx_queue_dispatch()
            .expect("duplicate guest request RST should dispatch");
        assert_eq!(rx.delivered_reset_packets(), 1);
        assert_eq!(rx.delivered_responses(), 0);
        assert_guest_reset_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            52,
            4000,
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_connection_count(),
            1
        );
        assert!(
            handler
                .activation_handler()
                .has_pending_guest_response_packet(VsockGuestConnectionKey::new(52, 4000))
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            0
        );
        let mut accepted = listener.accept();
        drop(handler);
        assert_stream_closed(
            &mut accepted,
            "dropping handler should close duplicate test guest stream",
        );
    }

    #[test]
    fn virtio_vsock_notifications_queue_reset_when_guest_request_limit_is_full() {
        let mut memory = vsock_tx_memory();
        let path = unique_socket_path("guest-limit-full");
        let _listener = guest_connection_listener(&path, 52);
        let mut handler = virtio_vsock_mmio_handler_with_host_socket(42, &path);
        let request = guest_request_tx_packet(42, 52, 4000).header();

        activate_vsock_handler(&mut handler);
        handler
            .activation_handler_mut()
            .set_guest_connection_limit(0);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, request);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("full guest request table should queue RST");

        assert_eq!(notification.guest_request_dispatch().request_packets(), 1);
        assert_eq!(notification.guest_request_dispatch().retained_requests(), 0);
        assert_eq!(notification.guest_request_dispatch().dropped_requests(), 1);
        assert_eq!(notification.guest_reset_dispatch().reset_candidates(), 1);
        assert_eq!(notification.guest_reset_dispatch().queued_resets(), 1);
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_connection_count(),
            0
        );
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            1
        );
    }

    #[test]
    fn virtio_vsock_notifications_drop_wrong_cid_and_guest_reset_without_rx_output() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");
        let wrong_destination = guest_request_tx_packet(42, 52, 4000)
            .header()
            .with_dst_cid(VIRTIO_VSOCK_HOST_CID + 1);
        let wrong_source = guest_request_tx_packet(43, 53, 4001).header();
        let guest_reset = guest_request_tx_packet(42, 53, 4001)
            .header()
            .with_operation(VIRTIO_VSOCK_OP_RST);

        activate_vsock_handler(&mut handler);
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, wrong_destination);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_packet_header(&mut memory, TEST_VSOCK_SECOND_HEADER, wrong_source);
        write_vsock_tx_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_VSOCK_SECOND_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_packet_header(&mut memory, TEST_VSOCK_PAYLOAD, guest_reset);
        write_vsock_tx_descriptor(
            &mut memory,
            2,
            TestDescriptor::readable(
                TEST_VSOCK_PAYLOAD,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0, 1, 2]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("ignored guest packets should still complete TX");

        assert_empty_guest_response_dispatch(notification.guest_response_dispatch());
        assert_eq!(notification.guest_request_dispatch().request_packets(), 2);
        assert_eq!(notification.guest_request_dispatch().retained_requests(), 0);
        assert_eq!(notification.guest_request_dispatch().ignored_requests(), 2);
        assert_eq!(notification.guest_request_dispatch().dropped_requests(), 0);
        assert_empty_guest_reset_dispatch(notification.guest_reset_dispatch());
        assert!(notification.rx_queue_dispatch().is_none());
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
        assert_eq!(read_vsock_tx_used_index(&memory), 3);
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            0
        );
    }

    #[test]
    fn virtio_vsock_notifications_queue_resets_for_unsupported_type_and_orphan_response() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");
        let unsupported_type = guest_request_tx_packet(42, 52, 4000)
            .header()
            .with_packet_type(0);
        let payload_request = guest_request_tx_packet(42, 53, 4001)
            .header()
            .with_payload_len(1);
        let orphan_response = guest_response_tx_packet(
            42,
            VsockHostLocalPort::try_from_raw(VSOCK_HOST_LOCAL_PORT_BASE)
                .expect("test local port should be valid"),
            4002,
        )
        .header();

        activate_vsock_handler(&mut handler);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, unsupported_type);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_packet_header(&mut memory, TEST_VSOCK_SECOND_HEADER, payload_request);
        write_guest_bytes(
            &mut memory,
            vsock_payload_address_after_header(TEST_VSOCK_SECOND_HEADER),
            &[0xa5],
        );
        write_vsock_tx_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_VSOCK_SECOND_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 + 1,
                None,
            ),
        );
        write_vsock_packet_header(&mut memory, TEST_VSOCK_PAYLOAD, orphan_response);
        write_vsock_tx_descriptor(
            &mut memory,
            2,
            TestDescriptor::readable(
                TEST_VSOCK_PAYLOAD,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0, 1, 2]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("unsupported guest packets should queue RST packets");

        assert_eq!(notification.guest_response_dispatch().response_packets(), 1);
        assert_eq!(
            notification.guest_response_dispatch().ignored_responses(),
            1
        );
        assert_eq!(notification.guest_request_dispatch().request_packets(), 2);
        assert_eq!(notification.guest_request_dispatch().retained_requests(), 0);
        assert_eq!(notification.guest_request_dispatch().ignored_requests(), 2);
        assert_eq!(notification.guest_request_dispatch().dropped_requests(), 0);
        assert_eq!(notification.guest_reset_dispatch().reset_candidates(), 3);
        assert_eq!(notification.guest_reset_dispatch().queued_resets(), 3);
        assert_eq!(notification.guest_reset_dispatch().dropped_resets(), 0);
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            3
        );
        let rx = notification
            .rx_queue_dispatch()
            .expect("pending guest RST should attempt RX dispatch");
        assert_eq!(rx.processed_buffers(), 0);
        assert_eq!(rx.delivered_reset_packets(), 0);
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
        assert_eq!(read_vsock_tx_used_index(&memory), 3);
    }

    #[test]
    fn virtio_vsock_device_drops_guest_resets_when_queue_is_full() {
        let mut device = VirtioVsockDevice::with_guest_cid(42);
        let header = super::guest_reset_packet_header_for_tx_packet(
            &guest_request_tx_packet(42, 52, 4000),
            42,
        )
        .expect("guest request should produce reset header");

        for _ in 0..VIRTIO_VSOCK_QUEUE_SIZE {
            assert!(device.queue_guest_reset_packet(header));
        }

        assert!(!device.queue_guest_reset_packet(header));
        assert_eq!(
            device.pending_guest_reset_packet_count(),
            usize::from(VIRTIO_VSOCK_QUEUE_SIZE)
        );
    }

    #[test]
    fn virtio_vsock_notifications_deliver_pending_guest_reset_before_host_request() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");
        let reset = super::guest_reset_packet_header_for_tx_packet(
            &guest_request_tx_packet(42, 52, 4000),
            42,
        )
        .expect("guest request should produce reset header");

        activate_vsock_handler(&mut handler);
        let (accepted, _client, request) =
            accepted_host_connection_with_request("reset-priority", 4001);
        let key = handler
            .activation_handler_mut()
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");
        assert!(
            handler
                .activation_handler_mut()
                .queue_guest_reset_packet(reset)
        );
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("pending guest RST should dispatch before host request");

        let rx = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.delivered_reset_packets(), 1);
        assert_eq!(rx.delivered_requests(), 0);
        assert_guest_reset_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            52,
            4000,
        );
        assert!(
            handler
                .activation_handler()
                .has_pending_host_request_packet(key)
        );
    }

    #[test]
    fn virtio_vsock_notifications_keep_guest_reset_pending_after_rx_buffer_failure() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");
        let reset = super::guest_reset_packet_header_for_tx_packet(
            &guest_request_tx_packet(42, 52, 4000),
            42,
        )
        .expect("guest request should produce reset header");

        activate_vsock_handler(&mut handler);
        assert!(
            handler
                .activation_handler_mut()
                .queue_guest_reset_packet(reset)
        );
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("malformed RX buffer should be completed");

        let rx = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.processed_buffers(), 1);
        assert_eq!(rx.delivered_reset_packets(), 0);
        assert_eq!(rx.buffer_parse_failures(), 1);
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            1
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(read_vsock_rx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_notifications_keep_guest_reset_pending_after_small_rx_buffer() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");
        let reset = super::guest_reset_packet_header_for_tx_packet(
            &guest_request_tx_packet(42, 52, 4000),
            42,
        )
        .expect("guest request should produce reset header");

        activate_vsock_handler(&mut handler);
        assert!(
            handler
                .activation_handler_mut()
                .queue_guest_reset_packet(reset)
        );
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 - 1,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("small RX buffer should be completed");

        let rx = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.processed_buffers(), 1);
        assert_eq!(rx.delivered_reset_packets(), 0);
        assert_eq!(rx.buffer_too_small_failures(), 1);
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            1
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(read_vsock_rx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_notifications_keep_guest_reset_pending_after_rx_used_ring_error() {
        let registers = vsock_device_registers();
        let queues =
            configured_vsock_queue_registers_with_rx_device_ring(TEST_VSOCK_RX_UNMAPPED_USED_RING);
        let mut memory = vsock_tx_memory();
        let mut device = VirtioVsockDevice::with_guest_cid(42);
        let reset = super::guest_reset_packet_header_for_tx_packet(
            &guest_request_tx_packet(42, 52, 4000),
            42,
        )
        .expect("guest request should produce reset header");

        device
            .activate_vsock(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect("vsock device should activate");
        assert!(device.queue_guest_reset_packet(reset));
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);

        let error = device
            .dispatch_drained_queue_notifications(&mut memory, vec![VIRTIO_VSOCK_RX_QUEUE_INDEX])
            .expect_err("unmapped RX used ring should fail notification dispatch");

        assert!(matches!(
            error,
            super::VirtioVsockDeviceNotificationError::RxQueueDispatch { .. }
        ));
        let completed = error
            .completed_rx_dispatch()
            .expect("completed RX metadata should be preserved");
        assert_eq!(completed.processed_buffers(), 0);
        assert_eq!(completed.delivered_reset_packets(), 0);
        assert!(!completed.needs_queue_interrupt());
        assert!(error.completed_tx_dispatch().is_none());
        assert_eq!(device.pending_guest_reset_packet_count(), 1);
        assert_eq!(read_vsock_rx_used_index(&memory), 0);
        assert_guest_reset_packet_header(
            read_vsock_packet_header(&memory, TEST_VSOCK_RX_BUFFER),
            42,
            52,
            4000,
        );
    }

    #[test]
    fn virtio_vsock_notifications_queue_reset_for_orphan_guest_rw() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");
        let packet = guest_rw_tx_packet(42, 52, 4000).header();

        activate_vsock_handler(&mut handler);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, packet);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("orphan guest RW should queue RST");

        assert_eq!(notification.guest_rw_dispatch().rw_packets(), 1);
        assert_eq!(notification.guest_rw_dispatch().forwarded_packets(), 0);
        assert_eq!(notification.guest_rw_dispatch().forwarded_bytes(), 0);
        assert_eq!(notification.guest_rw_dispatch().ignored_packets(), 0);
        assert_eq!(notification.guest_rw_dispatch().dropped_connections(), 1);
        assert_eq!(notification.guest_reset_dispatch().reset_candidates(), 1);
        assert_eq!(notification.guest_reset_dispatch().queued_resets(), 1);
        assert_eq!(
            handler
                .activation_handler()
                .pending_guest_reset_packet_count(),
            1
        );
        let rx = notification
            .rx_queue_dispatch()
            .expect("pending guest RST should attempt RX dispatch");
        assert_eq!(rx.processed_buffers(), 0);
        assert_eq!(rx.delivered_reset_packets(), 0);
    }

    #[test]
    fn virtio_vsock_notifications_dispatch_empty_tx_queue_without_interrupt() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("empty TX queue notification should dispatch");

        assert_eq!(
            notification.drained_notifications(),
            &[VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        assert!(!notification.needs_queue_interrupt());
        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(dispatch.processed_packets(), 0);
        assert_eq!(dispatch.successful_packets(), 0);
        assert_eq!(dispatch.parse_failures(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should remain active");
        assert_eq!(active_tx_queue.available_ring().next_avail(), 0);
        assert_eq!(active_tx_queue.used_ring().next_used(), 0);
        assert_eq!(read_vsock_tx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_vsock_notifications_complete_malformed_tx_packet_and_mark_interrupt() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32 - 1,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let notification = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect("malformed TX packet notification should dispatch");

        assert!(notification.needs_queue_interrupt());
        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(dispatch.processed_packets(), 1);
        assert_eq!(dispatch.successful_packets(), 0);
        assert_eq!(dispatch.parse_failures(), 1);
        assert!(matches!(
            dispatch.first_parse_failure(),
            Some(VirtioVsockTxPacketParseError::HeaderTooShort {
                descriptor_head: 0,
                actual: 43,
                min: VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64,
            })
        ));
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        assert_eq!(read_vsock_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_notifications_preserve_partial_tx_error_and_mark_interrupt() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");
        let header = test_vsock_packet_header().with_payload_len(0);

        activate_vsock_handler(&mut handler);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0, VIRTIO_VSOCK_QUEUE_SIZE]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let error = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect_err("invalid second TX head should fail after partial dispatch");

        assert!(matches!(
            error,
            super::VirtioVsockDeviceNotificationError::TxQueueDispatch { .. }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        let completed = error
            .completed_tx_dispatch()
            .expect("partial TX dispatch metadata should be preserved");
        assert_eq!(completed.processed_packets(), 1);
        assert_eq!(completed.successful_packets(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should remain active");
        assert_eq!(active_tx_queue.available_ring().next_avail(), 1);
        assert_eq!(active_tx_queue.used_ring().next_used(), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        assert_eq!(read_vsock_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_vsock_notifications_preserve_completed_rx_when_mixed_tx_fails() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");
        let header = test_vsock_packet_header().with_payload_len(0);

        activate_vsock_handler(&mut handler);
        let (accepted, _client, request) =
            accepted_host_connection_with_request("notify-mixed-error", 4004);
        let key = handler
            .activation_handler_mut()
            .insert_accepted_host_connection(accepted, request)
            .expect("host connection should insert");
        write_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_RX_BUFFER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_rx_available_heads(&mut memory, &[0]);
        write_vsock_packet_header(&mut memory, TEST_VSOCK_HEADER, header);
        write_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_vsock_tx_available_heads(&mut memory, &[0, VIRTIO_VSOCK_QUEUE_SIZE]);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let error = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect_err("invalid second TX head should fail after RX dispatch");

        assert!(matches!(
            error,
            super::VirtioVsockDeviceNotificationError::TxQueueDispatch { .. }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        let rx = error
            .completed_rx_dispatch()
            .expect("completed RX dispatch should be preserved");
        assert_eq!(rx.processed_buffers(), 1);
        assert_eq!(rx.delivered_requests(), 1);
        assert!(rx.needs_queue_interrupt());
        let tx = error
            .completed_tx_dispatch()
            .expect("completed TX dispatch should be preserved");
        assert_eq!(tx.processed_packets(), 1);
        assert_eq!(tx.successful_packets(), 1);
        assert!(tx.needs_queue_interrupt());
        assert!(
            !handler
                .activation_handler()
                .has_pending_host_request_packet(key)
        );
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_vsock_rx_used_index(&memory), 1);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
    }

    #[test]
    fn virtio_vsock_device_rejects_driver_ok_before_queues_are_ready() {
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");
        advance_handler_to_features_ok(&mut handler);

        let err = handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect_err("unready queues should reject DRIVER_OK");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::DeviceActivation { .. }
        ));
        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
    }

    #[test]
    fn virtio_vsock_device_rejects_unexpected_queue_count() {
        let registers = vsock_device_registers();

        for queue_count in [2, 4] {
            let queues = vsock_queue_registers_with_count(queue_count);
            let activation = VirtioMmioDeviceActivation::new(&registers, &queues);
            let mut device = VirtioVsockDevice::new();

            let err = device
                .activate_vsock(activation)
                .expect_err("unexpected queue count should fail activation");

            assert_eq!(
                err.to_string(),
                format!("virtio-vsock expected 3 queues, got {queue_count}")
            );
            assert!(matches!(
                err,
                VirtioVsockDeviceActivationError::QueueCountMismatch {
                    expected: 3,
                    got
                } if got == queue_count
            ));
            assert!(!device.is_activated());
        }
    }
}
