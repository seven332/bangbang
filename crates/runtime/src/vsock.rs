//! Backend-neutral vsock configuration model.

use std::collections::{BTreeSet, HashMap, HashSet, hash_map::Entry};
use std::fmt;
use std::fs;
use std::io::Read as _;
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
pub const VSOCK_HOST_LOCAL_PORT_BASE: u32 = 1_u32 << 30;
pub const VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE: u32 = 1_u32 << 31;
pub const VSOCK_HOST_LOCAL_PORT_CAPACITY: u32 =
    VSOCK_HOST_LOCAL_PORT_END_EXCLUSIVE - VSOCK_HOST_LOCAL_PORT_BASE;

const VIRTIO_VSOCK_RX_QUEUE_INDEX_U32: u32 = 0;
const VIRTIO_VSOCK_TX_QUEUE_INDEX_U32: u32 = 1;
const VIRTIO_VSOCK_EVENT_QUEUE_INDEX_U32: u32 = 2;
const VIRTIO_VSOCK_PACKET_HEADER_SIZE_U64: u64 = VIRTIO_VSOCK_PACKET_HEADER_SIZE as u64;
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
}

impl VsockHostConnection {
    fn from_accepted(accepted: VsockHostAcceptedConnection) -> Self {
        Self {
            accepted,
            request_packet_pending: true,
        }
    }

    pub fn stream(&self) -> &UnixStream {
        self.accepted.stream()
    }

    #[cfg(test)]
    const fn has_pending_request_packet(&self) -> bool {
        self.request_packet_pending
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
}

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

    pub fn take_pending_request_packet_header(
        &mut self,
        key: VsockHostConnectionKey,
        guest_cid: u32,
    ) -> Option<VirtioVsockPacketHeader> {
        self.connections
            .get_mut(&key)?
            .take_pending_request_packet_header(key, guest_cid)
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
pub struct VirtioVsockRxQueue {
    queue_state: VirtioMmioQueueState,
}

impl VirtioVsockRxQueue {
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

#[derive(Debug, Default)]
pub struct VirtioVsockDevice {
    active_rx_queue: Option<VirtioVsockRxQueue>,
    active_tx_queue: Option<VirtioVsockTxQueue>,
    active_event_queue: Option<VirtioVsockEventQueue>,
    host_socket_owner: Option<VsockHostSocketOwner>,
}

impl VirtioVsockDevice {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_host_socket_owner(host_socket_owner: VsockHostSocketOwner) -> Self {
        Self {
            host_socket_owner: Some(host_socket_owner),
            ..Self::default()
        }
    }

    pub fn is_activated(&self) -> bool {
        self.active_rx_queue.is_some()
            && self.active_tx_queue.is_some()
            && self.active_event_queue.is_some()
    }

    pub fn active_rx_queue(&self) -> Option<VirtioMmioQueueState> {
        self.active_rx_queue.map(VirtioVsockRxQueue::queue_state)
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
    }

    fn dispatch_drained_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
    ) -> Result<VirtioVsockDeviceNotificationDispatch, VirtioVsockDeviceNotificationError> {
        if drained_notifications.is_empty() {
            return Ok(VirtioVsockDeviceNotificationDispatch::new(
                drained_notifications,
                None,
            ));
        }

        if !self.is_activated() {
            return Err(VirtioVsockDeviceNotificationError::Inactive {
                drained_notifications,
            });
        }

        if let Some(queue_index) = drained_notifications
            .iter()
            .copied()
            .find(|queue_index| *queue_index != VIRTIO_VSOCK_TX_QUEUE_INDEX)
        {
            return Err(VirtioVsockDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        let dispatch_tx = drained_notifications
            .iter()
            .copied()
            .any(|queue_index| queue_index == VIRTIO_VSOCK_TX_QUEUE_INDEX);

        let tx_queue_dispatch = if dispatch_tx {
            let Some(queue) = self.active_tx_queue.as_mut() else {
                return Err(VirtioVsockDeviceNotificationError::Inactive {
                    drained_notifications,
                });
            };

            match queue.dispatch(memory) {
                Ok(dispatch) => Some(dispatch),
                Err(source) => {
                    return Err(VirtioVsockDeviceNotificationError::TxQueueDispatch {
                        drained_notifications,
                        source,
                    });
                }
            }
        } else {
            None
        };

        Ok(VirtioVsockDeviceNotificationDispatch::new(
            drained_notifications,
            tx_queue_dispatch,
        ))
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

#[derive(Debug)]
pub struct VirtioVsockDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
    tx_queue_dispatch: Option<VirtioVsockTxQueueDispatch>,
}

impl VirtioVsockDeviceNotificationDispatch {
    const fn new(
        drained_notifications: Vec<usize>,
        tx_queue_dispatch: Option<VirtioVsockTxQueueDispatch>,
    ) -> Self {
        Self {
            drained_notifications,
            tx_queue_dispatch,
        }
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub const fn tx_queue_dispatch(&self) -> Option<&VirtioVsockTxQueueDispatch> {
        self.tx_queue_dispatch.as_ref()
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.tx_queue_dispatch
            .as_ref()
            .is_some_and(VirtioVsockTxQueueDispatch::needs_queue_interrupt)
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
        source: VirtioVsockTxQueueDispatchError,
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
            } => drained_notifications,
        }
    }

    pub const fn completed_tx_dispatch(&self) -> Option<&VirtioVsockTxQueueDispatch> {
        match self {
            Self::TxQueueDispatch { source, .. } => source.completed_dispatch(),
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
        }
    }
}

impl std::error::Error for VirtioVsockDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TxQueueDispatch { source, .. } => Some(source),
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
            Err(error) => error
                .completed_tx_dispatch()
                .is_some_and(VirtioVsockTxQueueDispatch::needs_queue_interrupt),
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
        Self::from_config_with_device(config, VirtioVsockDevice::new())
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
            VirtioVsockDevice::with_host_socket_owner(owner),
        ))
    }

