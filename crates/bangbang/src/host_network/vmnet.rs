//! vmnet lifecycle boundary types for future macOS host networking.

use std::ffi::{CString, c_char, c_int, c_void};
use std::fmt;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};
use std::sync::mpsc;

use block2::{Block, RcBlock};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};

pub const VMNET_HOST_MODE_VALUE: u32 = 1000;
pub const VMNET_SHARED_MODE_VALUE: u32 = 1001;
pub const VMNET_BRIDGED_MODE_VALUE: u32 = 1002;

pub const VMNET_SUCCESS_VALUE: u32 = 1000;
pub const VMNET_FAILURE_VALUE: u32 = 1001;
pub const VMNET_MEM_FAILURE_VALUE: u32 = 1002;
pub const VMNET_INVALID_ARGUMENT_VALUE: u32 = 1003;
pub const VMNET_SETUP_INCOMPLETE_VALUE: u32 = 1004;
pub const VMNET_INVALID_ACCESS_VALUE: u32 = 1005;
pub const VMNET_PACKET_TOO_BIG_VALUE: u32 = 1006;
pub const VMNET_BUFFER_EXHAUSTED_VALUE: u32 = 1007;
pub const VMNET_TOO_MANY_PACKETS_VALUE: u32 = 1008;
pub const VMNET_SHARING_SERVICE_BUSY_VALUE: u32 = 1009;
pub const VMNET_NOT_AUTHORIZED_VALUE: u32 = 1010;

pub const VMNET_HOST_DEVICE_NAME_HOST: &str = "vmnet:host";
pub const VMNET_HOST_DEVICE_NAME_SHARED: &str = "vmnet:shared";
pub const VMNET_HOST_DEVICE_NAME_BRIDGED_PREFIX: &str = "vmnet:bridged:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetMode {
    Host,
    Shared,
    Bridged,
}

impl VmnetMode {
    pub const fn raw_value(self) -> u32 {
        match self {
            Self::Host => VMNET_HOST_MODE_VALUE,
            Self::Shared => VMNET_SHARED_MODE_VALUE,
            Self::Bridged => VMNET_BRIDGED_MODE_VALUE,
        }
    }
}

impl fmt::Display for VmnetMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Host => "host",
            Self::Shared => "shared",
            Self::Bridged => "bridged",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetStatus {
    Success,
    Failure,
    MemoryFailure,
    InvalidArgument,
    SetupIncomplete,
    InvalidAccess,
    PacketTooBig,
    BufferExhausted,
    TooManyPackets,
    SharingServiceBusy,
    NotAuthorized,
    Unknown(u32),
}

impl VmnetStatus {
    pub const fn from_raw(value: u32) -> Self {
        match value {
            VMNET_SUCCESS_VALUE => Self::Success,
            VMNET_FAILURE_VALUE => Self::Failure,
            VMNET_MEM_FAILURE_VALUE => Self::MemoryFailure,
            VMNET_INVALID_ARGUMENT_VALUE => Self::InvalidArgument,
            VMNET_SETUP_INCOMPLETE_VALUE => Self::SetupIncomplete,
            VMNET_INVALID_ACCESS_VALUE => Self::InvalidAccess,
            VMNET_PACKET_TOO_BIG_VALUE => Self::PacketTooBig,
            VMNET_BUFFER_EXHAUSTED_VALUE => Self::BufferExhausted,
            VMNET_TOO_MANY_PACKETS_VALUE => Self::TooManyPackets,
            VMNET_SHARING_SERVICE_BUSY_VALUE => Self::SharingServiceBusy,
            VMNET_NOT_AUTHORIZED_VALUE => Self::NotAuthorized,
            value => Self::Unknown(value),
        }
    }

    pub const fn raw_value(self) -> u32 {
        match self {
            Self::Success => VMNET_SUCCESS_VALUE,
            Self::Failure => VMNET_FAILURE_VALUE,
            Self::MemoryFailure => VMNET_MEM_FAILURE_VALUE,
            Self::InvalidArgument => VMNET_INVALID_ARGUMENT_VALUE,
            Self::SetupIncomplete => VMNET_SETUP_INCOMPLETE_VALUE,
            Self::InvalidAccess => VMNET_INVALID_ACCESS_VALUE,
            Self::PacketTooBig => VMNET_PACKET_TOO_BIG_VALUE,
            Self::BufferExhausted => VMNET_BUFFER_EXHAUSTED_VALUE,
            Self::TooManyPackets => VMNET_TOO_MANY_PACKETS_VALUE,
            Self::SharingServiceBusy => VMNET_SHARING_SERVICE_BUSY_VALUE,
            Self::NotAuthorized => VMNET_NOT_AUTHORIZED_VALUE,
            Self::Unknown(value) => value,
        }
    }

    const fn name(self) -> Option<&'static str> {
        match self {
            Self::Success => Some("VMNET_SUCCESS"),
            Self::Failure => Some("VMNET_FAILURE"),
            Self::MemoryFailure => Some("VMNET_MEM_FAILURE"),
            Self::InvalidArgument => Some("VMNET_INVALID_ARGUMENT"),
            Self::SetupIncomplete => Some("VMNET_SETUP_INCOMPLETE"),
            Self::InvalidAccess => Some("VMNET_INVALID_ACCESS"),
            Self::PacketTooBig => Some("VMNET_PACKET_TOO_BIG"),
            Self::BufferExhausted => Some("VMNET_BUFFER_EXHAUSTED"),
            Self::TooManyPackets => Some("VMNET_TOO_MANY_PACKETS"),
            Self::SharingServiceBusy => Some("VMNET_SHARING_SERVICE_BUSY"),
            Self::NotAuthorized => Some("VMNET_NOT_AUTHORIZED"),
            Self::Unknown(_) => None,
        }
    }
}

impl fmt::Display for VmnetStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => write!(f, "{name} (vmnet_return_t={})", self.raw_value()),
            None => write!(f, "vmnet_return_t={}", self.raw_value()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetOperation {
    StartInterface,
    StopInterface,
    ReadPackets,
    WritePackets,
}

impl fmt::Display for VmnetOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StartInterface => "vmnet_start_interface",
            Self::StopInterface => "vmnet_stop_interface",
            Self::ReadPackets => "vmnet_read",
            Self::WritePackets => "vmnet_write",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmnetError {
    operation: VmnetOperation,
    status: VmnetStatus,
}

impl VmnetError {
    pub const fn new(operation: VmnetOperation, status: VmnetStatus) -> Self {
        Self { operation, status }
    }

    pub const fn operation(&self) -> VmnetOperation {
        self.operation
    }

    pub const fn status(&self) -> VmnetStatus {
        self.status
    }
}

impl fmt::Display for VmnetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed with {}", self.operation, self.status)
    }
}

impl std::error::Error for VmnetError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmnetInterfaceConfig {
    mode: VmnetMode,
    bridged_interface_name: Option<String>,
}

impl VmnetInterfaceConfig {
    pub const fn host() -> Self {
        Self {
            mode: VmnetMode::Host,
            bridged_interface_name: None,
        }
    }

    pub const fn shared() -> Self {
        Self {
            mode: VmnetMode::Shared,
            bridged_interface_name: None,
        }
    }

    pub fn from_host_dev_name(host_dev_name: &str) -> Result<Self, VmnetHostDeviceNameConfigError> {
        match host_dev_name {
            VMNET_HOST_DEVICE_NAME_HOST => Ok(Self::host()),
            VMNET_HOST_DEVICE_NAME_SHARED => Ok(Self::shared()),
            name => match name.strip_prefix(VMNET_HOST_DEVICE_NAME_BRIDGED_PREFIX) {
                Some(interface_name) => Self::bridged(interface_name)
                    .map_err(|source| VmnetHostDeviceNameConfigError::BridgedInterface { source }),
                None => Err(VmnetHostDeviceNameConfigError::UnsupportedHostDeviceName),
            },
        }
    }

    pub fn bridged(interface_name: impl Into<String>) -> Result<Self, VmnetInterfaceConfigError> {
        let interface_name = interface_name.into();
        if interface_name.is_empty() {
            return Err(VmnetInterfaceConfigError::EmptyBridgedInterfaceName);
        }
        if interface_name.as_bytes().contains(&0) {
            return Err(VmnetInterfaceConfigError::InteriorNulInBridgedInterfaceName);
        }
        if interface_name.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(VmnetInterfaceConfigError::ControlCharacterInBridgedInterfaceName);
        }

        Ok(Self {
            mode: VmnetMode::Bridged,
            bridged_interface_name: Some(interface_name),
        })
    }

    pub const fn mode(&self) -> VmnetMode {
        self.mode
    }

    pub fn bridged_interface_name(&self) -> Option<&str> {
        self.bridged_interface_name.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmnetHostDeviceNameConfigError {
    UnsupportedHostDeviceName,
    BridgedInterface { source: VmnetInterfaceConfigError },
}

impl fmt::Display for VmnetHostDeviceNameConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHostDeviceName => f.write_str(
                "unsupported vmnet host_dev_name; expected vmnet:host, vmnet:shared, or vmnet:bridged:<interface>",
            ),
            Self::BridgedInterface { source } => {
                write!(f, "invalid vmnet bridged host_dev_name: {source}")
            }
        }
    }
}

impl std::error::Error for VmnetHostDeviceNameConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnsupportedHostDeviceName => None,
            Self::BridgedInterface { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetInterfaceConfigError {
    EmptyBridgedInterfaceName,
    InteriorNulInBridgedInterfaceName,
    ControlCharacterInBridgedInterfaceName,
}

impl fmt::Display for VmnetInterfaceConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBridgedInterfaceName => {
                f.write_str("vmnet bridged interface name must not be empty")
            }
            Self::InteriorNulInBridgedInterfaceName => {
                f.write_str("vmnet bridged interface name must not contain NUL bytes")
            }
            Self::ControlCharacterInBridgedInterfaceName => f.write_str(
                "vmnet bridged interface name must not contain ASCII control characters",
            ),
        }
    }
}

