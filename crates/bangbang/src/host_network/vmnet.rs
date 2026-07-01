//! vmnet lifecycle boundary types for future macOS host networking.

use std::ffi::{CString, c_char, c_void};
use std::fmt;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};

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

    pub fn bridged(interface_name: impl Into<String>) -> Result<Self, VmnetInterfaceConfigError> {
        let interface_name = interface_name.into();
        if interface_name.is_empty() {
            return Err(VmnetInterfaceConfigError::EmptyBridgedInterfaceName);
        }
        if interface_name.as_bytes().contains(&0) {
            return Err(VmnetInterfaceConfigError::InteriorNulInBridgedInterfaceName);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetInterfaceConfigError {
    EmptyBridgedInterfaceName,
    InteriorNulInBridgedInterfaceName,
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
    use std::ffi::CStr;
    use std::mem::{align_of, offset_of, size_of};
    use std::sync::{Arc, Mutex};

    use super::{
        OwnedVmnetInterface, StartedVmnetInterface, VMNET_BRIDGED_MODE_VALUE,
        VMNET_HOST_MODE_VALUE, VMNET_SHARED_MODE_VALUE, VmnetError, VmnetInterfaceBackend,
        VmnetInterfaceConfig, VmnetInterfaceConfigError, VmnetInterfaceDescriptor,
        VmnetInterfaceDescriptorError, VmnetInterfaceLifecycle, VmnetInterfaceStartError,
        VmnetMode, VmnetOperation, VmnetPacketDescriptor, VmnetPacketDescriptorError,
        VmnetReadPacket, VmnetStatus, VmnetWritePacket,
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
    }

    impl RecordingVmnetBackend {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                descriptor_error: None,
                start_status: None,
                stop_statuses: VecDeque::new(),
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