    fn from_config_with_device(config: &VsockConfig, device: VirtioVsockDevice) -> Self {
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
        VirtioVsockDevice::new(),
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
        VirtioMmioRegisterHandlerError,
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
        VirtioVsockMmioHandler, VirtioVsockPacketHeader, VirtioVsockPacketLengthError,
        VirtioVsockQueueBuildError, VirtioVsockTxPacket, VirtioVsockTxPacketParseError,
        VirtioVsockTxQueue, VirtioVsockTxQueueDispatchError, VsockConfigError, VsockConfigInput,
        VsockHostConnectHandshakeError, VsockHostConnectRequest, VsockHostConnectRequestError,
        VsockHostConnectionKey, VsockHostConnectionTable, VsockHostConnectionTableError,
        VsockHostLocalPort, VsockHostLocalPortAllocator, VsockHostLocalPortAllocatorError,
        VsockHostLocalPortError, VsockHostSocketAcceptError, VsockHostSocketOwner,
        VsockHostSocketOwnerError, VsockMmioDevice, VsockMmioLayout, VsockMmioRegistrationError,
        is_transient_host_socket_accept_error, is_transient_host_socket_read_error,
        parse_vsock_host_connect_request, virtio_vsock_mmio_handler,
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

        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_INIT)
            .expect("status reset should succeed");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
        assert!(handler.activation_handler().active_rx_queue().is_none());
        assert!(handler.activation_handler().active_tx_queue().is_none());
        assert!(handler.activation_handler().active_event_queue().is_none());
    }

    #[test]
    fn virtio_vsock_notifications_without_pending_work_are_noop() {
        let mut memory = vsock_tx_memory();
        let mut device = VirtioVsockDevice::new();

        let dispatch = device
            .dispatch_drained_queue_notifications(&mut memory, Vec::new())
            .expect("empty notification drain should be a no-op");

        assert_eq!(dispatch.drained_notifications(), &[]);
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
        assert!(error.source().is_none());
    }

    #[test]
    fn virtio_vsock_notifications_reject_rx_queue_without_dispatch() {
        let mut memory = vsock_tx_memory();
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");

        activate_vsock_handler(&mut handler);
        notify_vsock_queue(&mut handler, VIRTIO_VSOCK_RX_QUEUE_INDEX);

        let error = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect_err("RX notification should be unsupported");

        assert!(matches!(
            error,
            super::VirtioVsockDeviceNotificationError::UnsupportedQueue {
                queue_index: VIRTIO_VSOCK_RX_QUEUE_INDEX,
                ..
            }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX]
        );
        assert_eq!(
            error.to_string(),
            "virtio-vsock queue notification for unsupported queue 0"
        );
        assert!(error.completed_tx_dispatch().is_none());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should remain active");
        assert_eq!(active_tx_queue.available_ring().next_avail(), 0);
        assert_eq!(active_tx_queue.used_ring().next_used(), 0);
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
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn virtio_vsock_notifications_reject_mixed_unsupported_queue_without_tx_dispatch() {
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

        let error = handler
            .dispatch_vsock_queue_notifications(&mut memory)
            .expect_err("mixed unsupported notification should reject before TX dispatch");

        assert!(matches!(
            error,
            super::VirtioVsockDeviceNotificationError::UnsupportedQueue {
                queue_index: VIRTIO_VSOCK_RX_QUEUE_INDEX,
                ..
            }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        assert!(error.completed_tx_dispatch().is_none());
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