impl std::error::Error for VmnetInterfaceConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetInterfaceDescriptorError {
    CreateDictionaryFailed,
    InteriorNulInBridgedInterfaceName,
    MissingVmnetKey(&'static str),
}

impl fmt::Display for VmnetInterfaceDescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDictionaryFailed => {
                f.write_str("failed to create vmnet interface descriptor")
            }
            Self::InteriorNulInBridgedInterfaceName => {
                f.write_str("vmnet bridged interface name must not contain NUL bytes")
            }
            Self::MissingVmnetKey(key) => write!(f, "vmnet key symbol {key} is null"),
        }
    }
}

impl std::error::Error for VmnetInterfaceDescriptorError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmnetInterfaceStartError {
    Descriptor {
        source: VmnetInterfaceDescriptorError,
    },
    Start {
        source: VmnetError,
    },
}

impl fmt::Display for VmnetInterfaceStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Descriptor { source } => {
                write!(f, "failed to build vmnet interface descriptor: {source}")
            }
            Self::Start { source } => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for VmnetInterfaceStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Descriptor { source } => Some(source),
            Self::Start { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetPacketDescriptorError {
    EmptyPacketBuffer,
}

impl fmt::Display for VmnetPacketDescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPacketBuffer => {
                f.write_str("vmnet packet descriptor buffer must not be empty")
            }
        }
    }
}

impl std::error::Error for VmnetPacketDescriptorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetPacketCountExpectation {
    ZeroOrOne,
    One,
}

impl fmt::Display for VmnetPacketCountExpectation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ZeroOrOne => "0 or 1",
            Self::One => "1",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmnetPacketIoError {
    Vmnet {
        source: VmnetError,
    },
    InterfaceStopped,
    UnexpectedPacketCount {
        operation: VmnetOperation,
        expected: VmnetPacketCountExpectation,
        actual: i32,
    },
    ReadPacketSizeExceedsBuffer {
        packet_size: usize,
        buffer_len: usize,
    },
}

impl VmnetPacketIoError {
    const fn vmnet(operation: VmnetOperation, status: VmnetStatus) -> Self {
        Self::Vmnet {
            source: VmnetError::new(operation, status),
        }
    }
}

impl fmt::Display for VmnetPacketIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vmnet { source } => write!(f, "{source}"),
            Self::InterfaceStopped => f.write_str("vmnet interface is not started"),
            Self::UnexpectedPacketCount {
                operation,
                expected,
                actual,
            } => write!(
                f,
                "{operation} returned packet count {actual}, expected {expected}"
            ),
            Self::ReadPacketSizeExceedsBuffer {
                packet_size,
                buffer_len,
            } => write!(
                f,
                "vmnet_read returned packet size {packet_size}, larger than read buffer {buffer_len}"
            ),
        }
    }
}

impl std::error::Error for VmnetPacketIoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Vmnet { source } => Some(source),
            Self::InterfaceStopped
            | Self::UnexpectedPacketCount { .. }
            | Self::ReadPacketSizeExceedsBuffer { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct VmnetInterfaceDescriptor {
    dictionary: OwnedXpcObject,
}

impl VmnetInterfaceDescriptor {
    pub fn new(config: &VmnetInterfaceConfig) -> Result<Self, VmnetInterfaceDescriptorError> {
        let dictionary = OwnedXpcObject::dictionary()
            .ok_or(VmnetInterfaceDescriptorError::CreateDictionaryFailed)?;
        let operation_mode_key = vmnet_operation_mode_key()?;

        // SAFETY: `dictionary` owns a live XPC dictionary, `operation_mode_key`
        // is a non-null vmnet SDK key, and primitive uint64 insertion does not
        // borrow data after the call returns.
        unsafe {
            xpc::xpc_dictionary_set_uint64(
                dictionary.as_ptr(),
                operation_mode_key,
                u64::from(config.mode().raw_value()),
            );
        }

        if let Some(interface_name) = config.bridged_interface_name() {
            let interface_name = CString::new(interface_name)
                .map_err(|_| VmnetInterfaceDescriptorError::InteriorNulInBridgedInterfaceName)?;
            let shared_interface_name_key = vmnet_shared_interface_name_key()?;

            // SAFETY: `dictionary` owns a live XPC dictionary,
            // `shared_interface_name_key` is a non-null vmnet SDK key, and the
            // bridged interface name is a valid C string for the duration of the call.
            unsafe {
                xpc::xpc_dictionary_set_string(
                    dictionary.as_ptr(),
                    shared_interface_name_key,
                    interface_name.as_ptr(),
                );
            }
        }

        Ok(Self { dictionary })
    }

    pub fn as_raw_xpc_object(&self) -> *mut c_void {
        self.dictionary.as_ptr()
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct VmnetPacketDescriptor {
    vm_pkt_size: usize,
    vm_pkt_iov: *mut libc::iovec,
    vm_pkt_iovcnt: u32,
    vm_flags: u32,
}

impl VmnetPacketDescriptor {
    pub const fn packet_size(&self) -> usize {
        self.vm_pkt_size
    }

    pub const fn iov_count(&self) -> u32 {
        self.vm_pkt_iovcnt
    }

    pub const fn flags(&self) -> u32 {
        self.vm_flags
    }

    pub const fn iov_ptr(&self) -> *mut libc::iovec {
        self.vm_pkt_iov
    }
}

#[derive(Debug)]
pub struct VmnetWritePacket<'a> {
    descriptor: VmnetPacketDescriptor,
    iov: Box<libc::iovec>,
    _packet: PhantomData<&'a [u8]>,
}

impl<'a> VmnetWritePacket<'a> {
    pub fn new(packet: &'a [u8]) -> Result<Self, VmnetPacketDescriptorError> {
        if packet.is_empty() {
            return Err(VmnetPacketDescriptorError::EmptyPacketBuffer);
        }

        let mut iov = Box::new(libc::iovec {
            iov_base: packet.as_ptr().cast::<c_void>().cast_mut(),
            iov_len: packet.len(),
        });
        let descriptor = VmnetPacketDescriptor {
            vm_pkt_size: packet.len(),
            vm_pkt_iov: ptr::from_mut(iov.as_mut()),
            vm_pkt_iovcnt: 1,
            vm_flags: 0,
        };

        Ok(Self {
            descriptor,
            iov,
            _packet: PhantomData,
        })
    }

    pub const fn as_raw_descriptor(&self) -> &VmnetPacketDescriptor {
        &self.descriptor
    }

    pub fn as_mut_raw_descriptor(&mut self) -> &mut VmnetPacketDescriptor {
        &mut self.descriptor
    }

    pub fn iov(&self) -> &libc::iovec {
        &self.iov
    }
}

#[derive(Debug)]
pub struct VmnetReadPacket<'a> {
    descriptor: VmnetPacketDescriptor,
    iov: Box<libc::iovec>,
    _buffer: PhantomData<&'a mut [u8]>,
}

impl<'a> VmnetReadPacket<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Result<Self, VmnetPacketDescriptorError> {
        if buffer.is_empty() {
            return Err(VmnetPacketDescriptorError::EmptyPacketBuffer);
        }

        let mut iov = Box::new(libc::iovec {
            iov_base: buffer.as_mut_ptr().cast::<c_void>(),
            iov_len: buffer.len(),
        });
        let descriptor = VmnetPacketDescriptor {
            vm_pkt_size: buffer.len(),
            vm_pkt_iov: ptr::from_mut(iov.as_mut()),
            vm_pkt_iovcnt: 1,
            vm_flags: 0,
        };

        Ok(Self {
            descriptor,
            iov,
            _buffer: PhantomData,
        })
    }

    pub const fn as_raw_descriptor(&self) -> &VmnetPacketDescriptor {
        &self.descriptor
    }

    pub fn as_mut_raw_descriptor(&mut self) -> &mut VmnetPacketDescriptor {
        &mut self.descriptor
    }

    pub fn iov(&self) -> &libc::iovec {
        &self.iov
    }
}

#[derive(Debug)]
struct OwnedXpcObject {
    object: NonNull<c_void>,
}

impl OwnedXpcObject {
    fn dictionary() -> Option<Self> {
        // SAFETY: `xpc_dictionary_create` permits null key and value arrays
        // when `count` is zero, creating an empty retained dictionary object.
        let object = unsafe { xpc::xpc_dictionary_create(ptr::null(), ptr::null(), 0) };

        NonNull::new(object).map(|object| Self { object })
    }

    fn as_ptr(&self) -> xpc::XpcObject {
        self.object.as_ptr()
    }
}

impl Drop for OwnedXpcObject {
    fn drop(&mut self) {
        // SAFETY: `object` came from an XPC create function and this owner is
        // non-clone, so this is the single matching release.
        unsafe {
            xpc::xpc_release(self.object.as_ptr());
        }
    }
}

fn vmnet_operation_mode_key() -> Result<*const c_char, VmnetInterfaceDescriptorError> {
    // SAFETY: The symbol is provided by vmnet.framework when this macOS-gated
    // module is linked. The null check below prevents passing a null key to XPC.
    let key = unsafe { xpc::VMNET_OPERATION_MODE_KEY };
    if key.is_null() {
        Err(VmnetInterfaceDescriptorError::MissingVmnetKey(
            "vmnet_operation_mode_key",
        ))
    } else {
        Ok(key)
    }
}

fn vmnet_shared_interface_name_key() -> Result<*const c_char, VmnetInterfaceDescriptorError> {
    // SAFETY: The symbol is provided by vmnet.framework when this macOS-gated
    // module is linked. The null check below prevents passing a null key to XPC.
    let key = unsafe { xpc::VMNET_SHARED_INTERFACE_NAME_KEY };
    if key.is_null() {
        Err(VmnetInterfaceDescriptorError::MissingVmnetKey(
            "vmnet_shared_interface_name_key",
        ))
    } else {
        Ok(key)
    }
}

mod xpc {
    use std::ffi::{c_char, c_void};

    pub type XpcObject = *mut c_void;

    unsafe extern "C" {
        pub fn xpc_dictionary_create(
            keys: *const *const c_char,
            values: *const XpcObject,
            count: usize,
        ) -> XpcObject;
        pub fn xpc_dictionary_set_uint64(xdict: XpcObject, key: *const c_char, value: u64);
        pub fn xpc_dictionary_set_string(
            xdict: XpcObject,
            key: *const c_char,
            string: *const c_char,
        );
        #[cfg(test)]
        pub fn xpc_dictionary_get_uint64(xdict: XpcObject, key: *const c_char) -> u64;
        #[cfg(test)]
        pub fn xpc_dictionary_get_string(xdict: XpcObject, key: *const c_char) -> *const c_char;
        pub fn xpc_release(object: XpcObject);
    }

    #[link(name = "vmnet", kind = "framework")]
    unsafe extern "C" {
        #[link_name = "vmnet_operation_mode_key"]
        pub static VMNET_OPERATION_MODE_KEY: *const c_char;
        #[link_name = "vmnet_shared_interface_name_key"]
        pub static VMNET_SHARED_INTERFACE_NAME_KEY: *const c_char;
    }
}

mod vmnet_sys {
    use std::ffi::{c_int, c_void};

    use block2::Block;
    use dispatch2::DispatchQueue;

    use super::VmnetPacketDescriptor;

    #[link(name = "vmnet", kind = "framework")]
    unsafe extern "C" {
        pub fn vmnet_start_interface(
            interface_desc: *mut c_void,
            queue: &DispatchQueue,
            handler: &Block<dyn Fn(u32, *mut c_void)>,
        ) -> *mut c_void;
        pub fn vmnet_stop_interface(
            interface: *mut c_void,
            queue: &DispatchQueue,
            handler: &Block<dyn Fn(u32)>,
        ) -> u32;
        pub fn vmnet_read(
            interface: *mut c_void,
            packets: *mut VmnetPacketDescriptor,
            packet_count: *mut c_int,
        ) -> u32;
        pub fn vmnet_write(
            interface: *mut c_void,
            packets: *mut VmnetPacketDescriptor,
            packet_count: *mut c_int,
        ) -> u32;
    }
}

pub trait VmnetInterfaceBackend: fmt::Debug + Send + 'static {
    type Interface: fmt::Debug + Send + 'static;

    fn build_interface_descriptor(
        &mut self,
        config: &VmnetInterfaceConfig,
    ) -> Result<VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError> {
        VmnetInterfaceDescriptor::new(config)
    }

    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
    ) -> Result<Self::Interface, VmnetError>;

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError>;
}

pub trait VmnetPacketIoBackend: fmt::Debug + Send + 'static {
    type Interface: fmt::Debug + Send + 'static;

    fn read_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError>;

    fn write_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError>;
}

trait VmnetSystemApi: fmt::Debug + Send + 'static {
    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
        queue: &DispatchQueue,
        completion: &Block<dyn Fn(u32, *mut c_void)>,
    ) -> Option<NonNull<c_void>>;

    fn stop_interface(
        &mut self,
        interface: NonNull<c_void>,
        queue: &DispatchQueue,
        completion: &Block<dyn Fn(u32)>,
    ) -> VmnetStatus;

    fn read_packets(
        &mut self,
        interface: NonNull<c_void>,
        packets: NonNull<VmnetPacketDescriptor>,
        packet_count: &mut c_int,
    ) -> VmnetStatus;

    fn write_packets(
        &mut self,
        interface: NonNull<c_void>,
        packets: NonNull<VmnetPacketDescriptor>,
        packet_count: &mut c_int,
    ) -> VmnetStatus;
}

#[derive(Debug, Default)]
struct SystemVmnetApi;

impl VmnetSystemApi for SystemVmnetApi {
    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
        queue: &DispatchQueue,
        completion: &Block<dyn Fn(u32, *mut c_void)>,
    ) -> Option<NonNull<c_void>> {
        // SAFETY: `descriptor` owns a live XPC dictionary, `queue` is a live
        // dispatch queue, and `completion` is a valid Objective-C block. The
        // vmnet async completion API owns any later callback use by copying the
        // block before returning.
        let interface = unsafe {
            vmnet_sys::vmnet_start_interface(descriptor.as_raw_xpc_object(), queue, completion)
        };

        NonNull::new(interface)
    }

    fn stop_interface(
        &mut self,
        interface: NonNull<c_void>,
        queue: &DispatchQueue,
        completion: &Block<dyn Fn(u32)>,
    ) -> VmnetStatus {
        // SAFETY: `interface` is an opaque handle returned by
        // `vmnet_start_interface`, `queue` is a live dispatch queue, and
        // `completion` is a valid Objective-C block. The vmnet async completion
        // API owns any later callback use by copying the block before returning.
        let status =
            unsafe { vmnet_sys::vmnet_stop_interface(interface.as_ptr(), queue, completion) };

        VmnetStatus::from_raw(status)
    }

    fn read_packets(
        &mut self,
        interface: NonNull<c_void>,
        packets: NonNull<VmnetPacketDescriptor>,
        packet_count: &mut c_int,
    ) -> VmnetStatus {
        // SAFETY: `interface` is an opaque handle returned by
        // `vmnet_start_interface`, `packets` points to a live packet descriptor
        // for one element, and `packet_count` is a valid mutable pointer for
        // vmnet.framework to read and update during the synchronous call.
        let status = unsafe {
            vmnet_sys::vmnet_read(
                interface.as_ptr(),
                packets.as_ptr(),
                ptr::from_mut(packet_count),
            )
        };

        VmnetStatus::from_raw(status)
    }

    fn write_packets(
        &mut self,
        interface: NonNull<c_void>,
        packets: NonNull<VmnetPacketDescriptor>,
        packet_count: &mut c_int,
    ) -> VmnetStatus {
        // SAFETY: `interface` is an opaque handle returned by
        // `vmnet_start_interface`, `packets` points to a live packet descriptor
        // for one element, and `packet_count` is a valid mutable pointer for
        // vmnet.framework to read and update during the synchronous call.
        let status = unsafe {
            vmnet_sys::vmnet_write(
                interface.as_ptr(),
                packets.as_ptr(),
                ptr::from_mut(packet_count),
            )
        };

        VmnetStatus::from_raw(status)
    }
}

#[derive(Debug)]
pub struct SystemVmnetInterface {
    interface: NonNull<c_void>,
}

// SAFETY: `interface` is an opaque vmnet.framework handle. Moving the owner
// between threads does not dereference it, and lifecycle operations still go
// through vmnet.framework with an explicit dispatch queue.
unsafe impl Send for SystemVmnetInterface {}

impl SystemVmnetInterface {
    const fn new(interface: NonNull<c_void>) -> Self {
        Self { interface }
    }

    pub const fn as_raw_interface(&self) -> NonNull<c_void> {
        self.interface
    }
}

#[derive(Debug)]
pub struct SystemVmnetInterfaceBackend {
    inner: SystemVmnetInterfaceBackendWithApi<SystemVmnetApi>,
}

impl Default for SystemVmnetInterfaceBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemVmnetInterfaceBackend {
    pub fn new() -> Self {
        Self {
            inner: SystemVmnetInterfaceBackendWithApi::with_api(SystemVmnetApi),
        }
    }
}

impl VmnetInterfaceBackend for SystemVmnetInterfaceBackend {
    type Interface = SystemVmnetInterface;

    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
    ) -> Result<Self::Interface, VmnetError> {
        self.inner.start_interface(descriptor)
    }

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
        self.inner.stop_interface(interface)
    }
}

impl VmnetPacketIoBackend for SystemVmnetInterfaceBackend {
    type Interface = SystemVmnetInterface;

    fn read_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError> {
        self.inner.read_packet(interface, packet)
    }

    fn write_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError> {
        self.inner.write_packet(interface, packet)
    }
}

#[derive(Debug)]
struct SystemVmnetInterfaceBackendWithApi<A> {
    api: A,
    queue: DispatchRetained<DispatchQueue>,
}

impl<A> SystemVmnetInterfaceBackendWithApi<A> {
    fn with_api(api: A) -> Self {
        Self {
            api,
            queue: DispatchQueue::new(
                "com.github.seven332.bangbang.vmnet",
                DispatchQueueAttr::SERIAL,
            ),
        }
    }
}

impl<A> VmnetInterfaceBackend for SystemVmnetInterfaceBackendWithApi<A>
where
    A: VmnetSystemApi,
{
    type Interface = SystemVmnetInterface;

    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
    ) -> Result<Self::Interface, VmnetError> {
        let (sender, receiver) = mpsc::channel();
        let completion = RcBlock::new(move |status: u32, _interface_param: *mut c_void| {
            let _ = sender.send(VmnetStatus::from_raw(status));
        });
        let Some(interface) = self
            .api
            .start_interface(descriptor, &self.queue, &completion)
        else {
            let status = receiver.try_recv().unwrap_or(VmnetStatus::Failure);
            let status = if status == VmnetStatus::Success {
                VmnetStatus::Failure
            } else {
                status
            };

            return Err(VmnetError::new(VmnetOperation::StartInterface, status));
        };
        let status = wait_for_vmnet_completion(receiver, VmnetOperation::StartInterface)?;
        if status == VmnetStatus::Success {
            return Ok(SystemVmnetInterface::new(interface));
        }

        let mut interface = SystemVmnetInterface::new(interface);
        match self.stop_interface(&mut interface) {
            Ok(()) | Err(_) => {}
        }
        Err(VmnetError::new(VmnetOperation::StartInterface, status))
    }

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
        let (sender, receiver) = mpsc::channel();
        let completion = RcBlock::new(move |status: u32| {
            let _ = sender.send(VmnetStatus::from_raw(status));
        });
        let schedule_status =
            self.api
                .stop_interface(interface.as_raw_interface(), &self.queue, &completion);
        if schedule_status != VmnetStatus::Success {
            return Err(VmnetError::new(
                VmnetOperation::StopInterface,
                schedule_status,
            ));
        }

        let status = wait_for_vmnet_completion(receiver, VmnetOperation::StopInterface)?;
        if status == VmnetStatus::Success {
            Ok(())
        } else {
            Err(VmnetError::new(VmnetOperation::StopInterface, status))
        }
    }
}

impl<A> VmnetPacketIoBackend for SystemVmnetInterfaceBackendWithApi<A>
where
    A: VmnetSystemApi,
{
    type Interface = SystemVmnetInterface;

    fn read_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError> {
        let mut packet_count = 1;
        let status = self.api.read_packets(
            interface.as_raw_interface(),
            NonNull::from(packet.as_mut_raw_descriptor()),
            &mut packet_count,
        );

        if status != VmnetStatus::Success {
            return Err(VmnetPacketIoError::vmnet(
                VmnetOperation::ReadPackets,
                status,
            ));
        }

        match packet_count {
            0 => Ok(None),
            1 => {
                let packet_size = packet.as_raw_descriptor().packet_size();
                let buffer_len = packet.iov().iov_len;
                if packet_size > buffer_len {
                    return Err(VmnetPacketIoError::ReadPacketSizeExceedsBuffer {
                        packet_size,
                        buffer_len,
                    });
                }

                Ok(Some(packet_size))
            }
            actual => Err(VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::ReadPackets,
                expected: VmnetPacketCountExpectation::ZeroOrOne,
                actual,
            }),
        }
    }

    fn write_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError> {
        let mut packet_count = 1;
        let status = self.api.write_packets(
            interface.as_raw_interface(),
            NonNull::from(packet.as_mut_raw_descriptor()),
            &mut packet_count,
        );

        if status != VmnetStatus::Success {
            return Err(VmnetPacketIoError::vmnet(
                VmnetOperation::WritePackets,
                status,
            ));
        }

        if packet_count == 1 {
            Ok(())
        } else {
            Err(VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::WritePackets,
                expected: VmnetPacketCountExpectation::One,
                actual: packet_count,
            })
        }
    }
}

fn wait_for_vmnet_completion(
    receiver: mpsc::Receiver<VmnetStatus>,
    operation: VmnetOperation,
) -> Result<VmnetStatus, VmnetError> {
    receiver
        .recv()
        .map_err(|_| VmnetError::new(operation, VmnetStatus::Failure))
}

#[derive(Debug)]
pub struct StartedVmnetInterface<B>
where
    B: VmnetInterfaceBackend,
{
    backend: B,
    interface: Option<B::Interface>,
}

impl<B> StartedVmnetInterface<B>
where
    B: VmnetInterfaceBackend,
{
    pub fn start(
        mut backend: B,
        config: &VmnetInterfaceConfig,
    ) -> Result<Self, VmnetInterfaceStartError> {
        let descriptor = backend
            .build_interface_descriptor(config)
            .map_err(|source| VmnetInterfaceStartError::Descriptor { source })?;
        let interface = backend
            .start_interface(&descriptor)
            .map_err(|source| VmnetInterfaceStartError::Start { source })?;

        Ok(Self {
            backend,
            interface: Some(interface),
        })
    }

    pub const fn is_started(&self) -> bool {
        self.interface.is_some()
    }

    pub fn stop(&mut self) -> Result<(), VmnetError> {
        if let Some(interface) = self.interface.as_mut() {
            self.backend.stop_interface(interface)?;
            self.interface = None;
        }

        Ok(())
    }
}

impl<B> Drop for StartedVmnetInterface<B>
where
    B: VmnetInterfaceBackend,
{
    fn drop(&mut self) {
        match self.stop() {
            Ok(()) | Err(_) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StartedVmnetPacketIoInterface;

#[derive(Debug)]
pub struct StartedVmnetPacketIoBackend<B>
where
    B: VmnetInterfaceBackend
        + VmnetPacketIoBackend<Interface = <B as VmnetInterfaceBackend>::Interface>,
{
    started: StartedVmnetInterface<B>,
}

impl<B> StartedVmnetPacketIoBackend<B>
where
    B: VmnetInterfaceBackend
        + VmnetPacketIoBackend<Interface = <B as VmnetInterfaceBackend>::Interface>,
{
    pub fn start(
        backend: B,
        config: &VmnetInterfaceConfig,
    ) -> Result<(Self, StartedVmnetPacketIoInterface), VmnetInterfaceStartError> {
        Ok((
            Self {
                started: StartedVmnetInterface::start(backend, config)?,
            },
            StartedVmnetPacketIoInterface,
        ))
    }

    pub const fn is_started(&self) -> bool {
        self.started.is_started()
    }

    pub fn stop(&mut self) -> Result<(), VmnetError> {
        self.started.stop()
    }
}

impl<B> VmnetPacketIoBackend for StartedVmnetPacketIoBackend<B>
where
    B: VmnetInterfaceBackend
        + VmnetPacketIoBackend<Interface = <B as VmnetInterfaceBackend>::Interface>,
{
    type Interface = StartedVmnetPacketIoInterface;

    fn read_packet(
        &mut self,
        _interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError> {
        let Some(interface) = self.started.interface.as_mut() else {
            return Err(VmnetPacketIoError::InterfaceStopped);
        };

        self.started.backend.read_packet(interface, packet)
    }

    fn write_packet(
        &mut self,
        _interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError> {
        let Some(interface) = self.started.interface.as_mut() else {
            return Err(VmnetPacketIoError::InterfaceStopped);
        };

        self.started.backend.write_packet(interface, packet)
    }
}

pub trait VmnetInterfaceLifecycle: fmt::Debug + Send + 'static {
    type Interface: fmt::Debug + Send + 'static;

    fn start_interface(
        &mut self,
        config: &VmnetInterfaceConfig,
    ) -> Result<Self::Interface, VmnetError>;

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError>;
}

#[derive(Debug)]
pub struct OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    lifecycle: L,
    interface: Option<L::Interface>,
}

impl<L> OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    pub fn start(mut lifecycle: L, config: &VmnetInterfaceConfig) -> Result<Self, VmnetError> {
        let interface = lifecycle.start_interface(config)?;

        Ok(Self {
            lifecycle,
            interface: Some(interface),
        })
    }

    pub const fn is_started(&self) -> bool {
        self.interface.is_some()
    }

    pub fn stop(&mut self) -> Result<(), VmnetError> {
        if let Some(interface) = self.interface.as_mut() {
            self.lifecycle.stop_interface(interface)?;
            self.interface = None;
        }

        Ok(())
    }
}

impl<L> Drop for OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    fn drop(&mut self) {
        match self.stop() {
            Ok(()) | Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::error::Error;
    use std::ffi::{CStr, c_int, c_void};
    use std::mem::{align_of, offset_of, size_of};
    use std::ptr::{self, NonNull};
    use std::sync::{Arc, Mutex};

    use bangbang_runtime::network::VirtioNetworkRxPacketSource;
    use block2::Block;
    use dispatch2::DispatchQueue;

    use crate::host_network::virtio_vmnet::VmnetVirtioNetworkPacketIo;

    use super::{
        OwnedVmnetInterface, StartedVmnetInterface, StartedVmnetPacketIoBackend,
        VMNET_BRIDGED_MODE_VALUE, VMNET_HOST_MODE_VALUE, VMNET_SHARED_MODE_VALUE, VmnetError,
        VmnetHostDeviceNameConfigError, VmnetInterfaceBackend, VmnetInterfaceConfig,
        VmnetInterfaceConfigError, VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError,
        VmnetInterfaceLifecycle, VmnetInterfaceStartError, VmnetMode, VmnetOperation,
        VmnetPacketCountExpectation, VmnetPacketDescriptor, VmnetPacketDescriptorError,
        VmnetPacketIoBackend, VmnetPacketIoError, VmnetReadPacket, VmnetStatus, VmnetSystemApi,
        VmnetWritePacket,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedVmnetInterface {
        id: u64,
    }

    #[derive(Debug, Clone)]
    struct RecordingVmnetLifecycle {
        events: Arc<Mutex<Vec<String>>>,
        start_status: Option<VmnetStatus>,
        stop_status: Option<VmnetStatus>,
    }

    impl RecordingVmnetLifecycle {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                start_status: None,
                stop_status: None,
            }
        }

        fn with_start_status(mut self, status: VmnetStatus) -> Self {
            self.start_status = Some(status);
            self
        }

        fn with_stop_status(mut self, status: VmnetStatus) -> Self {
            self.stop_status = Some(status);
            self
        }

        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.events)
        }
    }

    impl VmnetInterfaceLifecycle for RecordingVmnetLifecycle {
        type Interface = RecordedVmnetInterface;

        fn start_interface(
            &mut self,
            config: &VmnetInterfaceConfig,
        ) -> Result<Self::Interface, VmnetError> {
            push_event(&self.events, format!("start:{}", config.mode()));
            if let Some(status) = self.start_status {
                return Err(VmnetError::new(VmnetOperation::StartInterface, status));
            }

            Ok(RecordedVmnetInterface { id: 7 })
        }

        fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
            push_event(&self.events, format!("stop:{}", interface.id));
            if let Some(status) = self.stop_status {
                return Err(VmnetError::new(VmnetOperation::StopInterface, status));
            }

            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    struct RecordingVmnetBackend {
        events: Arc<Mutex<Vec<String>>>,
        descriptor_error: Option<VmnetInterfaceDescriptorError>,
        start_status: Option<VmnetStatus>,
        stop_statuses: VecDeque<VmnetStatus>,
        read_result: Result<Option<usize>, VmnetPacketIoError>,
        write_result: Result<(), VmnetPacketIoError>,
    }

    impl RecordingVmnetBackend {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                descriptor_error: None,
                start_status: None,
                stop_statuses: VecDeque::new(),
                read_result: Ok(None),
                write_result: Ok(()),
            }
        }

        fn with_descriptor_error(mut self, error: VmnetInterfaceDescriptorError) -> Self {
            self.descriptor_error = Some(error);
            self
        }

        fn with_start_status(mut self, status: VmnetStatus) -> Self {
            self.start_status = Some(status);
            self
        }

        fn with_stop_status(mut self, status: VmnetStatus) -> Self {
            self.stop_statuses.push_back(status);
            self
        }

        fn with_read_result(mut self, result: Result<Option<usize>, VmnetPacketIoError>) -> Self {
            self.read_result = result;
            self
        }

        fn with_write_result(mut self, result: Result<(), VmnetPacketIoError>) -> Self {
            self.write_result = result;
            self
        }

        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.events)
        }
    }

    impl VmnetInterfaceBackend for RecordingVmnetBackend {
        type Interface = RecordedVmnetInterface;

        fn build_interface_descriptor(
            &mut self,
            config: &VmnetInterfaceConfig,
        ) -> Result<VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError> {
            push_event(&self.events, format!("descriptor:{}", config.mode()));
            if let Some(error) = self.descriptor_error {
                return Err(error);
            }

            VmnetInterfaceDescriptor::new(config)
        }

        fn start_interface(
            &mut self,
            descriptor: &VmnetInterfaceDescriptor,
        ) -> Result<Self::Interface, VmnetError> {
            push_event(
                &self.events,
                format!("start:{}", descriptor_mode(descriptor)),
            );
            if let Some(status) = self.start_status {
                return Err(VmnetError::new(VmnetOperation::StartInterface, status));
            }

            Ok(RecordedVmnetInterface { id: 9 })
        }

        fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
            push_event(&self.events, format!("stop:{}", interface.id));
            if let Some(status) = self.stop_statuses.pop_front() {
                return Err(VmnetError::new(VmnetOperation::StopInterface, status));
            }

            Ok(())
        }
    }

    impl VmnetPacketIoBackend for RecordingVmnetBackend {
        type Interface = RecordedVmnetInterface;

        fn read_packet(
            &mut self,
            interface: &mut Self::Interface,
            _packet: &mut VmnetReadPacket<'_>,
        ) -> Result<Option<usize>, VmnetPacketIoError> {
            push_event(&self.events, format!("read:{}", interface.id));
            self.read_result.clone()
        }

        fn write_packet(
            &mut self,
            interface: &mut Self::Interface,
            packet: &mut VmnetWritePacket<'_>,
        ) -> Result<(), VmnetPacketIoError> {
            push_event(
                &self.events,
                format!(
                    "write:{}:{}",
                    interface.id,
                    packet.as_raw_descriptor().packet_size()
                ),
            );
            self.write_result.clone()
        }
    }

    #[derive(Debug, Clone)]
    struct RecordingVmnetSystemApi {
        events: Arc<Mutex<Vec<String>>>,
        start_handle: Option<usize>,
        start_completion: VmnetStatus,
        stop_schedule_statuses: VecDeque<VmnetStatus>,
        stop_completion_statuses: VecDeque<VmnetStatus>,
        read_status: VmnetStatus,
        read_packet_count: c_int,
        read_packet_size: usize,
        write_status: VmnetStatus,
        write_packet_count: c_int,
    }

    impl RecordingVmnetSystemApi {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                start_handle: Some(0x10),
                start_completion: VmnetStatus::Success,
                stop_schedule_statuses: VecDeque::new(),
                stop_completion_statuses: VecDeque::new(),
                read_status: VmnetStatus::Success,
                read_packet_count: 1,
                read_packet_size: 64,
                write_status: VmnetStatus::Success,
                write_packet_count: 1,
            }
        }

        fn with_null_start_handle(mut self) -> Self {
            self.start_handle = None;
            self
        }

        fn with_start_completion(mut self, status: VmnetStatus) -> Self {
            self.start_completion = status;
            self
        }

        fn with_stop_schedule_status(mut self, status: VmnetStatus) -> Self {
            self.stop_schedule_statuses.push_back(status);
            self
        }

        fn with_stop_completion_status(mut self, status: VmnetStatus) -> Self {
            self.stop_completion_statuses.push_back(status);
            self
        }

        fn with_read_result(
            mut self,
            status: VmnetStatus,
            packet_count: c_int,
            packet_size: usize,
        ) -> Self {
            self.read_status = status;
            self.read_packet_count = packet_count;
            self.read_packet_size = packet_size;
            self
        }

        fn with_write_result(mut self, status: VmnetStatus, packet_count: c_int) -> Self {
            self.write_status = status;
            self.write_packet_count = packet_count;
            self
        }

        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.events)
        }
    }

    impl VmnetSystemApi for RecordingVmnetSystemApi {
        fn start_interface(
            &mut self,
            descriptor: &VmnetInterfaceDescriptor,
            _queue: &DispatchQueue,
            completion: &Block<dyn Fn(u32, *mut c_void)>,
        ) -> Option<NonNull<c_void>> {
            push_event(
                &self.events,
                format!("system-start:{}", descriptor_mode(descriptor)),
            );
            completion.call((self.start_completion.raw_value(), ptr::null_mut()));

            self.start_handle.map(fake_interface)
        }

        fn stop_interface(
            &mut self,
            interface: NonNull<c_void>,
            _queue: &DispatchQueue,
            completion: &Block<dyn Fn(u32)>,
        ) -> VmnetStatus {
            push_event(
                &self.events,
                format!("system-stop:{:x}", interface.as_ptr() as usize),
            );
            let schedule_status = self
                .stop_schedule_statuses
                .pop_front()
                .unwrap_or(VmnetStatus::Success);
            if schedule_status == VmnetStatus::Success {
                let completion_status = self
                    .stop_completion_statuses
                    .pop_front()
                    .unwrap_or(VmnetStatus::Success);
                completion.call((completion_status.raw_value(),));
            }

            schedule_status
        }

        fn read_packets(
            &mut self,
            interface: NonNull<c_void>,
            packets: NonNull<VmnetPacketDescriptor>,
            packet_count: &mut c_int,
        ) -> VmnetStatus {
            push_event(
                &self.events,
                format!(
                    "system-read:{:x}:{}",
                    interface.as_ptr() as usize,
                    *packet_count
                ),
            );
            *packet_count = self.read_packet_count;
            if self.read_status == VmnetStatus::Success && self.read_packet_count == 1 {
                // SAFETY: The fake backend is called with a descriptor borrowed
                // from the packet wrapper for the duration of this synchronous
                // method call, matching the production adapter contract.
                unsafe {
                    (*packets.as_ptr()).vm_pkt_size = self.read_packet_size;
                }
            }

            self.read_status
        }

        fn write_packets(
            &mut self,
            interface: NonNull<c_void>,
            _packets: NonNull<VmnetPacketDescriptor>,
            packet_count: &mut c_int,
        ) -> VmnetStatus {
            push_event(
                &self.events,
                format!(
                    "system-write:{:x}:{}",
                    interface.as_ptr() as usize,
                    *packet_count
                ),
            );
            *packet_count = self.write_packet_count;

            self.write_status
        }
    }

    fn push_event(events: &Arc<Mutex<Vec<String>>>, event: String) {
        match events.lock() {
            Ok(mut guard) => guard.push(event),
            Err(poisoned) => poisoned.into_inner().push(event),
        }
    }

    fn recorded_events(events: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        match events.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn fake_interface(handle: usize) -> NonNull<c_void> {
        NonNull::new(handle as *mut c_void).expect("fake vmnet interface handle must be non-null")
    }

    fn descriptor_mode(descriptor: &VmnetInterfaceDescriptor) -> u64 {
        let key = super::vmnet_operation_mode_key().expect("vmnet mode key should be available");

        // SAFETY: The descriptor owns a live XPC dictionary, and the key comes
        // from the vmnet SDK symbol wrapper.
        unsafe { super::xpc::xpc_dictionary_get_uint64(descriptor.dictionary.as_ptr(), key) }
    }

    fn descriptor_bridged_interface_name(descriptor: &VmnetInterfaceDescriptor) -> Option<String> {
        let key = super::vmnet_shared_interface_name_key()
            .expect("vmnet shared interface name key should be available");

        // SAFETY: The descriptor owns a live XPC dictionary, and the key comes
        // from the vmnet SDK symbol wrapper. XPC owns the returned C string.
        let value =
            unsafe { super::xpc::xpc_dictionary_get_string(descriptor.dictionary.as_ptr(), key) };

        if value.is_null() {
            None
        } else {
            // SAFETY: XPC returns either null or a valid null-terminated C string
            // for the lifetime of the dictionary.
            Some(
                unsafe { CStr::from_ptr(value) }
                    .to_str()
                    .expect("bridged interface name should be valid UTF-8")
                    .to_string(),
            )
        }
    }

    fn descriptor_iov(descriptor: &VmnetPacketDescriptor) -> &libc::iovec {
        assert!(!descriptor.iov_ptr().is_null());

        // SAFETY: Tests only call this after descriptor construction, which
        // stores a pointer to a boxed iovec owned by the packet wrapper.
        unsafe { &*descriptor.iov_ptr() }
    }

    #[test]
    fn vmnet_modes_match_sdk_values() {
        assert_eq!(VmnetMode::Host.raw_value(), VMNET_HOST_MODE_VALUE);
        assert_eq!(VmnetMode::Shared.raw_value(), VMNET_SHARED_MODE_VALUE);
        assert_eq!(VmnetMode::Bridged.raw_value(), VMNET_BRIDGED_MODE_VALUE);
        assert_eq!(VmnetMode::Host.to_string(), "host");
        assert_eq!(VmnetMode::Shared.to_string(), "shared");
        assert_eq!(VmnetMode::Bridged.to_string(), "bridged");
    }

    #[test]
    fn vmnet_status_maps_known_and_unknown_values() {
        assert_eq!(VmnetStatus::from_raw(1000), VmnetStatus::Success);
        assert_eq!(VmnetStatus::from_raw(1010), VmnetStatus::NotAuthorized);
        assert_eq!(VmnetStatus::from_raw(9000), VmnetStatus::Unknown(9000));
        assert_eq!(
            VmnetStatus::NotAuthorized.to_string(),
            "VMNET_NOT_AUTHORIZED (vmnet_return_t=1010)"
        );
        assert_eq!(
            VmnetStatus::Unknown(9000).to_string(),
            "vmnet_return_t=9000"
        );
    }

    #[test]
    fn vmnet_packet_descriptor_matches_sdk_layout() {
        assert_eq!(offset_of!(VmnetPacketDescriptor, vm_pkt_size), 0);
        assert_eq!(
            offset_of!(VmnetPacketDescriptor, vm_pkt_iov),
            size_of::<usize>()
        );
        assert_eq!(
            offset_of!(VmnetPacketDescriptor, vm_pkt_iovcnt),
            size_of::<usize>() + size_of::<*mut libc::iovec>()
        );
        assert_eq!(
            offset_of!(VmnetPacketDescriptor, vm_flags),
            size_of::<usize>() + size_of::<*mut libc::iovec>() + size_of::<u32>()
        );
        assert_eq!(align_of::<VmnetPacketDescriptor>(), align_of::<usize>());
        assert_eq!(
            size_of::<VmnetPacketDescriptor>(),
            size_of::<usize>() + size_of::<*mut libc::iovec>() + (2 * size_of::<u32>())
        );
    }

    #[test]
    fn write_packet_descriptor_borrows_packet_buffer() {
        let packet = [0x12, 0x34, 0x56];
        let packet_ptr = packet.as_ptr().cast::<std::ffi::c_void>().cast_mut();
        let descriptor = VmnetWritePacket::new(&packet)
            .expect("non-empty write packet descriptor should be created");
        let raw_descriptor = descriptor.as_raw_descriptor();
        let iov = descriptor_iov(raw_descriptor);

        assert_eq!(raw_descriptor.packet_size(), packet.len());
        assert_eq!(raw_descriptor.iov_count(), 1);
        assert_eq!(raw_descriptor.flags(), 0);
        assert_eq!(
            raw_descriptor.iov_ptr(),
            descriptor.iov() as *const _ as *mut _
        );
        assert_eq!(iov.iov_base, packet_ptr);
        assert_eq!(iov.iov_len, packet.len());
    }

    #[test]
    fn read_packet_descriptor_borrows_read_buffer() {
        let mut buffer = [0_u8; 2048];
        let buffer_ptr = buffer.as_mut_ptr().cast::<std::ffi::c_void>();
        let buffer_len = buffer.len();
        let descriptor = VmnetReadPacket::new(&mut buffer)
            .expect("non-empty read packet descriptor should be created");
        let raw_descriptor = descriptor.as_raw_descriptor();
        let iov = descriptor_iov(raw_descriptor);

        assert_eq!(raw_descriptor.packet_size(), buffer_len);
        assert_eq!(raw_descriptor.iov_count(), 1);
        assert_eq!(raw_descriptor.flags(), 0);
        assert_eq!(
            raw_descriptor.iov_ptr(),
            descriptor.iov() as *const _ as *mut _
        );
        assert_eq!(iov.iov_base, buffer_ptr);
        assert_eq!(iov.iov_len, buffer_len);
    }

    #[test]
    fn packet_descriptors_reject_empty_buffers() {
        let write_error = VmnetWritePacket::new(&[])
            .expect_err("empty write packet descriptor should be rejected");
        let mut buffer = [];
        let read_error = VmnetReadPacket::new(&mut buffer)
            .expect_err("empty read packet descriptor should be rejected");

        assert_eq!(write_error, VmnetPacketDescriptorError::EmptyPacketBuffer);
        assert_eq!(read_error, VmnetPacketDescriptorError::EmptyPacketBuffer);
        assert_eq!(
            write_error.to_string(),
            "vmnet packet descriptor buffer must not be empty"
        );
    }

    #[test]
    fn write_packet_descriptor_iovec_pointer_survives_wrapper_move() {
        let packet = [0x7f, 0x45, 0x4c, 0x46];
        let packet_ptr = packet.as_ptr().cast::<std::ffi::c_void>().cast_mut();
        let descriptor = VmnetWritePacket::new(&packet)
            .expect("non-empty write packet descriptor should be created");
        let original_iov_ptr = descriptor.as_raw_descriptor().iov_ptr();

        let moved_descriptor = descriptor;
        let moved_iov = descriptor_iov(moved_descriptor.as_raw_descriptor());

        assert_eq!(
            moved_descriptor.as_raw_descriptor().iov_ptr(),
            original_iov_ptr
        );
        assert_eq!(moved_iov.iov_base, packet_ptr);
        assert_eq!(moved_iov.iov_len, packet.len());
    }

    #[test]
    fn read_packet_descriptor_iovec_pointer_survives_wrapper_move() {
        let mut buffer = [0_u8; 64];
        let buffer_ptr = buffer.as_mut_ptr().cast::<std::ffi::c_void>();
        let buffer_len = buffer.len();
        let descriptor = VmnetReadPacket::new(&mut buffer)
            .expect("non-empty read packet descriptor should be created");
        let original_iov_ptr = descriptor.as_raw_descriptor().iov_ptr();

        let moved_descriptor = descriptor;
        let moved_iov = descriptor_iov(moved_descriptor.as_raw_descriptor());

        assert_eq!(
            moved_descriptor.as_raw_descriptor().iov_ptr(),
            original_iov_ptr
        );
        assert_eq!(moved_iov.iov_base, buffer_ptr);
        assert_eq!(moved_iov.iov_len, buffer_len);
    }

    #[test]
    fn started_interface_builds_descriptor_and_stops_once() {
        let backend = RecordingVmnetBackend::new();
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("started vmnet interface should be created");

        assert!(interface.is_started());
        interface
            .stop()
            .expect("started vmnet interface should stop");
        assert!(!interface.is_started());
        interface
            .stop()
            .expect("second stop should be a no-op after cleanup");

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_interface_descriptor_failure_skips_backend_start() {
        let descriptor_error = VmnetInterfaceDescriptorError::MissingVmnetKey("test_key");
        let backend = RecordingVmnetBackend::new().with_descriptor_error(descriptor_error);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("descriptor failure should prevent interface ownership");

        assert_eq!(
            error,
            VmnetInterfaceStartError::Descriptor {
                source: descriptor_error
            }
        );
        assert_eq!(
            error
                .source()
                .expect("descriptor start error should preserve source")
                .to_string(),
            descriptor_error.to_string()
        );
        assert_eq!(recorded_events(&event_log), ["descriptor:host"]);
    }

    #[test]
    fn started_interface_start_failure_does_not_create_owner() {
        let backend = RecordingVmnetBackend::new().with_start_status(VmnetStatus::InvalidArgument);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("start failure should prevent interface ownership");

        match error {
            VmnetInterfaceStartError::Start { source } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::InvalidArgument);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("start failure should not return a descriptor error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
            ]
        );
    }

    #[test]
    fn started_interface_failed_stop_keeps_interface_for_retry() {
        let backend = RecordingVmnetBackend::new().with_stop_status(VmnetStatus::SetupIncomplete);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("started vmnet interface should be created");
        let error = interface
            .stop()
            .expect_err("failed stop should return an error");

        assert_eq!(error.operation(), VmnetOperation::StopInterface);
        assert_eq!(error.status(), VmnetStatus::SetupIncomplete);
        assert!(interface.is_started());

        interface
            .stop()
            .expect("second stop should retry and succeed");
        assert!(!interface.is_started());
        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "stop:9".to_string(),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_interface_drop_stops_started_interface() {
        let backend = RecordingVmnetBackend::new();
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let _interface = StartedVmnetInterface::start(backend, &config)
                .expect("started vmnet interface should be created");
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_interface_drop_retries_after_failed_explicit_stop() {
        let backend = RecordingVmnetBackend::new().with_stop_status(VmnetStatus::SetupIncomplete);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let mut interface = StartedVmnetInterface::start(backend, &config)
                .expect("started vmnet interface should be created");
            let error = interface
                .stop()
                .expect_err("first stop should return configured failure");

            assert_eq!(error.operation(), VmnetOperation::StopInterface);
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "stop:9".to_string(),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_starts_and_stops_on_drop() {
        let backend = RecordingVmnetBackend::new();
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();

        {
            let (backend, _interface) = StartedVmnetPacketIoBackend::start(backend, &config)
                .expect("started packet I/O backend should be created");
            assert!(backend.is_started());
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_delegates_read_and_write() {
        let backend = RecordingVmnetBackend::new().with_read_result(Ok(Some(7)));
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let (mut backend, mut interface) = StartedVmnetPacketIoBackend::start(backend, &config)
                .expect("started packet I/O backend should be created");
            let mut read_buffer = [0_u8; 2048];
            let mut read_packet =
                VmnetReadPacket::new(&mut read_buffer).expect("read packet should build");
            let write_bytes = [0xaa, 0xbb, 0xcc];
            let mut write_packet =
                VmnetWritePacket::new(&write_bytes).expect("write packet should build");

            assert_eq!(
                backend
                    .read_packet(&mut interface, &mut read_packet)
                    .expect("read should delegate"),
                Some(7)
            );
            backend
                .write_packet(&mut interface, &mut write_packet)
                .expect("write should delegate");
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "read:9".to_string(),
                "write:9:3".to_string(),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_builds_virtio_packet_io_adapter() {
        let backend = RecordingVmnetBackend::new().with_read_result(Ok(Some(5)));
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let (backend, interface) = StartedVmnetPacketIoBackend::start(backend, &config)
                .expect("started packet I/O backend should be created");
            let mut packet_io =
                VmnetVirtioNetworkPacketIo::with_rx_buffer_len(backend, interface, 2048)
                    .expect("virtio vmnet packet I/O should build");
            let packet = packet_io
                .rx_source()
                .peek_packet()
                .expect("adapter RX should delegate")
                .expect("adapter RX packet should be present");

            assert_eq!(packet.bytes().len(), 5);
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "read:9".to_string(),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_preserves_write_failure() {
        let backend = RecordingVmnetBackend::new().with_write_result(Err(
            VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::WritePackets,
                expected: VmnetPacketCountExpectation::One,
                actual: 0,
            },
        ));
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();

        {
            let (mut backend, mut interface) = StartedVmnetPacketIoBackend::start(backend, &config)
                .expect("started packet I/O backend should be created");
            let write_bytes = [0xaa];
            let mut write_packet =
                VmnetWritePacket::new(&write_bytes).expect("write packet should build");
            let error = backend
                .write_packet(&mut interface, &mut write_packet)
                .expect_err("write failure should be preserved");

            assert_eq!(
                error,
                VmnetPacketIoError::UnexpectedPacketCount {
                    operation: VmnetOperation::WritePackets,
                    expected: VmnetPacketCountExpectation::One,
                    actual: 0,
                }
            );
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "write:9:1".to_string(),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_start_failure_does_not_create_owner() {
        let backend = RecordingVmnetBackend::new().with_start_status(VmnetStatus::NotAuthorized);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();
        let error = StartedVmnetPacketIoBackend::start(backend, &config)
            .expect_err("start failure should prevent packet I/O ownership");

        match error {
            VmnetInterfaceStartError::Start { source } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::NotAuthorized);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("start failure should not return a descriptor error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_starts_and_stops_interface() {
        let api = RecordingVmnetSystemApi::new();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("system vmnet interface should start");

        assert!(interface.is_started());
        interface
            .stop()
            .expect("system vmnet interface should stop");
        assert!(!interface.is_started());
        interface
            .stop()
            .expect("second system stop should be a no-op");

        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_cleans_up_after_start_completion_failure() {
        let api = RecordingVmnetSystemApi::new().with_start_completion(VmnetStatus::InvalidAccess);
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("system start completion failure should prevent ownership");

        match error {
            VmnetInterfaceStartError::Start { source } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::InvalidAccess);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("system start failure should not be reported as a descriptor error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_rejects_null_start_handle() {
        let api = RecordingVmnetSystemApi::new()
            .with_null_start_handle()
            .with_start_completion(VmnetStatus::NotAuthorized);
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("null vmnet start handle should prevent ownership");

        match error {
            VmnetInterfaceStartError::Start { source } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::NotAuthorized);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("null start handle should not be reported as a descriptor error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE))]
        );
    }

    #[test]
    fn system_vmnet_backend_maps_successful_null_start_handle_to_failure() {
        let api = RecordingVmnetSystemApi::new().with_null_start_handle();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("null vmnet start handle should fail even with success completion");

        match error {
            VmnetInterfaceStartError::Start { source } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::Failure);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("null start handle should not be reported as a descriptor error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [format!(
                "system-start:{}",
                u64::from(VMNET_SHARED_MODE_VALUE)
            )]
        );
    }

    #[test]
    fn system_vmnet_backend_failed_stop_schedule_keeps_interface_for_retry() {
        let api =
            RecordingVmnetSystemApi::new().with_stop_schedule_status(VmnetStatus::SetupIncomplete);
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("system vmnet interface should start");
        let error = interface
            .stop()
            .expect_err("failed system stop schedule should return an error");

        assert_eq!(error.operation(), VmnetOperation::StopInterface);
        assert_eq!(error.status(), VmnetStatus::SetupIncomplete);
        assert!(interface.is_started());

        interface
            .stop()
            .expect("second system stop should retry and succeed");
        assert!(!interface.is_started());
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "system-stop:10".to_string(),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_failed_stop_completion_keeps_interface_for_retry() {
        let api = RecordingVmnetSystemApi::new()
            .with_stop_completion_status(VmnetStatus::SharingServiceBusy);
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("system vmnet interface should start");
        let error = interface
            .stop()
            .expect_err("failed system stop completion should return an error");

        assert_eq!(error.operation(), VmnetOperation::StopInterface);
        assert_eq!(error.status(), VmnetStatus::SharingServiceBusy);
        assert!(interface.is_started());

        interface
            .stop()
            .expect("second system stop should retry and succeed");
        assert!(!interface.is_started());
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "system-stop:10".to_string(),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_drop_stops_started_interface() {
        let api = RecordingVmnetSystemApi::new();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
            let _interface = StartedVmnetInterface::start(backend, &config)
                .expect("system vmnet interface should start");
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_reads_single_packet() {
        let api = RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, 1, 128);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");

        let packet_size = backend
            .read_packet(&mut interface, &mut packet)
            .expect("system vmnet read should succeed");

        assert_eq!(packet_size, Some(128));
        assert_eq!(packet.as_raw_descriptor().packet_size(), 128);
        assert_eq!(recorded_events(&event_log), ["system-read:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_returns_none_when_no_packet_is_available() {
        let api = RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, 0, 64);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");

        let packet_size = backend
            .read_packet(&mut interface, &mut packet)
            .expect("system vmnet read should succeed without packets");

        assert_eq!(packet_size, None);
        assert_eq!(recorded_events(&event_log), ["system-read:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_preserves_read_failure_status() {
        let api =
            RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::BufferExhausted, 0, 64);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");
        let error = backend
            .read_packet(&mut interface, &mut packet)
            .expect_err("read failure status should be preserved");

        match error {
            VmnetPacketIoError::Vmnet { source } => {
                assert_eq!(source.operation(), VmnetOperation::ReadPackets);
                assert_eq!(source.status(), VmnetStatus::BufferExhausted);
            }
            VmnetPacketIoError::InterfaceStopped => {
                panic!("vmnet read status failure should not become a stopped interface error");
            }
            VmnetPacketIoError::UnexpectedPacketCount { .. } => {
                panic!("vmnet read status failure should not become a count error");
            }
            VmnetPacketIoError::ReadPacketSizeExceedsBuffer { .. } => {
                panic!("vmnet read status failure should not become a packet size error");
            }
        }
        assert_eq!(recorded_events(&event_log), ["system-read:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_rejects_unexpected_read_packet_count() {
        let api = RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, -1, 64);
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");
        let error = backend
            .read_packet(&mut interface, &mut packet)
            .expect_err("unexpected read packet count should fail");

        assert_eq!(
            error,
            VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::ReadPackets,
                expected: VmnetPacketCountExpectation::ZeroOrOne,
                actual: -1,
            }
        );
        assert_eq!(
            error.to_string(),
            "vmnet_read returned packet count -1, expected 0 or 1"
        );
    }

    #[test]
    fn system_vmnet_backend_rejects_oversized_read_packet() {
        let api = RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, 1, 2049);
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");
        let error = backend
            .read_packet(&mut interface, &mut packet)
            .expect_err("oversized read packet should fail");

        assert_eq!(
            error,
            VmnetPacketIoError::ReadPacketSizeExceedsBuffer {
                packet_size: 2049,
                buffer_len: 2048,
            }
        );
        assert_eq!(
            error.to_string(),
            "vmnet_read returned packet size 2049, larger than read buffer 2048"
        );
    }

    #[test]
    fn system_vmnet_backend_writes_single_packet() {
        let api = RecordingVmnetSystemApi::new();
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let packet = [0xde, 0xad, 0xbe, 0xef];
        let mut packet =
            VmnetWritePacket::new(&packet).expect("write packet descriptor should be valid");

        backend
            .write_packet(&mut interface, &mut packet)
            .expect("system vmnet write should succeed");

        assert_eq!(recorded_events(&event_log), ["system-write:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_preserves_write_failure_status() {
        let api = RecordingVmnetSystemApi::new().with_write_result(VmnetStatus::PacketTooBig, 0);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let packet = [0xde, 0xad, 0xbe, 0xef];
        let mut packet =
            VmnetWritePacket::new(&packet).expect("write packet descriptor should be valid");
        let error = backend
            .write_packet(&mut interface, &mut packet)
            .expect_err("write failure status should be preserved");

        match error {
            VmnetPacketIoError::Vmnet { source } => {
                assert_eq!(source.operation(), VmnetOperation::WritePackets);
                assert_eq!(source.status(), VmnetStatus::PacketTooBig);
            }
            VmnetPacketIoError::InterfaceStopped => {
                panic!("vmnet write status failure should not become a stopped interface error");
            }
            VmnetPacketIoError::UnexpectedPacketCount { .. } => {
                panic!("vmnet write status failure should not become a count error");
            }
            VmnetPacketIoError::ReadPacketSizeExceedsBuffer { .. } => {
                panic!("vmnet write status failure should not become a packet size error");
            }
        }
        assert_eq!(recorded_events(&event_log), ["system-write:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_rejects_unexpected_write_packet_count() {
        let api = RecordingVmnetSystemApi::new().with_write_result(VmnetStatus::Success, 2);
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let packet = [0xde, 0xad, 0xbe, 0xef];
        let mut packet =
            VmnetWritePacket::new(&packet).expect("write packet descriptor should be valid");
        let error = backend
            .write_packet(&mut interface, &mut packet)
            .expect_err("unexpected write packet count should fail");

        assert_eq!(
            error,
            VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::WritePackets,
                expected: VmnetPacketCountExpectation::One,
                actual: 2,
            }
        );
        assert_eq!(
            error.to_string(),
            "vmnet_write returned packet count 2, expected 1"
        );
    }

    #[test]
    fn vmnet_error_includes_operation_and_status() {
        let error = VmnetError::new(VmnetOperation::ReadPackets, VmnetStatus::BufferExhausted);

        assert_eq!(error.operation(), VmnetOperation::ReadPackets);
        assert_eq!(error.status(), VmnetStatus::BufferExhausted);
        assert_eq!(
            error.to_string(),
            "vmnet_read failed with VMNET_BUFFER_EXHAUSTED (vmnet_return_t=1007)"
        );
    }

    #[test]
    fn vmnet_interface_config_models_modes() {
        let host = VmnetInterfaceConfig::host();
        let shared = VmnetInterfaceConfig::shared();
        let bridged = VmnetInterfaceConfig::bridged("en0").expect("bridged config should validate");

        assert_eq!(host.mode(), VmnetMode::Host);
        assert_eq!(host.bridged_interface_name(), None);
        assert_eq!(shared.mode(), VmnetMode::Shared);
        assert_eq!(shared.bridged_interface_name(), None);
        assert_eq!(bridged.mode(), VmnetMode::Bridged);
        assert_eq!(bridged.bridged_interface_name(), Some("en0"));
    }

    #[test]
    fn vmnet_host_dev_name_maps_host_mode() {
        let config = VmnetInterfaceConfig::from_host_dev_name("vmnet:host")
            .expect("host host_dev_name should map");

        assert_eq!(config.mode(), VmnetMode::Host);
        assert_eq!(config.bridged_interface_name(), None);
    }

    #[test]
    fn vmnet_host_dev_name_maps_shared_mode() {
        let config = VmnetInterfaceConfig::from_host_dev_name("vmnet:shared")
            .expect("shared host_dev_name should map");

        assert_eq!(config.mode(), VmnetMode::Shared);
        assert_eq!(config.bridged_interface_name(), None);
    }

    #[test]
    fn vmnet_host_dev_name_maps_bridged_mode() {
        let config = VmnetInterfaceConfig::from_host_dev_name("vmnet:bridged:en0")
            .expect("bridged host_dev_name should map");

        assert_eq!(config.mode(), VmnetMode::Bridged);
        assert_eq!(config.bridged_interface_name(), Some("en0"));
    }

    #[test]
    fn vmnet_host_dev_name_rejects_unsupported_bare_name() {
        let error = VmnetInterfaceConfig::from_host_dev_name("tap0")
            .expect_err("bare host_dev_name should be rejected by vmnet mapping");

        assert_eq!(
            error,
            VmnetHostDeviceNameConfigError::UnsupportedHostDeviceName
        );
        assert!(error.source().is_none());
        assert_eq!(
            error.to_string(),
            "unsupported vmnet host_dev_name; expected vmnet:host, vmnet:shared, or vmnet:bridged:<interface>"
        );
    }

    #[test]
    fn vmnet_host_dev_name_rejects_empty_bridged_interface() {
        let error = VmnetInterfaceConfig::from_host_dev_name("vmnet:bridged:")
            .expect_err("empty bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetHostDeviceNameConfigError::BridgedInterface {
                source: VmnetInterfaceConfigError::EmptyBridgedInterfaceName
            }
        );
        assert_eq!(
            error
                .source()
                .expect("bridged error should expose source")
                .to_string(),
            "vmnet bridged interface name must not be empty"
        );
    }

    #[test]
    fn vmnet_host_dev_name_rejects_interior_nul_bridged_interface() {
        let error = VmnetInterfaceConfig::from_host_dev_name("vmnet:bridged:en\0")
            .expect_err("interior NUL bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetHostDeviceNameConfigError::BridgedInterface {
                source: VmnetInterfaceConfigError::InteriorNulInBridgedInterfaceName
            }
        );
        assert_eq!(
            error
                .source()
                .expect("bridged error should expose source")
                .to_string(),
            "vmnet bridged interface name must not contain NUL bytes"
        );
    }

    #[test]
    fn vmnet_host_dev_name_rejects_control_character_bridged_interface() {
        let error = VmnetInterfaceConfig::from_host_dev_name("vmnet:bridged:en\n0")
            .expect_err("control-character bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetHostDeviceNameConfigError::BridgedInterface {
                source: VmnetInterfaceConfigError::ControlCharacterInBridgedInterfaceName
            }
        );
        assert_eq!(
            error
                .source()
                .expect("bridged error should expose source")
                .to_string(),
            "vmnet bridged interface name must not contain ASCII control characters"
        );
        assert!(!error.to_string().contains("en\n0"));
    }

    #[test]
    fn bridged_config_rejects_empty_interface_name() {
        let error = VmnetInterfaceConfig::bridged("")
            .expect_err("empty bridged interface should be rejected");

        assert_eq!(error, VmnetInterfaceConfigError::EmptyBridgedInterfaceName);
        assert_eq!(
            error.to_string(),
            "vmnet bridged interface name must not be empty"
        );
    }

    #[test]
    fn bridged_config_rejects_interior_nul_interface_name() {
        let error = VmnetInterfaceConfig::bridged("en\0")
            .expect_err("interior NUL bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetInterfaceConfigError::InteriorNulInBridgedInterfaceName
        );
        assert_eq!(
            error.to_string(),
            "vmnet bridged interface name must not contain NUL bytes"
        );
    }

    #[test]
    fn bridged_config_rejects_control_character_interface_name() {
        let error = VmnetInterfaceConfig::bridged("en\t0")
            .expect_err("control-character bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetInterfaceConfigError::ControlCharacterInBridgedInterfaceName
        );
        assert_eq!(
            error.to_string(),
            "vmnet bridged interface name must not contain ASCII control characters"
        );
    }

    #[test]
    fn vmnet_interface_descriptor_models_host_mode() {
        let config = VmnetInterfaceConfig::host();
        let descriptor =
            VmnetInterfaceDescriptor::new(&config).expect("host descriptor should be created");

        assert_eq!(
            descriptor_mode(&descriptor),
            u64::from(VMNET_HOST_MODE_VALUE)
        );
        assert_eq!(descriptor_bridged_interface_name(&descriptor), None);
    }

    #[test]
    fn vmnet_interface_descriptor_models_shared_mode() {
        let config = VmnetInterfaceConfig::shared();
        let descriptor =
            VmnetInterfaceDescriptor::new(&config).expect("shared descriptor should be created");

        assert_eq!(
            descriptor_mode(&descriptor),
            u64::from(VMNET_SHARED_MODE_VALUE)
        );
        assert_eq!(descriptor_bridged_interface_name(&descriptor), None);
    }

    #[test]
    fn vmnet_interface_descriptor_models_bridged_mode() {
        let config = VmnetInterfaceConfig::bridged("en0").expect("bridged config should validate");
        let descriptor =
            VmnetInterfaceDescriptor::new(&config).expect("bridged descriptor should be created");

        assert_eq!(
            descriptor_mode(&descriptor),
            u64::from(VMNET_BRIDGED_MODE_VALUE)
        );
        assert_eq!(
            descriptor_bridged_interface_name(&descriptor),
            Some("en0".to_string())
        );
    }

    #[test]
    fn owned_interface_starts_and_stops_once() {
        let lifecycle = RecordingVmnetLifecycle::new();
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();
        let mut interface = OwnedVmnetInterface::start(lifecycle, &config)
            .expect("owned vmnet interface should start");

        assert!(interface.is_started());
        interface.stop().expect("owned vmnet interface should stop");
        assert!(!interface.is_started());
        interface.stop().expect("second stop should be a no-op");

        assert_eq!(recorded_events(&event_log), ["start:host", "stop:7"]);
    }

    #[test]
    fn owned_interface_drop_stops_started_interface() {
        let lifecycle = RecordingVmnetLifecycle::new();
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let _interface = OwnedVmnetInterface::start(lifecycle, &config)
                .expect("owned vmnet interface should start");
        }

        assert_eq!(recorded_events(&event_log), ["start:shared", "stop:7"]);
    }

    #[test]
    fn owned_interface_start_failure_does_not_create_owner() {
        let lifecycle =
            RecordingVmnetLifecycle::new().with_start_status(VmnetStatus::InvalidAccess);
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();
        let error = OwnedVmnetInterface::start(lifecycle, &config)
            .expect_err("start failure should return an error");

        assert_eq!(error.operation(), VmnetOperation::StartInterface);
        assert_eq!(error.status(), VmnetStatus::InvalidAccess);
        assert_eq!(recorded_events(&event_log), ["start:host"]);
    }

    #[test]
    fn failed_stop_keeps_interface_started_for_retry() {
        let lifecycle =
            RecordingVmnetLifecycle::new().with_stop_status(VmnetStatus::SetupIncomplete);
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();
        let mut interface = OwnedVmnetInterface::start(lifecycle, &config)
            .expect("owned vmnet interface should start");
        let error = interface
            .stop()
            .expect_err("failed stop should return an error");

        assert_eq!(error.operation(), VmnetOperation::StopInterface);
        assert_eq!(error.status(), VmnetStatus::SetupIncomplete);
        assert!(interface.is_started());
        assert_eq!(recorded_events(&event_log), ["start:host", "stop:7"]);
    }

    #[test]
    fn drop_retries_cleanup_after_failed_explicit_stop() {
        let lifecycle =
            RecordingVmnetLifecycle::new().with_stop_status(VmnetStatus::SetupIncomplete);
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();

        {
            let mut interface = OwnedVmnetInterface::start(lifecycle, &config)
                .expect("owned vmnet interface should start");

            let _ = interface.stop();
        }

        assert_eq!(
            recorded_events(&event_log),
            ["start:host", "stop:7", "stop:7"]
        );
    }

    #[test]
    fn vmnet_write_operation_displays_name() {
        let error = VmnetError::new(VmnetOperation::WritePackets, VmnetStatus::PacketTooBig);

        assert_eq!(
            error.to_string(),
            "vmnet_write failed with VMNET_PACKET_TOO_BIG (vmnet_return_t=1006)"
        );
    }
}
