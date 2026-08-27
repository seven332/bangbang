//! Contained-worker adapter for the launcher-owned vmnet provider protocol.
//!
//! A remote backend never calls `vmnet_start_interface`. The first real vmnet
//! interface lazily consumes the authenticated provider grant, and one pump
//! thread remains the sole owner of each protocol state machine and stream.

use std::fmt;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bangbang_session::vmnet_provider::{
    ControlClientEvent, DataClientEvent, MAX_PROVIDER_TIMEOUT, ProviderCancelReason,
    ProviderOperation, ProviderStatus, ProviderTerminalCode, RealizedVmnetParameters,
    RequestedVmnetParameters, VmnetControlClient, VmnetDataClient, VmnetGeneration,
    VmnetInterfaceId, VmnetPacketBatch, VmnetPolicySlot, VmnetProviderError,
    VmnetProviderTransport,
};
use bangbang_session::{SessionId, VmnetAuthority};

use crate::contained_session::VmnetProviderGrantAuthority;

use super::vmnet::{
    SystemVmnetInterface, SystemVmnetInterfaceBackend, VmnetError, VmnetInterfaceBackend,
    VmnetInterfaceConfig, VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError,
    VmnetInterfaceParameters, VmnetInterfaceStartDisposition, VmnetInterfaceStartError, VmnetMode,
    VmnetOperation, VmnetPacketAvailableCallback, VmnetPacketCountExpectation,
    VmnetPacketIoBackend, VmnetPacketIoError, VmnetReadPacket, VmnetStartedInterface, VmnetStatus,
    VmnetWritePacket,
};

const COMMAND_QUEUE_CAPACITY: usize = 32;
const CALL_TIMEOUT: Duration = Duration::from_secs(6);
const PUMP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CANCEL_NONE: u8 = 0;
const CANCEL_WORKER: u8 = 1;
const CANCEL_LAUNCHER: u8 = 2;

/// Cloneable backend selector retained by startup and runtime-hotplug paths.
#[derive(Clone, Default)]
pub(crate) enum ProcessVmnetBackendSource {
    #[default]
    LocalSystem,
    Remote(RemoteVmnetProviderSource),
}

impl fmt::Debug for ProcessVmnetBackendSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::LocalSystem => "ProcessVmnetBackendSource::LocalSystem",
            Self::Remote(_) => "ProcessVmnetBackendSource::Remote(<owned>)",
        })
    }
}

impl ProcessVmnetBackendSource {
    pub(crate) const fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    pub(crate) fn new_backend(&self) -> ProcessVmnetInterfaceBackend {
        match self {
            Self::LocalSystem => {
                ProcessVmnetInterfaceBackend::Local(SystemVmnetInterfaceBackend::new())
            }
            Self::Remote(source) => ProcessVmnetInterfaceBackend::Remote(
                RemoteVmnetInterfaceBackend::new(source.clone()),
            ),
        }
    }

    pub(crate) fn cancel_from_launcher(&self) {
        if let Self::Remote(source) = self {
            source.cancel(ProviderCancelReason::Launcher);
        }
    }
}

/// Production interface backend selected before any host-network work begins.
pub(crate) enum ProcessVmnetInterfaceBackend {
    Local(SystemVmnetInterfaceBackend),
    Remote(RemoteVmnetInterfaceBackend),
}

impl fmt::Debug for ProcessVmnetInterfaceBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Local(_) => "ProcessVmnetInterfaceBackend::Local(<owned>)",
            Self::Remote(_) => "ProcessVmnetInterfaceBackend::Remote(<owned>)",
        })
    }
}

/// Exact interface owner paired with its selected backend.
pub(crate) enum ProcessVmnetInterface {
    Local(SystemVmnetInterface),
    Remote(RemoteVmnetInterface),
}

impl fmt::Debug for ProcessVmnetInterface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Local(_) => "ProcessVmnetInterface::Local(<owned>)",
            Self::Remote(_) => "ProcessVmnetInterface::Remote(<owned>)",
        })
    }
}

impl VmnetInterfaceBackend for ProcessVmnetInterfaceBackend {
    type Interface = ProcessVmnetInterface;

    fn build_interface_descriptor(
        &mut self,
        config: &VmnetInterfaceConfig,
    ) -> Result<VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError> {
        match self {
            Self::Local(backend) => backend.build_interface_descriptor(config),
            Self::Remote(backend) => backend.build_interface_descriptor(config),
        }
    }

    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
    ) -> Result<VmnetStartedInterface<Self::Interface>, VmnetInterfaceStartError> {
        match self {
            Self::Local(backend) => backend.start_interface(descriptor).map(|started| {
                let (interface, parameters) = started.into_parts();
                VmnetStartedInterface::new(ProcessVmnetInterface::Local(interface), parameters)
            }),
            Self::Remote(backend) => backend.start_interface(descriptor).map(|started| {
                let (interface, parameters) = started.into_parts();
                VmnetStartedInterface::new(ProcessVmnetInterface::Remote(interface), parameters)
            }),
        }
    }

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
        match (self, interface) {
            (Self::Local(backend), ProcessVmnetInterface::Local(interface)) => {
                backend.stop_interface(interface)
            }
            (Self::Remote(backend), ProcessVmnetInterface::Remote(interface)) => {
                backend.stop_interface(interface)
            }
            _ => Err(VmnetError::new(
                VmnetOperation::StopInterface,
                VmnetStatus::InvalidArgument,
            )),
        }
    }

    fn enable_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
        callback: VmnetPacketAvailableCallback,
    ) -> Result<(), VmnetError> {
        match (self, interface) {
            (Self::Local(backend), ProcessVmnetInterface::Local(interface)) => {
                backend.enable_packet_available_callback(interface, callback)
            }
            (Self::Remote(backend), ProcessVmnetInterface::Remote(interface)) => {
                backend.enable_packet_available_callback(interface, callback)
            }
            _ => Err(VmnetError::new(
                VmnetOperation::EnablePacketEvents,
                VmnetStatus::InvalidArgument,
            )),
        }
    }

    fn disable_and_drain_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
    ) -> Result<(), VmnetError> {
        match (self, interface) {
            (Self::Local(backend), ProcessVmnetInterface::Local(interface)) => {
                backend.disable_and_drain_packet_available_callback(interface)
            }
            (Self::Remote(backend), ProcessVmnetInterface::Remote(interface)) => {
                backend.disable_and_drain_packet_available_callback(interface)
            }
            _ => Err(VmnetError::new(
                VmnetOperation::DisablePacketEvents,
                VmnetStatus::InvalidArgument,
            )),
        }
    }
}

impl VmnetPacketIoBackend for ProcessVmnetInterfaceBackend {
    type Interface = ProcessVmnetInterface;

    fn read_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError> {
        match (self, interface) {
            (Self::Local(backend), ProcessVmnetInterface::Local(interface)) => {
                backend.read_packet(interface, packet)
            }
            (Self::Remote(backend), ProcessVmnetInterface::Remote(interface)) => {
                backend.read_packet(interface, packet)
            }
            _ => Err(VmnetPacketIoError::InterfaceStopped),
        }
    }

    fn write_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError> {
        match (self, interface) {
            (Self::Local(backend), ProcessVmnetInterface::Local(interface)) => {
                backend.write_packet(interface, packet)
            }
            (Self::Remote(backend), ProcessVmnetInterface::Remote(interface)) => {
                backend.write_packet(interface, packet)
            }
            _ => Err(VmnetPacketIoError::InterfaceStopped),
        }
    }

    fn read_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &mut [u8],
        packet_capacity: usize,
        requested_packets: usize,
        packet_lengths: &mut [usize],
    ) -> Result<usize, VmnetPacketIoError> {
        match (self, interface) {
            (Self::Local(backend), ProcessVmnetInterface::Local(interface)) => backend
                .read_packet_batch(
                    interface,
                    buffer,
                    packet_capacity,
                    requested_packets,
                    packet_lengths,
                ),
            (Self::Remote(backend), ProcessVmnetInterface::Remote(interface)) => backend
                .read_packet_batch(
                    interface,
                    buffer,
                    packet_capacity,
                    requested_packets,
                    packet_lengths,
                ),
            _ => Err(VmnetPacketIoError::InterfaceStopped),
        }
    }

    fn write_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &[u8],
        packet_ranges: &[Range<usize>],
    ) -> Result<usize, VmnetPacketIoError> {
        match (self, interface) {
            (Self::Local(backend), ProcessVmnetInterface::Local(interface)) => {
                backend.write_packet_batch(interface, buffer, packet_ranges)
            }
            (Self::Remote(backend), ProcessVmnetInterface::Remote(interface)) => {
                backend.write_packet_batch(interface, buffer, packet_ranges)
            }
            _ => Err(VmnetPacketIoError::InterfaceStopped),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RemoteVmnetProviderSource {
    inner: Arc<RemoteProviderSourceInner>,
}

struct RemoteProviderSourceInner {
    session: SessionId,
    authority: VmnetAuthority,
    grant: Mutex<RemoteProviderGrantState>,
    next_interface: AtomicU32,
    cancel_reason: AtomicU8,
}

enum RemoteProviderGrantState {
    Unclaimed(VmnetProviderGrantAuthority),
    Connecting(UnixStream),
    Ready(ControlPumpHandle),
    Terminal,
}

impl fmt::Debug for RemoteVmnetProviderSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteVmnetProviderSource(<owned>)")
    }
}

impl RemoteVmnetProviderSource {
    pub(crate) fn new(
        session: SessionId,
        authority: VmnetAuthority,
        grant: VmnetProviderGrantAuthority,
    ) -> Option<Self> {
        if session.is_pre_session() || authority.is_denied() {
            return None;
        }
        let registration = grant.clone();
        let source = Self {
            inner: Arc::new(RemoteProviderSourceInner {
                session,
                authority,
                grant: Mutex::new(RemoteProviderGrantState::Unclaimed(grant)),
                next_interface: AtomicU32::new(1),
                cancel_reason: AtomicU8::new(CANCEL_NONE),
            }),
        };
        let weak = Arc::downgrade(&source.inner);
        registration
            .register_invalidation(Arc::new(move || {
                cancel_remote_source(&weak, ProviderCancelReason::Launcher);
            }))
            .ok()?;
        Some(source)
    }

    fn start(&self, config: &VmnetInterfaceConfig) -> Result<RemoteStarted, RemoteFailure> {
        let slot = policy_slot(self.inner.authority, config)?;
        let requested =
            RequestedVmnetParameters::new(config.guest_mac().map(|mac| mac.octets()), config.mtu())
                .map_err(|_| RemoteFailure::retryable(VmnetStatus::InvalidArgument))?;
        let interface_value = self
            .inner
            .next_interface
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1).filter(|next| *next != 0)
            })
            .map_err(|_| RemoteFailure::retryable(VmnetStatus::Failure))?;
        let interface = VmnetInterfaceId::new(interface_value)
            .map_err(|_| RemoteFailure::retryable(VmnetStatus::Failure))?;
        self.with_control(|control| control.start(interface, slot, requested))
    }

    fn with_control<T>(
        &self,
        operation: impl FnOnce(&ControlPumpHandle) -> Result<T, RemoteFailure>,
    ) -> Result<T, RemoteFailure> {
        enum Acquisition {
            Ready(ControlPumpHandle),
            Connect(UnixStream),
        }

        let acquisition = {
            let mut state = self
                .inner
                .grant
                .lock()
                .map_err(|_| RemoteFailure::terminal(VmnetStatus::Failure))?;
            match &*state {
                RemoteProviderGrantState::Ready(control) => Acquisition::Ready(control.clone()),
                RemoteProviderGrantState::Connecting(_) => {
                    return Err(RemoteFailure::retryable(VmnetStatus::SharingServiceBusy));
                }
                RemoteProviderGrantState::Terminal => {
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
                RemoteProviderGrantState::Unclaimed(grant) => {
                    let stream = grant
                        .claim()
                        .map_err(|_| RemoteFailure::terminal(VmnetStatus::InvalidAccess))?
                        .into_stream();
                    let cancellation_stream = stream.try_clone().map_err(|error| {
                        protocol_failure(VmnetProviderError::Io(error.kind()), true)
                    })?;
                    *state = RemoteProviderGrantState::Connecting(cancellation_stream);
                    Acquisition::Connect(stream)
                }
            }
        };
        let control = match acquisition {
            Acquisition::Ready(control) => control,
            Acquisition::Connect(stream) => {
                let connected = ControlPumpHandle::connect(self.inner.session, stream);
                let mut state = self
                    .inner
                    .grant
                    .lock()
                    .map_err(|_| RemoteFailure::terminal(VmnetStatus::Failure))?;
                match connected {
                    Ok(control) if matches!(*state, RemoteProviderGrantState::Connecting(_)) => {
                        *state = RemoteProviderGrantState::Ready(control.clone());
                        control
                    }
                    Ok(control) => {
                        control.cancel(decoded_cancel_reason(
                            self.inner.cancel_reason.load(Ordering::Acquire),
                        ));
                        return Err(RemoteFailure::terminal(VmnetStatus::SetupIncomplete));
                    }
                    Err(error) => {
                        *state = RemoteProviderGrantState::Terminal;
                        return Err(error.terminalized());
                    }
                }
            }
        };
        let result = operation(&control);
        if result
            .as_ref()
            .err()
            .is_some_and(|error| error.cleanup_uncertain)
        {
            control.cancel(ProviderCancelReason::Worker);
            let mut state = match self.inner.grant.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            if matches!(*state, RemoteProviderGrantState::Ready(_)) {
                *state = RemoteProviderGrantState::Terminal;
            }
        }
        result
    }

    fn stop(&self, interface: VmnetInterfaceId) -> Result<(), RemoteFailure> {
        self.with_control(|control| control.stop(interface))
    }

    pub(crate) fn cancel(&self, reason: ProviderCancelReason) {
        cancel_remote_source_inner(&self.inner, reason);
    }
}

fn cancel_remote_source(weak: &Weak<RemoteProviderSourceInner>, reason: ProviderCancelReason) {
    if let Some(inner) = weak.upgrade() {
        cancel_remote_source_inner(&inner, reason);
    }
}

fn cancel_remote_source_inner(inner: &RemoteProviderSourceInner, reason: ProviderCancelReason) {
    let encoded = match reason {
        ProviderCancelReason::Worker => CANCEL_WORKER,
        ProviderCancelReason::Launcher => CANCEL_LAUNCHER,
    };
    if encoded == CANCEL_LAUNCHER {
        inner.cancel_reason.store(encoded, Ordering::Release);
    } else {
        let _ = inner.cancel_reason.compare_exchange(
            CANCEL_NONE,
            encoded,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
    let mut state = match inner.grant.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    };
    let previous = std::mem::replace(&mut *state, RemoteProviderGrantState::Terminal);
    let control = match previous {
        RemoteProviderGrantState::Ready(control) => Some(control),
        RemoteProviderGrantState::Connecting(stream) => {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            None
        }
        RemoteProviderGrantState::Unclaimed(_) => None,
        RemoteProviderGrantState::Terminal => None,
    };
    drop(state);
    if let Some(control) = control {
        control.cancel(reason);
    }
}

fn policy_slot(
    authority: VmnetAuthority,
    config: &VmnetInterfaceConfig,
) -> Result<VmnetPolicySlot, RemoteFailure> {
    match config.mode() {
        VmnetMode::Host if authority.allows_host() => Ok(VmnetPolicySlot::Host),
        VmnetMode::Shared if authority.allows_shared() => Ok(VmnetPolicySlot::Shared),
        VmnetMode::Bridged => {
            let requested = config
                .bridged_interface_name()
                .ok_or_else(|| RemoteFailure::retryable(VmnetStatus::InvalidArgument))?;
            match (0..4).find(|index| authority.bridge_slot(*index) == Some(requested)) {
                Some(0) => Ok(VmnetPolicySlot::Bridge0),
                Some(1) => Ok(VmnetPolicySlot::Bridge1),
                Some(2) => Ok(VmnetPolicySlot::Bridge2),
                Some(3) => Ok(VmnetPolicySlot::Bridge3),
                Some(_) | None => Err(RemoteFailure::retryable(VmnetStatus::NotAuthorized)),
            }
        }
        VmnetMode::Host | VmnetMode::Shared => {
            Err(RemoteFailure::retryable(VmnetStatus::NotAuthorized))
        }
    }
}

pub(crate) struct RemoteVmnetInterfaceBackend {
    source: RemoteVmnetProviderSource,
    pending_config: Option<VmnetInterfaceConfig>,
}

impl fmt::Debug for RemoteVmnetInterfaceBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteVmnetInterfaceBackend(<owned>)")
    }
}

impl RemoteVmnetInterfaceBackend {
    fn new(source: RemoteVmnetProviderSource) -> Self {
        Self {
            source,
            pending_config: None,
        }
    }
}

pub(crate) struct RemoteVmnetInterface {
    source: RemoteVmnetProviderSource,
    interface: VmnetInterfaceId,
    data: DataPumpHandle,
    stopped: bool,
}

impl fmt::Debug for RemoteVmnetInterface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteVmnetInterface(<owned>)")
    }
}

impl VmnetInterfaceBackend for RemoteVmnetInterfaceBackend {
    type Interface = RemoteVmnetInterface;

    fn build_interface_descriptor(
        &mut self,
        config: &VmnetInterfaceConfig,
    ) -> Result<VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError> {
        self.pending_config = Some(
            config
                .try_clone()
                .map_err(|_| VmnetInterfaceDescriptorError::CopyConfigurationFailed)?,
        );
        Ok(VmnetInterfaceDescriptor::remote(config))
    }

    fn start_interface(
        &mut self,
        _descriptor: &VmnetInterfaceDescriptor,
    ) -> Result<VmnetStartedInterface<Self::Interface>, VmnetInterfaceStartError> {
        let config = self.pending_config.take().ok_or_else(|| {
            VmnetInterfaceStartError::start(
                VmnetError::new(VmnetOperation::StartInterface, VmnetStatus::InvalidArgument),
                VmnetInterfaceStartDisposition::Retryable,
            )
        })?;
        let started = self.source.start(&config).map_err(start_error)?;
        let parameters = match VmnetInterfaceParameters::from_provider(&config, started.parameters)
        {
            Ok(parameters) => parameters,
            Err(source) => {
                let mut interface = RemoteVmnetInterface {
                    source: self.source.clone(),
                    interface: started.interface,
                    data: started.data,
                    stopped: false,
                };
                let disposition = if stop_remote_interface(&mut interface).is_ok() {
                    VmnetInterfaceStartDisposition::Retryable
                } else {
                    VmnetInterfaceStartDisposition::Terminal
                };
                return Err(VmnetInterfaceStartError::parameters(source, disposition));
            }
        };
        Ok(VmnetStartedInterface::new(
            RemoteVmnetInterface {
                source: self.source.clone(),
                interface: started.interface,
                data: started.data,
                stopped: false,
            },
            parameters,
        ))
    }

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
        stop_remote_interface(interface)
            .map_err(|failure| failure.vmnet(VmnetOperation::StopInterface))
    }

    fn enable_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
        callback: VmnetPacketAvailableCallback,
    ) -> Result<(), VmnetError> {
        interface
            .data
            .enable_callback(callback)
            .map_err(|failure| failure.vmnet(VmnetOperation::EnablePacketEvents))
    }

    fn disable_and_drain_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
    ) -> Result<(), VmnetError> {
        interface
            .data
            .disable_callback()
            .map_err(|failure| failure.vmnet(VmnetOperation::DisablePacketEvents))
    }
}

fn stop_remote_interface(interface: &mut RemoteVmnetInterface) -> Result<(), RemoteFailure> {
    if interface.stopped {
        return Ok(());
    }
    if let Err(error) = interface.data.stop() {
        interface.stopped = true;
        interface.source.cancel(ProviderCancelReason::Worker);
        return Err(error.terminalized());
    }
    interface.stopped = true;
    if let Err(error) = interface.source.stop(interface.interface) {
        interface.source.cancel(ProviderCancelReason::Worker);
        Err(error.terminalized())
    } else {
        Ok(())
    }
}

impl VmnetPacketIoBackend for RemoteVmnetInterfaceBackend {
    type Interface = RemoteVmnetInterface;

    fn read_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError> {
        let iov = packet.iov();
        // SAFETY: `VmnetReadPacket` owns an iovec whose mutable buffer remains
        // live and exclusively borrowed for this call.
        let buffer =
            unsafe { std::slice::from_raw_parts_mut(iov.iov_base.cast::<u8>(), iov.iov_len) };
        let mut lengths = [0_usize; 1];
        let count = self.read_packet_batch(interface, buffer, buffer.len(), 1, &mut lengths)?;
        Ok((count == 1).then_some(lengths[0]))
    }

    fn write_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError> {
        let iov = packet.iov();
        // SAFETY: `VmnetWritePacket` owns an iovec whose immutable packet
        // bytes remain live for this call.
        let bytes = unsafe { std::slice::from_raw_parts(iov.iov_base.cast::<u8>(), iov.iov_len) };
        let packet_range = 0..bytes.len();
        let completed =
            self.write_packet_batch(interface, bytes, std::slice::from_ref(&packet_range))?;
        if completed == 1 {
            Ok(())
        } else {
            Err(VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::WritePackets,
                expected: VmnetPacketCountExpectation::One,
                actual: i32::try_from(completed).unwrap_or(i32::MAX),
            })
        }
    }

    fn read_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &mut [u8],
        packet_capacity: usize,
        requested_packets: usize,
        packet_lengths: &mut [usize],
    ) -> Result<usize, VmnetPacketIoError> {
        if interface.stopped
            || packet_capacity == 0
            || requested_packets == 0
            || packet_lengths.len() < requested_packets
            || packet_capacity
                .checked_mul(requested_packets)
                .is_none_or(|required| required > buffer.len())
        {
            return Err(VmnetPacketIoError::InvalidBatch {
                message: "remote vmnet read batch layout is invalid",
            });
        }
        let requested =
            u16::try_from(requested_packets).map_err(|_| VmnetPacketIoError::InvalidBatch {
                message: "remote vmnet read batch count is too large",
            })?;
        let packets = interface
            .data
            .read(requested)
            .map_err(|failure| failure.packet_io(VmnetOperation::ReadPackets))?;
        if packets.packet_count() > requested_packets {
            return Err(VmnetPacketIoError::InvalidBatch {
                message: "remote vmnet read result exceeds its request",
            });
        }
        for (index, packet_length) in packet_lengths
            .iter_mut()
            .take(packets.packet_count())
            .enumerate()
        {
            let packet = packets
                .packet(index)
                .ok_or(VmnetPacketIoError::InvalidBatch {
                    message: "remote vmnet read result is malformed",
                })?;
            if packet.len() > packet_capacity {
                return Err(VmnetPacketIoError::ReadPacketSizeExceedsBuffer {
                    packet_size: packet.len(),
                    buffer_len: packet_capacity,
                });
            }
            let start =
                index
                    .checked_mul(packet_capacity)
                    .ok_or(VmnetPacketIoError::InvalidBatch {
                        message: "remote vmnet read offset overflowed",
                    })?;
            let end = start
                .checked_add(packet.len())
                .ok_or(VmnetPacketIoError::InvalidBatch {
                    message: "remote vmnet read range overflowed",
                })?;
            buffer
                .get_mut(start..end)
                .ok_or(VmnetPacketIoError::InvalidBatch {
                    message: "remote vmnet read result exceeds the aggregate buffer",
                })?
                .copy_from_slice(packet);
            *packet_length = packet.len();
        }
        Ok(packets.packet_count())
    }

    fn write_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &[u8],
        packet_ranges: &[Range<usize>],
    ) -> Result<usize, VmnetPacketIoError> {
        if interface.stopped || packet_ranges.is_empty() {
            return Err(VmnetPacketIoError::InvalidBatch {
                message: "remote vmnet write batch layout is invalid",
            });
        }
        let mut packets = Vec::new();
        packets
            .try_reserve_exact(packet_ranges.len())
            .map_err(|_| VmnetPacketIoError::InvalidBatch {
                message: "remote vmnet write batch allocation failed",
            })?;
        let mut previous_end = 0;
        for range in packet_ranges {
            if range.start < previous_end {
                return Err(VmnetPacketIoError::InvalidBatch {
                    message: "remote vmnet write packet ranges overlap",
                });
            }
            packets.push(
                buffer
                    .get(range.clone())
                    .filter(|packet| !packet.is_empty())
                    .ok_or(VmnetPacketIoError::InvalidBatch {
                        message: "remote vmnet write packet range is invalid",
                    })?,
            );
            previous_end = range.end;
        }
        let batch =
            VmnetPacketBatch::write(&packets).map_err(|_| VmnetPacketIoError::InvalidBatch {
                message: "remote vmnet write batch exceeds protocol limits",
            })?;
        interface
            .data
            .write(batch)
            .map(usize::from)
            .map_err(|failure| failure.packet_io(VmnetOperation::WritePackets))
    }
}

struct RemoteStarted {
    interface: VmnetInterfaceId,
    parameters: RealizedVmnetParameters,
    data: DataPumpHandle,
}

impl fmt::Debug for RemoteStarted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteStarted(<owned>)")
    }
}

#[derive(Debug, Clone, Copy)]
struct RemoteFailure {
    status: VmnetStatus,
    cleanup_uncertain: bool,
}

impl RemoteFailure {
    const fn retryable(status: VmnetStatus) -> Self {
        Self {
            status,
            cleanup_uncertain: false,
        }
    }

    const fn terminal(status: VmnetStatus) -> Self {
        Self {
            status,
            cleanup_uncertain: true,
        }
    }

    const fn terminalized(mut self) -> Self {
        self.cleanup_uncertain = true;
        self
    }

    const fn vmnet(self, operation: VmnetOperation) -> VmnetError {
        VmnetError::new(operation, self.status)
    }

    fn packet_io(self, operation: VmnetOperation) -> VmnetPacketIoError {
        VmnetPacketIoError::Vmnet {
            source: self.vmnet(operation),
        }
    }
}

fn start_error(failure: RemoteFailure) -> VmnetInterfaceStartError {
    VmnetInterfaceStartError::start(
        failure.vmnet(VmnetOperation::StartInterface),
        if failure.cleanup_uncertain {
            VmnetInterfaceStartDisposition::Terminal
        } else {
            VmnetInterfaceStartDisposition::Retryable
        },
    )
}

fn provider_status(status: ProviderStatus) -> VmnetStatus {
    match status {
        ProviderStatus::PolicyDenied | ProviderStatus::NotAuthorized => VmnetStatus::NotAuthorized,
        ProviderStatus::SharingServiceBusy => VmnetStatus::SharingServiceBusy,
        ProviderStatus::InvalidArgument => VmnetStatus::InvalidArgument,
        ProviderStatus::MemoryFailure => VmnetStatus::MemoryFailure,
        ProviderStatus::PacketTooBig => VmnetStatus::PacketTooBig,
        ProviderStatus::BufferExhausted => VmnetStatus::BufferExhausted,
        ProviderStatus::TooManyPackets => VmnetStatus::TooManyPackets,
        ProviderStatus::SetupIncomplete => VmnetStatus::SetupIncomplete,
        ProviderStatus::ResourceLimit
        | ProviderStatus::BackendFailure
        | ProviderStatus::CleanupUncertain => VmnetStatus::Failure,
    }
}

fn protocol_failure(_error: VmnetProviderError, cleanup_uncertain: bool) -> RemoteFailure {
    if cleanup_uncertain {
        RemoteFailure::terminal(VmnetStatus::Failure)
    } else {
        RemoteFailure::retryable(VmnetStatus::Failure)
    }
}

struct WakeWriter {
    stream: Mutex<UnixStream>,
}

impl WakeWriter {
    fn signal(&self) {
        let mut stream = match self.stream.lock() {
            Ok(stream) => stream,
            Err(poisoned) => poisoned.into_inner(),
        };
        match stream.write(&[1]) {
            Ok(_) | Err(_) => {}
        }
    }
}

fn wake_pair() -> io::Result<(UnixStream, Arc<WakeWriter>)> {
    let (reader, writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    writer.set_nonblocking(true)?;
    Ok((
        reader,
        Arc::new(WakeWriter {
            stream: Mutex::new(writer),
        }),
    ))
}

fn drain_wake(stream: &mut UnixStream) {
    let mut bytes = [0_u8; 64];
    loop {
        match stream.read(&mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PumpEvent {
    Wake,
    Peer,
    Tick,
}

fn wait_for_pump_event(peer: &UnixStream, wake: &UnixStream) -> io::Result<PumpEvent> {
    let mut descriptors = [
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: peer.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let timeout = i32::try_from(PUMP_POLL_INTERVAL.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `descriptors` is a live two-element pollfd array for the
        // duration of this blocking call.
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, timeout) };
        if result > 0 {
            let terminal = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
            if descriptors[0].revents & (libc::POLLIN | terminal) != 0 {
                return Ok(PumpEvent::Wake);
            }
            if descriptors[1].revents & (libc::POLLIN | terminal) != 0 {
                return Ok(PumpEvent::Peer);
            }
        } else if result == 0 {
            return Ok(PumpEvent::Tick);
        } else {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

struct PumpLifetime {
    shutdown: Arc<AtomicBool>,
    cancel_reason: Option<Arc<AtomicU8>>,
    wake: Arc<WakeWriter>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for PumpLifetime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(reason) = &self.cancel_reason {
            let _ = reason.compare_exchange(
                CANCEL_NONE,
                CANCEL_WORKER,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        self.wake.signal();
        let worker = match self.worker.get_mut() {
            Ok(worker) => worker.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

struct CommandCompletion<T> {
    live: Arc<AtomicBool>,
    deadline: Instant,
    sender: mpsc::SyncSender<Result<T, RemoteFailure>>,
}

impl<T> CommandCompletion<T> {
    fn is_live(&self) -> bool {
        command_is_live(&self.live, self.deadline)
    }

    fn complete(self, result: Result<T, RemoteFailure>) -> bool {
        self.is_live() && self.sender.try_send(result).is_ok()
    }
}

struct CommandCallerGuard {
    live: Arc<AtomicBool>,
    wake: Arc<WakeWriter>,
    armed: bool,
}

impl CommandCallerGuard {
    fn finish(mut self) {
        self.abandon();
        self.armed = false;
    }

    fn abandon(&self) {
        self.live.store(false, Ordering::Release);
        self.wake.signal();
    }
}

impl Drop for CommandCallerGuard {
    fn drop(&mut self) {
        if self.armed {
            self.abandon();
        }
    }
}

fn command_is_live(live: &AtomicBool, deadline: Instant) -> bool {
    live.load(Ordering::Acquire) && Instant::now() < deadline
}

fn call_pump<T, C>(
    commands: &mpsc::SyncSender<C>,
    wake: &Arc<WakeWriter>,
    timeout_status: VmnetStatus,
    command: impl FnOnce(CommandCompletion<T>) -> C,
) -> Result<T, RemoteFailure> {
    let deadline = Instant::now()
        .checked_add(CALL_TIMEOUT)
        .ok_or_else(|| RemoteFailure::terminal(timeout_status))?;
    let live = Arc::new(AtomicBool::new(true));
    let (sender, receiver) = mpsc::sync_channel(1);
    commands
        .try_send(command(CommandCompletion {
            live: Arc::clone(&live),
            deadline,
            sender,
        }))
        .map_err(|error| match error {
            mpsc::TrySendError::Full(_) => RemoteFailure::retryable(VmnetStatus::BufferExhausted),
            mpsc::TrySendError::Disconnected(_) => RemoteFailure::terminal(VmnetStatus::Failure),
        })?;
    wake.signal();
    let guard = CommandCallerGuard {
        live,
        wake: Arc::clone(wake),
        armed: true,
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(RemoteFailure::terminal(timeout_status));
    }
    let result = receiver
        .recv_timeout(remaining)
        .map_err(|_| RemoteFailure::terminal(timeout_status))?;
    guard.finish();
    result
}

enum ControlCommand {
    Start {
        interface: VmnetInterfaceId,
        slot: VmnetPolicySlot,
        requested: RequestedVmnetParameters,
        completion: CommandCompletion<RemoteStarted>,
    },
    Stop {
        interface: VmnetInterfaceId,
        completion: CommandCompletion<()>,
    },
}

#[derive(Clone)]
struct ControlPumpHandle {
    commands: mpsc::SyncSender<ControlCommand>,
    lifetime: Arc<PumpLifetime>,
    cancel_reason: Arc<AtomicU8>,
}

impl ControlPumpHandle {
    fn connect(session: SessionId, stream: UnixStream) -> Result<Self, RemoteFailure> {
        let peer = stream
            .try_clone()
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), false))?;
        let (mut wake_reader, wake) = wake_pair()
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), false))?;
        let (commands, receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let cancel_reason = Arc::new(AtomicU8::new(CANCEL_NONE));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_cancel = Arc::clone(&cancel_reason);
        let worker = thread::Builder::new()
            .name("bangbang-vmnet-control".to_string())
            .spawn(move || {
                let result = run_control_pump(
                    session,
                    stream,
                    peer,
                    &mut wake_reader,
                    receiver,
                    &worker_shutdown,
                    &worker_cancel,
                    &ready_sender,
                );
                if let Err(error) = result {
                    let _ = ready_sender.try_send(Err(error));
                }
            })
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), false))?;
        let lifetime = Arc::new(PumpLifetime {
            shutdown,
            cancel_reason: Some(Arc::clone(&cancel_reason)),
            wake,
            worker: Mutex::new(Some(worker)),
        });
        let handle = Self {
            commands,
            lifetime,
            cancel_reason,
        };
        match ready_receiver.recv_timeout(CALL_TIMEOUT) {
            Ok(Ok(())) => Ok(handle),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(RemoteFailure::retryable(VmnetStatus::SetupIncomplete)),
        }
    }

    fn start(
        &self,
        interface: VmnetInterfaceId,
        slot: VmnetPolicySlot,
        requested: RequestedVmnetParameters,
    ) -> Result<RemoteStarted, RemoteFailure> {
        self.call(|completion| ControlCommand::Start {
            interface,
            slot,
            requested,
            completion,
        })
    }

    fn stop(&self, interface: VmnetInterfaceId) -> Result<(), RemoteFailure> {
        self.call(|completion| ControlCommand::Stop {
            interface,
            completion,
        })
    }

    fn call<T>(
        &self,
        command: impl FnOnce(CommandCompletion<T>) -> ControlCommand,
    ) -> Result<T, RemoteFailure> {
        call_pump(
            &self.commands,
            &self.lifetime.wake,
            VmnetStatus::SetupIncomplete,
            command,
        )
    }

    fn cancel(&self, reason: ProviderCancelReason) {
        let encoded = match reason {
            ProviderCancelReason::Worker => CANCEL_WORKER,
            ProviderCancelReason::Launcher => CANCEL_LAUNCHER,
        };
        if encoded == CANCEL_LAUNCHER {
            self.cancel_reason.store(encoded, Ordering::Release);
        } else {
            let _ = self.cancel_reason.compare_exchange(
                CANCEL_NONE,
                encoded,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        self.lifetime.wake.signal();
    }
}

#[allow(clippy::too_many_arguments)]
fn run_control_pump(
    session: SessionId,
    stream: UnixStream,
    peer: UnixStream,
    wake: &mut UnixStream,
    commands: mpsc::Receiver<ControlCommand>,
    shutdown: &AtomicBool,
    cancel_reason: &AtomicU8,
    ready: &mpsc::SyncSender<Result<(), RemoteFailure>>,
) -> Result<(), RemoteFailure> {
    let mut transport = VmnetProviderTransport::new(stream, MAX_PROVIDER_TIMEOUT)
        .map_err(|error| protocol_failure(error, false))?;
    let mut state =
        VmnetControlClient::new(session).map_err(|error| protocol_failure(error, false))?;
    transport
        .send(
            state
                .hello()
                .map_err(|error| protocol_failure(error, false))?,
        )
        .map_err(|error| protocol_failure(error, false))?;
    loop {
        match wait_for_pump_event(&peer, wake)
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), false))?
        {
            PumpEvent::Wake => {
                drain_wake(wake);
                if shutdown.load(Ordering::Acquire) {
                    return Ok(());
                }
            }
            PumpEvent::Peer => {
                let event = state
                    .receive(
                        transport
                            .receive()
                            .map_err(|error| protocol_failure(error, false))?,
                    )
                    .map_err(|error| protocol_failure(error, false))?;
                if matches!(event, ControlClientEvent::Ready) {
                    let _ = ready.try_send(Ok(()));
                    break;
                }
                return Err(RemoteFailure::retryable(VmnetStatus::Failure));
            }
            PumpEvent::Tick => {
                if shutdown.load(Ordering::Acquire) {
                    return Ok(());
                }
            }
        }
    }

    loop {
        if cancel_reason.load(Ordering::Acquire) != CANCEL_NONE || shutdown.load(Ordering::Acquire)
        {
            cancel_control_session(&mut state, &mut transport, &peer, wake, cancel_reason)?;
            return Ok(());
        }
        match commands.try_recv() {
            Ok(ControlCommand::Start {
                interface,
                slot,
                requested,
                completion,
            }) => {
                if !completion.is_live() {
                    return cancel_control_session(
                        &mut state,
                        &mut transport,
                        &peer,
                        wake,
                        cancel_reason,
                    );
                }
                let result = process_control_start(
                    session,
                    &mut state,
                    &mut transport,
                    &peer,
                    wake,
                    cancel_reason,
                    shutdown,
                    interface,
                    slot,
                    requested,
                    &completion.live,
                    completion.deadline,
                );
                let should_exit = result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.cleanup_uncertain)
                    || cancel_reason.load(Ordering::Acquire) != CANCEL_NONE
                    || shutdown.load(Ordering::Acquire);
                let published = completion.complete(result);
                if !published && !should_exit {
                    cancel_control_session(&mut state, &mut transport, &peer, wake, cancel_reason)?;
                    return Ok(());
                }
                if should_exit {
                    return Ok(());
                }
            }
            Ok(ControlCommand::Stop {
                interface,
                completion,
            }) => {
                if !completion.is_live() {
                    return cancel_control_session(
                        &mut state,
                        &mut transport,
                        &peer,
                        wake,
                        cancel_reason,
                    );
                }
                let result = process_control_stop(
                    &mut state,
                    &mut transport,
                    &peer,
                    wake,
                    cancel_reason,
                    shutdown,
                    interface,
                    &completion.live,
                    completion.deadline,
                );
                let should_exit = result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.cleanup_uncertain)
                    || cancel_reason.load(Ordering::Acquire) != CANCEL_NONE
                    || shutdown.load(Ordering::Acquire);
                let published = completion.complete(result);
                if !published && !should_exit {
                    cancel_control_session(&mut state, &mut transport, &peer, wake, cancel_reason)?;
                    return Ok(());
                }
                if should_exit {
                    return Ok(());
                }
            }
            Err(mpsc::TryRecvError::Empty) => match wait_for_pump_event(&peer, wake)
                .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
            {
                PumpEvent::Wake => drain_wake(wake),
                PumpEvent::Peer => {
                    let event = state
                        .receive(
                            transport
                                .receive()
                                .map_err(|error| protocol_failure(error, true))?,
                        )
                        .map_err(|error| protocol_failure(error, true))?;
                    if matches!(event, ControlClientEvent::PeerTerminal { .. }) {
                        return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                    }
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
                PumpEvent::Tick => {}
            },
            Err(mpsc::TryRecvError::Disconnected) => {
                cancel_control_session(&mut state, &mut transport, &peer, wake, cancel_reason)?;
                return Ok(());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_control_start(
    session: SessionId,
    state: &mut VmnetControlClient,
    transport: &mut VmnetProviderTransport,
    peer: &UnixStream,
    wake: &mut UnixStream,
    cancel_reason: &AtomicU8,
    shutdown: &AtomicBool,
    interface: VmnetInterfaceId,
    slot: VmnetPolicySlot,
    requested: RequestedVmnetParameters,
    live: &AtomicBool,
    deadline: Instant,
) -> Result<RemoteStarted, RemoteFailure> {
    if !command_is_live(live, deadline)
        || cancel_reason.load(Ordering::Acquire) != CANCEL_NONE
        || shutdown.load(Ordering::Acquire)
    {
        cancel_control_session(state, transport, peer, wake, cancel_reason)?;
        return Err(RemoteFailure::terminal(VmnetStatus::SetupIncomplete));
    }
    transport
        .send(
            state
                .start(interface, slot, requested)
                .map_err(|error| protocol_failure(error, false))?,
        )
        .map_err(|error| protocol_failure(error, false))?;
    let mut cancelling = false;
    let mut cancel_deadline = None;
    let mut cancellation_failure = None;
    loop {
        let cancelled = !command_is_live(live, deadline)
            || cancel_reason.load(Ordering::Acquire) != CANCEL_NONE
            || shutdown.load(Ordering::Acquire);
        if cancelled && !cancelling {
            let reason = decoded_cancel_reason(cancel_reason.load(Ordering::Acquire));
            transport
                .send(
                    state
                        .cancel(reason)
                        .map_err(|error| protocol_failure(error, true))?,
                )
                .map_err(|error| protocol_failure(error, true))?;
            cancelling = true;
            cancel_deadline = Instant::now().checked_add(MAX_PROVIDER_TIMEOUT);
        }
        if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(RemoteFailure::terminal(VmnetStatus::SetupIncomplete));
        }
        match wait_for_pump_event(peer, wake)
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
        {
            PumpEvent::Wake => drain_wake(wake),
            PumpEvent::Peer => {
                let event = state
                    .receive(
                        transport
                            .receive()
                            .map_err(|error| protocol_failure(error, true))?,
                    )
                    .map_err(|error| protocol_failure(error, true))?;
                match event {
                    ControlClientEvent::Started {
                        interface,
                        generation,
                        parameters,
                        stream,
                    } => {
                        if cancelled
                            || !command_is_live(live, deadline)
                            || cancel_reason.load(Ordering::Acquire) != CANCEL_NONE
                            || shutdown.load(Ordering::Acquire)
                        {
                            drop(stream);
                            if !cancelling {
                                transport
                                    .send(
                                        state
                                            .cancel(decoded_cancel_reason(
                                                cancel_reason.load(Ordering::Acquire),
                                            ))
                                            .map_err(|error| protocol_failure(error, true))?,
                                    )
                                    .map_err(|error| protocol_failure(error, true))?;
                                cancelling = true;
                                cancel_deadline = Instant::now().checked_add(MAX_PROVIDER_TIMEOUT);
                            }
                            continue;
                        }
                        let data = match DataPumpHandle::start(
                            session, interface, generation, parameters, stream,
                        ) {
                            Ok(data) => data,
                            Err(error) => {
                                transport
                                    .send(
                                        state
                                            .cancel(ProviderCancelReason::Worker)
                                            .map_err(|source| protocol_failure(source, true))?,
                                    )
                                    .map_err(|source| protocol_failure(source, true))?;
                                cancelling = true;
                                cancellation_failure = Some(error.terminalized());
                                cancel_deadline = Instant::now().checked_add(MAX_PROVIDER_TIMEOUT);
                                continue;
                            }
                        };
                        if command_is_live(live, deadline)
                            && cancel_reason.load(Ordering::Acquire) == CANCEL_NONE
                            && !shutdown.load(Ordering::Acquire)
                        {
                            return Ok(RemoteStarted {
                                interface,
                                parameters,
                                data,
                            });
                        }
                        drop(data);
                        transport
                            .send(
                                state
                                    .cancel(decoded_cancel_reason(
                                        cancel_reason.load(Ordering::Acquire),
                                    ))
                                    .map_err(|error| protocol_failure(error, true))?,
                            )
                            .map_err(|error| protocol_failure(error, true))?;
                        cancelling = true;
                        cancel_deadline = Instant::now().checked_add(MAX_PROVIDER_TIMEOUT);
                    }
                    ControlClientEvent::StartFailed { status, .. } if !cancelling => {
                        if command_is_live(live, deadline)
                            && cancel_reason.load(Ordering::Acquire) == CANCEL_NONE
                            && !shutdown.load(Ordering::Acquire)
                        {
                            return Err(if status == ProviderStatus::CleanupUncertain {
                                RemoteFailure::terminal(provider_status(status))
                            } else {
                                RemoteFailure::retryable(provider_status(status))
                            });
                        }
                        transport
                            .send(
                                state
                                    .cancel(decoded_cancel_reason(
                                        cancel_reason.load(Ordering::Acquire),
                                    ))
                                    .map_err(|error| protocol_failure(error, true))?,
                            )
                            .map_err(|error| protocol_failure(error, true))?;
                        cancelling = true;
                        cancel_deadline = Instant::now().checked_add(MAX_PROVIDER_TIMEOUT);
                    }
                    ControlClientEvent::StartRetiredDuringCancellation { .. }
                    | ControlClientEvent::StartFailedDuringCancellation { .. }
                    | ControlClientEvent::StoppedDuringCancellation { .. } => {}
                    ControlClientEvent::Cancelled => {
                        return Err(cancellation_failure.unwrap_or_else(|| {
                            RemoteFailure::terminal(VmnetStatus::SetupIncomplete)
                        }));
                    }
                    ControlClientEvent::PeerTerminal { .. } => {
                        return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                    }
                    _ => return Err(RemoteFailure::terminal(VmnetStatus::Failure)),
                }
            }
            PumpEvent::Tick => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_control_stop(
    state: &mut VmnetControlClient,
    transport: &mut VmnetProviderTransport,
    peer: &UnixStream,
    wake: &mut UnixStream,
    cancel_reason: &AtomicU8,
    shutdown: &AtomicBool,
    interface: VmnetInterfaceId,
    live: &AtomicBool,
    deadline: Instant,
) -> Result<(), RemoteFailure> {
    if !command_is_live(live, deadline)
        || cancel_reason.load(Ordering::Acquire) != CANCEL_NONE
        || shutdown.load(Ordering::Acquire)
    {
        cancel_control_session(state, transport, peer, wake, cancel_reason)?;
        return Err(RemoteFailure::terminal(VmnetStatus::Failure));
    }
    transport
        .send(
            state
                .stop(interface)
                .map_err(|error| protocol_failure(error, true))?,
        )
        .map_err(|error| protocol_failure(error, true))?;
    let mut cancelling = false;
    let mut cancel_deadline = None;
    loop {
        if (!command_is_live(live, deadline)
            || cancel_reason.load(Ordering::Acquire) != CANCEL_NONE
            || shutdown.load(Ordering::Acquire))
            && !cancelling
        {
            transport
                .send(
                    state
                        .cancel(decoded_cancel_reason(cancel_reason.load(Ordering::Acquire)))
                        .map_err(|error| protocol_failure(error, true))?,
                )
                .map_err(|error| protocol_failure(error, true))?;
            cancelling = true;
            cancel_deadline = Instant::now().checked_add(MAX_PROVIDER_TIMEOUT);
        }
        if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(RemoteFailure::terminal(VmnetStatus::Failure));
        }
        match wait_for_pump_event(peer, wake)
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
        {
            PumpEvent::Wake => drain_wake(wake),
            PumpEvent::Peer => match state
                .receive(
                    transport
                        .receive()
                        .map_err(|error| protocol_failure(error, true))?,
                )
                .map_err(|error| protocol_failure(error, true))?
            {
                ControlClientEvent::Stopped { .. }
                    if !cancelling
                        && command_is_live(live, deadline)
                        && cancel_reason.load(Ordering::Acquire) == CANCEL_NONE
                        && !shutdown.load(Ordering::Acquire) =>
                {
                    return Ok(());
                }
                ControlClientEvent::Stopped { .. } if !cancelling => {
                    transport
                        .send(
                            state
                                .cancel(decoded_cancel_reason(
                                    cancel_reason.load(Ordering::Acquire),
                                ))
                                .map_err(|error| protocol_failure(error, true))?,
                        )
                        .map_err(|error| protocol_failure(error, true))?;
                    cancelling = true;
                    cancel_deadline = Instant::now().checked_add(MAX_PROVIDER_TIMEOUT);
                }
                ControlClientEvent::StoppedDuringCancellation { .. } => {}
                ControlClientEvent::Cancelled => {
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
                ControlClientEvent::PeerTerminal { .. } => {
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
                _ => return Err(RemoteFailure::terminal(VmnetStatus::Failure)),
            },
            PumpEvent::Tick => {}
        }
    }
}

fn cancel_control_session(
    state: &mut VmnetControlClient,
    transport: &mut VmnetProviderTransport,
    peer: &UnixStream,
    wake: &mut UnixStream,
    cancel_reason: &AtomicU8,
) -> Result<(), RemoteFailure> {
    transport
        .send(
            state
                .cancel(decoded_cancel_reason(cancel_reason.load(Ordering::Acquire)))
                .map_err(|error| protocol_failure(error, true))?,
        )
        .map_err(|error| protocol_failure(error, true))?;
    let deadline = Instant::now()
        .checked_add(MAX_PROVIDER_TIMEOUT)
        .ok_or_else(|| RemoteFailure::terminal(VmnetStatus::Failure))?;
    loop {
        if Instant::now() >= deadline {
            return Err(RemoteFailure::terminal(VmnetStatus::Failure));
        }
        match wait_for_pump_event(peer, wake)
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
        {
            PumpEvent::Wake => drain_wake(wake),
            PumpEvent::Peer => match state
                .receive(
                    transport
                        .receive()
                        .map_err(|error| protocol_failure(error, true))?,
                )
                .map_err(|error| protocol_failure(error, true))?
            {
                ControlClientEvent::StartRetiredDuringCancellation { .. }
                | ControlClientEvent::StartFailedDuringCancellation { .. }
                | ControlClientEvent::StoppedDuringCancellation { .. } => {}
                ControlClientEvent::Cancelled => return Ok(()),
                ControlClientEvent::PeerTerminal { .. } => {
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
                _ => return Err(RemoteFailure::terminal(VmnetStatus::Failure)),
            },
            PumpEvent::Tick => {}
        }
    }
}

fn decoded_cancel_reason(encoded: u8) -> ProviderCancelReason {
    if encoded == CANCEL_LAUNCHER {
        ProviderCancelReason::Launcher
    } else {
        ProviderCancelReason::Worker
    }
}

enum DataCommand {
    Enable {
        callback: VmnetPacketAvailableCallback,
        completion: CommandCompletion<()>,
    },
    Disable {
        completion: CommandCompletion<()>,
    },
    Read {
        maximum: u16,
        completion: CommandCompletion<VmnetPacketBatch>,
    },
    Write {
        packets: VmnetPacketBatch,
        completion: CommandCompletion<u16>,
    },
    Stop {
        completion: CommandCompletion<()>,
    },
}

struct DataPumpHandle {
    commands: mpsc::SyncSender<DataCommand>,
    lifetime: Arc<PumpLifetime>,
}

impl DataPumpHandle {
    fn start(
        session: SessionId,
        interface: VmnetInterfaceId,
        generation: VmnetGeneration,
        parameters: RealizedVmnetParameters,
        stream: UnixStream,
    ) -> Result<Self, RemoteFailure> {
        let peer = stream
            .try_clone()
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?;
        let (mut wake_reader, wake) = wake_pair()
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?;
        let (commands, receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::Builder::new()
            .name("bangbang-vmnet-data".to_string())
            .spawn(move || {
                let result = run_data_pump(
                    session,
                    interface,
                    generation,
                    parameters,
                    stream,
                    peer,
                    &mut wake_reader,
                    receiver,
                    &worker_shutdown,
                    &ready_sender,
                );
                if let Err(error) = result {
                    let _ = ready_sender.try_send(Err(error));
                }
            })
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?;
        let lifetime = Arc::new(PumpLifetime {
            shutdown,
            cancel_reason: None,
            wake,
            worker: Mutex::new(Some(worker)),
        });
        let handle = Self { commands, lifetime };
        match ready_receiver.recv_timeout(CALL_TIMEOUT) {
            Ok(Ok(())) => Ok(handle),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(RemoteFailure::terminal(VmnetStatus::SetupIncomplete)),
        }
    }

    fn enable_callback(&self, callback: VmnetPacketAvailableCallback) -> Result<(), RemoteFailure> {
        self.call(|completion| DataCommand::Enable {
            callback,
            completion,
        })
    }

    fn disable_callback(&self) -> Result<(), RemoteFailure> {
        self.call(|completion| DataCommand::Disable { completion })
    }

    fn read(&self, maximum: u16) -> Result<VmnetPacketBatch, RemoteFailure> {
        self.call(|completion| DataCommand::Read {
            maximum,
            completion,
        })
    }

    fn write(&self, packets: VmnetPacketBatch) -> Result<u16, RemoteFailure> {
        self.call(|completion| DataCommand::Write {
            packets,
            completion,
        })
    }

    fn stop(&self) -> Result<(), RemoteFailure> {
        self.call(|completion| DataCommand::Stop { completion })
    }

    fn call<T>(
        &self,
        command: impl FnOnce(CommandCompletion<T>) -> DataCommand,
    ) -> Result<T, RemoteFailure> {
        call_pump(
            &self.commands,
            &self.lifetime.wake,
            VmnetStatus::Failure,
            command,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn run_data_pump(
    session: SessionId,
    interface: VmnetInterfaceId,
    generation: VmnetGeneration,
    parameters: RealizedVmnetParameters,
    stream: UnixStream,
    peer: UnixStream,
    wake: &mut UnixStream,
    commands: mpsc::Receiver<DataCommand>,
    shutdown: &AtomicBool,
    ready: &mpsc::SyncSender<Result<(), RemoteFailure>>,
) -> Result<(), RemoteFailure> {
    let mut transport = VmnetProviderTransport::new(stream, MAX_PROVIDER_TIMEOUT)
        .map_err(|error| protocol_failure(error, true))?;
    let mut state = VmnetDataClient::new(session, interface, generation, parameters)
        .map_err(|error| protocol_failure(error, true))?;
    transport
        .send(
            state
                .hello()
                .map_err(|error| protocol_failure(error, true))?,
        )
        .map_err(|error| protocol_failure(error, true))?;
    loop {
        match wait_for_pump_event(&peer, wake)
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
        {
            PumpEvent::Wake => {
                drain_wake(wake);
                if shutdown.load(Ordering::Acquire) {
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
            }
            PumpEvent::Peer => match state
                .receive(
                    transport
                        .receive()
                        .map_err(|error| protocol_failure(error, true))?,
                )
                .map_err(|error| protocol_failure(error, true))?
            {
                DataClientEvent::Ready => {
                    let _ = ready.try_send(Ok(()));
                    break;
                }
                _ => return Err(RemoteFailure::terminal(VmnetStatus::Failure)),
            },
            PumpEvent::Tick => {
                if shutdown.load(Ordering::Acquire) {
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
            }
        }
    }

    let mut callback: Option<VmnetPacketAvailableCallback> = None;
    let mut pending_readiness = None;
    loop {
        if shutdown.load(Ordering::Acquire) {
            let _ = state
                .terminal(ProviderTerminalCode::Supervisor)
                .and_then(|frame| transport.send(frame));
            return Err(RemoteFailure::terminal(VmnetStatus::Failure));
        }
        match commands.try_recv() {
            Ok(DataCommand::Enable {
                callback: requested,
                completion,
            }) => {
                if !completion.is_live() {
                    return Err(terminalize_data_session(&mut state, &mut transport));
                }
                if callback.is_some() {
                    if !completion
                        .complete(Err(RemoteFailure::retryable(VmnetStatus::InvalidArgument)))
                    {
                        return Err(terminalize_data_session(&mut state, &mut transport));
                    }
                } else if completion.complete(Ok(())) {
                    if let Some(estimate) = pending_readiness {
                        requested.publish(Some(estimate));
                    }
                    callback = Some(requested);
                } else {
                    return Err(terminalize_data_session(&mut state, &mut transport));
                }
            }
            Ok(DataCommand::Disable { completion }) => {
                if !completion.is_live() || !completion.complete(Ok(())) {
                    return Err(terminalize_data_session(&mut state, &mut transport));
                }
                drop(callback.take());
            }
            Ok(DataCommand::Read {
                maximum,
                completion,
            }) => {
                pending_readiness = None;
                let result = process_data_read(
                    &mut state,
                    &mut transport,
                    &peer,
                    wake,
                    shutdown,
                    &completion.live,
                    completion.deadline,
                    maximum,
                    callback.as_ref(),
                    &mut pending_readiness,
                );
                let terminal = result.is_err();
                let published = completion.complete(result);
                if terminal {
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
                if !published {
                    return Err(terminalize_data_session(&mut state, &mut transport));
                }
            }
            Ok(DataCommand::Write {
                packets,
                completion,
            }) => {
                let result = process_data_write(
                    &mut state,
                    &mut transport,
                    &peer,
                    wake,
                    shutdown,
                    &completion.live,
                    completion.deadline,
                    packets,
                    callback.as_ref(),
                    &mut pending_readiness,
                );
                let terminal = result.is_err();
                let published = completion.complete(result);
                if terminal {
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
                if !published {
                    return Err(terminalize_data_session(&mut state, &mut transport));
                }
            }
            Ok(DataCommand::Stop { completion }) => {
                if !completion.is_live() {
                    return Err(terminalize_data_session(&mut state, &mut transport));
                }
                drop(callback.take());
                let result = process_data_stop(
                    &mut state,
                    &mut transport,
                    &peer,
                    wake,
                    shutdown,
                    &completion.live,
                    completion.deadline,
                );
                let terminal = result.is_err();
                let published = completion.complete(result);
                if terminal || !published {
                    return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                }
                return Ok(());
            }
            Err(mpsc::TryRecvError::Empty) => match wait_for_pump_event(&peer, wake)
                .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
            {
                PumpEvent::Wake => drain_wake(wake),
                PumpEvent::Peer => match state
                    .receive(
                        transport
                            .receive()
                            .map_err(|error| protocol_failure(error, true))?,
                    )
                    .map_err(|error| protocol_failure(error, true))?
                {
                    DataClientEvent::Readiness {
                        estimated_packets, ..
                    } => publish_readiness(
                        callback.as_ref(),
                        &mut pending_readiness,
                        estimated_packets,
                    ),
                    DataClientEvent::PeerTerminal { .. } => {
                        return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                    }
                    _ => return Err(RemoteFailure::terminal(VmnetStatus::Failure)),
                },
                PumpEvent::Tick => {}
            },
            Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_data_read(
    state: &mut VmnetDataClient,
    transport: &mut VmnetProviderTransport,
    peer: &UnixStream,
    wake: &mut UnixStream,
    shutdown: &AtomicBool,
    live: &AtomicBool,
    deadline: Instant,
    maximum: u16,
    callback: Option<&VmnetPacketAvailableCallback>,
    pending_readiness: &mut Option<u64>,
) -> Result<VmnetPacketBatch, RemoteFailure> {
    require_data_command(state, transport, shutdown, live, deadline)?;
    transport
        .send(
            state
                .read(maximum)
                .map_err(|error| protocol_failure(error, false))?,
        )
        .map_err(|error| protocol_failure(error, true))?;
    loop {
        require_data_command(state, transport, shutdown, live, deadline)?;
        match wait_for_pump_event(peer, wake)
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
        {
            PumpEvent::Wake => drain_wake(wake),
            PumpEvent::Peer => {
                let event = state
                    .receive(
                        transport
                            .receive()
                            .map_err(|error| protocol_failure(error, true))?,
                    )
                    .map_err(|error| protocol_failure(error, true))?;
                require_data_command(state, transport, shutdown, live, deadline)?;
                match event {
                    DataClientEvent::Readiness {
                        estimated_packets, ..
                    } => publish_readiness(callback, pending_readiness, estimated_packets),
                    DataClientEvent::ReadComplete { packets } => return Ok(packets),
                    DataClientEvent::OperationFailed {
                        operation: ProviderOperation::Read,
                        status,
                    } => return Err(RemoteFailure::terminal(provider_status(status))),
                    DataClientEvent::PeerTerminal { .. } => {
                        return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                    }
                    _ => return Err(RemoteFailure::terminal(VmnetStatus::Failure)),
                }
            }
            PumpEvent::Tick => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_data_write(
    state: &mut VmnetDataClient,
    transport: &mut VmnetProviderTransport,
    peer: &UnixStream,
    wake: &mut UnixStream,
    shutdown: &AtomicBool,
    live: &AtomicBool,
    deadline: Instant,
    packets: VmnetPacketBatch,
    callback: Option<&VmnetPacketAvailableCallback>,
    pending_readiness: &mut Option<u64>,
) -> Result<u16, RemoteFailure> {
    require_data_command(state, transport, shutdown, live, deadline)?;
    transport
        .send(
            state
                .write(packets)
                .map_err(|error| protocol_failure(error, false))?,
        )
        .map_err(|error| protocol_failure(error, true))?;
    loop {
        require_data_command(state, transport, shutdown, live, deadline)?;
        match wait_for_pump_event(peer, wake)
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
        {
            PumpEvent::Wake => drain_wake(wake),
            PumpEvent::Peer => {
                let event = state
                    .receive(
                        transport
                            .receive()
                            .map_err(|error| protocol_failure(error, true))?,
                    )
                    .map_err(|error| protocol_failure(error, true))?;
                require_data_command(state, transport, shutdown, live, deadline)?;
                match event {
                    DataClientEvent::Readiness {
                        estimated_packets, ..
                    } => publish_readiness(callback, pending_readiness, estimated_packets),
                    DataClientEvent::WriteComplete { completed_packets } => {
                        return Ok(completed_packets);
                    }
                    DataClientEvent::OperationFailed {
                        operation: ProviderOperation::Write,
                        status,
                    } => return Err(RemoteFailure::terminal(provider_status(status))),
                    DataClientEvent::PeerTerminal { .. } => {
                        return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                    }
                    _ => return Err(RemoteFailure::terminal(VmnetStatus::Failure)),
                }
            }
            PumpEvent::Tick => {}
        }
    }
}

fn process_data_stop(
    state: &mut VmnetDataClient,
    transport: &mut VmnetProviderTransport,
    peer: &UnixStream,
    wake: &mut UnixStream,
    shutdown: &AtomicBool,
    live: &AtomicBool,
    deadline: Instant,
) -> Result<(), RemoteFailure> {
    require_data_command(state, transport, shutdown, live, deadline)?;
    transport
        .send(
            state
                .stop()
                .map_err(|error| protocol_failure(error, true))?,
        )
        .map_err(|error| protocol_failure(error, true))?;
    loop {
        require_data_command(state, transport, shutdown, live, deadline)?;
        match wait_for_pump_event(peer, wake)
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
        {
            PumpEvent::Wake => drain_wake(wake),
            PumpEvent::Peer => {
                let event = state
                    .receive(
                        transport
                            .receive()
                            .map_err(|error| protocol_failure(error, true))?,
                    )
                    .map_err(|error| protocol_failure(error, true))?;
                require_data_command(state, transport, shutdown, live, deadline)?;
                match event {
                    DataClientEvent::Readiness { .. } => {}
                    DataClientEvent::Stopped => break,
                    DataClientEvent::PeerTerminal { .. } => {
                        return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                    }
                    _ => return Err(RemoteFailure::terminal(VmnetStatus::Failure)),
                }
            }
            PumpEvent::Tick => {}
        }
    }
    transport
        .send(
            state
                .shutdown()
                .map_err(|error| protocol_failure(error, true))?,
        )
        .map_err(|error| protocol_failure(error, true))?;
    loop {
        require_data_command(state, transport, shutdown, live, deadline)?;
        match wait_for_pump_event(peer, wake)
            .map_err(|error| protocol_failure(VmnetProviderError::Io(error.kind()), true))?
        {
            PumpEvent::Wake => drain_wake(wake),
            PumpEvent::Peer => {
                let event = state
                    .receive(
                        transport
                            .receive()
                            .map_err(|error| protocol_failure(error, true))?,
                    )
                    .map_err(|error| protocol_failure(error, true))?;
                require_data_command(state, transport, shutdown, live, deadline)?;
                match event {
                    DataClientEvent::Shutdown => return Ok(()),
                    DataClientEvent::PeerTerminal { .. } => {
                        return Err(RemoteFailure::terminal(VmnetStatus::Failure));
                    }
                    _ => return Err(RemoteFailure::terminal(VmnetStatus::Failure)),
                }
            }
            PumpEvent::Tick => {}
        }
    }
}

fn require_data_command(
    state: &mut VmnetDataClient,
    transport: &mut VmnetProviderTransport,
    shutdown: &AtomicBool,
    live: &AtomicBool,
    deadline: Instant,
) -> Result<(), RemoteFailure> {
    if shutdown.load(Ordering::Acquire) || !command_is_live(live, deadline) {
        return Err(terminalize_data_session(state, transport));
    }
    Ok(())
}

fn terminalize_data_session(
    state: &mut VmnetDataClient,
    transport: &mut VmnetProviderTransport,
) -> RemoteFailure {
    let _ = state
        .terminal(ProviderTerminalCode::Supervisor)
        .and_then(|frame| transport.send(frame));
    RemoteFailure::terminal(VmnetStatus::Failure)
}

fn publish_readiness(
    callback: Option<&VmnetPacketAvailableCallback>,
    pending: &mut Option<u64>,
    estimated_packets: u16,
) {
    let estimate = u64::from(estimated_packets);
    *pending = Some(estimate);
    if let Some(callback) = callback {
        callback.publish(Some(estimate));
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::sync::MutexGuard;

    use bangbang_session::vmnet_provider::{
        ControlBrokerEvent, DataOwnerEvent, ProviderCleanup, VmnetControlBroker, VmnetDataOwner,
    };

    use super::*;

    // Keep independent in-process descriptor fixtures from overlapping. The
    // two-data-pump case below remains the explicit concurrency proof.
    static PUMP_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_pump_tests() -> MutexGuard<'static, ()> {
        match PUMP_TEST_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn session() -> SessionId {
        SessionId::from_bytes([73; 32])
    }

    fn realized() -> RealizedVmnetParameters {
        RealizedVmnetParameters::new([2, 0, 0, 0, 0, 73], 1500, 2048)
            .expect("test parameters should validate")
            .with_batch_limits(Some(4), Some(4))
            .expect("test batch limits should validate")
    }

    fn spawn_data_owner(
        stream: UnixStream,
        interface: VmnetInterfaceId,
        generation: VmnetGeneration,
        parameters: RealizedVmnetParameters,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut transport = VmnetProviderTransport::new(stream, MAX_PROVIDER_TIMEOUT)
                .expect("data owner transport should construct");
            let mut owner = VmnetDataOwner::new(session(), interface, generation, parameters)
                .expect("data owner should construct");
            let event = owner
                .receive(transport.receive().expect("data hello should be received"))
                .expect("data hello should validate");
            assert!(matches!(event, DataOwnerEvent::Hello));
            transport
                .send(owner.hello_ack().expect("hello ack should construct"))
                .expect("hello ack should send");

            loop {
                let event = owner
                    .receive(
                        transport
                            .receive()
                            .expect("data command should be received"),
                    )
                    .expect("data command should validate");
                match event {
                    DataOwnerEvent::Read { .. } => {
                        transport
                            .send(owner.readiness(1).expect("readiness should construct"))
                            .expect("readiness should send");
                        let packet = [0xa5_u8; 60];
                        let packets = VmnetPacketBatch::read(&[&packet])
                            .expect("read result should validate");
                        transport
                            .send(
                                owner
                                    .read_result(packets)
                                    .expect("read result should construct"),
                            )
                            .expect("read result should send");
                    }
                    DataOwnerEvent::Write { packets, .. } => {
                        let completed = u16::try_from(packets.packet_count())
                            .expect("test packet count should fit");
                        assert_eq!(packets.packet(0), Some(&[0x5a_u8; 60][..]));
                        transport
                            .send(
                                owner
                                    .write_result(completed)
                                    .expect("write result should construct"),
                            )
                            .expect("write result should send");
                    }
                    DataOwnerEvent::Stop => {
                        transport
                            .send(
                                owner
                                    .stopped(ProviderCleanup::Complete)
                                    .expect("stopped should construct"),
                            )
                            .expect("stopped should send");
                    }
                    DataOwnerEvent::Shutdown => {
                        transport
                            .send(owner.shutdown_ack().expect("shutdown ack should construct"))
                            .expect("shutdown ack should send");
                        return;
                    }
                    DataOwnerEvent::PeerTerminal { .. } => return,
                    DataOwnerEvent::Hello => panic!("duplicate data hello"),
                }
            }
        })
    }

    fn spawn_normal_broker(stream: UnixStream) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut transport = VmnetProviderTransport::new(stream, MAX_PROVIDER_TIMEOUT)
                .expect("control broker transport should construct");
            let mut broker =
                VmnetControlBroker::new(session()).expect("control broker should construct");
            let hello = broker
                .receive(
                    transport
                        .receive()
                        .expect("control hello should be received"),
                )
                .expect("control hello should validate");
            assert!(matches!(hello, ControlBrokerEvent::Hello));
            transport
                .send(broker.hello_ack().expect("hello ack should construct"))
                .expect("hello ack should send");

            let mut data_owner = None;
            loop {
                let event = broker
                    .receive(
                        transport
                            .receive()
                            .expect("control command should be received"),
                    )
                    .expect("control command should validate");
                match event {
                    ControlBrokerEvent::Start {
                        interface,
                        policy_slot,
                        requested,
                    } => {
                        assert_eq!(policy_slot, VmnetPolicySlot::Shared);
                        assert_eq!(requested.mtu(), Some(1500));
                        let generation =
                            VmnetGeneration::new(1).expect("generation should validate");
                        let parameters = realized();
                        let (client, owner) =
                            UnixStream::pair().expect("data stream pair should construct");
                        data_owner =
                            Some(spawn_data_owner(owner, interface, generation, parameters));
                        transport
                            .send(
                                broker
                                    .started(generation, parameters, client)
                                    .expect("started should construct"),
                            )
                            .expect("started should send");
                    }
                    ControlBrokerEvent::Stop { .. } => {
                        data_owner
                            .take()
                            .expect("data owner should exist")
                            .join()
                            .expect("data owner should finish");
                        transport
                            .send(
                                broker
                                    .stopped(ProviderCleanup::Complete)
                                    .expect("stopped should construct"),
                            )
                            .expect("stopped should send");
                    }
                    ControlBrokerEvent::Cancel { .. } => {
                        if let Some(owner) = data_owner.take() {
                            owner.join().expect("data owner should finish");
                        }
                        transport
                            .send(
                                broker
                                    .cancelled(ProviderCleanup::Complete)
                                    .expect("cancelled should construct"),
                            )
                            .expect("cancelled should send");
                        return;
                    }
                    ControlBrokerEvent::Shutdown => {
                        transport
                            .send(
                                broker
                                    .shutdown_ack()
                                    .expect("shutdown ack should construct"),
                            )
                            .expect("shutdown ack should send");
                        return;
                    }
                    ControlBrokerEvent::PeerTerminal { .. } => return,
                    ControlBrokerEvent::Hello => panic!("duplicate control hello"),
                }
            }
        })
    }

    fn spawn_control_cancel_observer(
        stream: UnixStream,
        expected_reason: ProviderCancelReason,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut transport = VmnetProviderTransport::new(stream, MAX_PROVIDER_TIMEOUT)
                .expect("control broker transport should construct");
            let mut broker =
                VmnetControlBroker::new(session()).expect("control broker should construct");
            assert!(matches!(
                broker
                    .receive(transport.receive().expect("hello should receive"))
                    .expect("hello should validate"),
                ControlBrokerEvent::Hello
            ));
            transport
                .send(broker.hello_ack().expect("hello ack should construct"))
                .expect("hello ack should send");
            assert!(matches!(
                broker
                    .receive(transport.receive().expect("cancel should receive"))
                    .expect("cancel should validate"),
                ControlBrokerEvent::Cancel { reason } if reason == expected_reason
            ));
            transport
                .send(
                    broker
                        .cancelled(ProviderCleanup::Complete)
                        .expect("cancelled should construct"),
                )
                .expect("cancelled should send");
        })
    }

    fn assert_expired_data_command<T>(command: impl FnOnce(CommandCompletion<T>) -> DataCommand) {
        let expired_session = SessionId::from_bytes([74; 32]);
        let interface = VmnetInterfaceId::new(1).expect("interface should validate");
        let generation = VmnetGeneration::new(1).expect("generation should validate");
        let parameters = realized();
        let (client, owner_stream) = UnixStream::pair().expect("data pair should construct");
        let owner = thread::spawn(move || {
            let mut transport = VmnetProviderTransport::new(owner_stream, MAX_PROVIDER_TIMEOUT)
                .expect("data owner transport should construct");
            let mut state = VmnetDataOwner::new(expired_session, interface, generation, parameters)
                .expect("data owner should construct");
            assert!(matches!(
                state
                    .receive(transport.receive().expect("hello should receive"))
                    .expect("hello should validate"),
                DataOwnerEvent::Hello
            ));
            transport
                .send(state.hello_ack().expect("hello ack should construct"))
                .expect("hello ack should send");
            assert!(matches!(
                state
                    .receive(transport.receive().expect("terminal should receive"))
                    .expect("terminal should validate"),
                DataOwnerEvent::PeerTerminal {
                    code: ProviderTerminalCode::Supervisor
                }
            ));
        });
        let data =
            DataPumpHandle::start(expired_session, interface, generation, parameters, client)
                .expect("data pump should start");
        let (sender, receiver) = mpsc::sync_channel(1);
        data.commands
            .try_send(command(CommandCompletion {
                live: Arc::new(AtomicBool::new(true)),
                deadline: Instant::now()
                    .checked_sub(Duration::from_millis(1))
                    .expect("past deadline should construct"),
                sender,
            }))
            .expect("expired command should enqueue");
        data.lifetime.wake.signal();

        owner.join().expect("data owner should observe terminal");
        drop(data);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn remote_pumps_route_readiness_packets_and_ordered_cleanup() {
        let _test_guard = lock_pump_tests();
        let (client, broker) = UnixStream::pair().expect("control pair should construct");
        let broker = spawn_normal_broker(broker);
        let control =
            ControlPumpHandle::connect(session(), client).expect("control should connect");
        let interface = VmnetInterfaceId::new(1).expect("interface should validate");
        let requested =
            RequestedVmnetParameters::new(None, Some(1500)).expect("request should validate");
        let started = control
            .start(interface, VmnetPolicySlot::Shared, requested)
            .expect("remote interface should start");
        assert_eq!(started.parameters, realized());

        let (readiness_sender, readiness_receiver) = mpsc::sync_channel(1);
        started
            .data
            .enable_callback(VmnetPacketAvailableCallback::new(move |estimate| {
                let _ = readiness_sender.try_send(estimate);
            }))
            .expect("callback should enable");
        let write_packet = [0x5a_u8; 60];
        let write = VmnetPacketBatch::write(&[&write_packet]).expect("write should validate");
        assert_eq!(started.data.write(write).expect("write should complete"), 1);
        let read = started.data.read(1).expect("read should complete");
        assert_eq!(read.packet(0), Some(&[0xa5_u8; 60][..]));
        assert_eq!(
            readiness_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("readiness should publish"),
            Some(1)
        );
        started
            .data
            .disable_callback()
            .expect("callback should drain");
        started.data.stop().expect("data stream should stop");
        drop(started.data);
        control
            .stop(started.interface)
            .expect("control interface should stop");
        drop(control);
        broker.join().expect("broker should finish");
    }

    #[test]
    fn one_control_pump_owns_two_independent_data_pumps() {
        let _test_guard = lock_pump_tests();
        let (client, broker_stream) = UnixStream::pair().expect("control pair should construct");
        let broker = thread::spawn(move || {
            let mut transport = VmnetProviderTransport::new(broker_stream, MAX_PROVIDER_TIMEOUT)
                .expect("control broker transport should construct");
            let mut state =
                VmnetControlBroker::new(session()).expect("control broker should construct");
            assert!(matches!(
                state
                    .receive(transport.receive().expect("hello should receive"))
                    .expect("hello should validate"),
                ControlBrokerEvent::Hello
            ));
            transport
                .send(state.hello_ack().expect("hello ack should construct"))
                .expect("hello ack should send");
            let mut owners = Vec::new();
            let mut next_generation = 1_u64;
            loop {
                match state
                    .receive(transport.receive().expect("control event should receive"))
                    .expect("control event should validate")
                {
                    ControlBrokerEvent::Start { interface, .. } => {
                        let generation = VmnetGeneration::new(next_generation)
                            .expect("generation should validate");
                        next_generation = next_generation
                            .checked_add(1)
                            .expect("test generation should advance");
                        let parameters = realized();
                        let (data_client, data_owner) =
                            UnixStream::pair().expect("data pair should construct");
                        owners.push((
                            interface,
                            spawn_data_owner(data_owner, interface, generation, parameters),
                        ));
                        transport
                            .send(
                                state
                                    .started(generation, parameters, data_client)
                                    .expect("started should construct"),
                            )
                            .expect("started should send");
                    }
                    ControlBrokerEvent::Stop { interface, .. } => {
                        let owner = owners
                            .iter()
                            .position(|(owned, _)| *owned == interface)
                            .map(|index| owners.swap_remove(index).1)
                            .expect("matching data owner should exist");
                        owner.join().expect("data owner should finish");
                        transport
                            .send(
                                state
                                    .stopped(ProviderCleanup::Complete)
                                    .expect("stopped should construct"),
                            )
                            .expect("stopped should send");
                    }
                    ControlBrokerEvent::Cancel { .. } => {
                        assert!(owners.is_empty());
                        transport
                            .send(
                                state
                                    .cancelled(ProviderCleanup::Complete)
                                    .expect("cancelled should construct"),
                            )
                            .expect("cancelled should send");
                        return;
                    }
                    event => panic!("unexpected control event: {event:?}"),
                }
            }
        });
        let control =
            ControlPumpHandle::connect(session(), client).expect("control should connect");
        let request =
            || RequestedVmnetParameters::new(None, Some(1500)).expect("request should validate");
        let first = control
            .start(
                VmnetInterfaceId::new(1).expect("interface should validate"),
                VmnetPolicySlot::Shared,
                request(),
            )
            .expect("first interface should start");
        let second = control
            .start(
                VmnetInterfaceId::new(2).expect("interface should validate"),
                VmnetPolicySlot::Shared,
                request(),
            )
            .expect("second interface should start");
        let packet = [0x5a_u8; 60];
        for data in [&first.data, &second.data] {
            assert_eq!(
                data.write(VmnetPacketBatch::write(&[&packet]).expect("write should validate"))
                    .expect("write should complete"),
                1
            );
            assert_eq!(
                data.read(1).expect("read should complete").packet(0),
                Some(&[0xa5_u8; 60][..])
            );
        }
        first.data.stop().expect("first data pump should stop");
        second.data.stop().expect("second data pump should stop");
        drop(first.data);
        drop(second.data);
        control
            .stop(first.interface)
            .expect("first interface should retire");
        control
            .stop(second.interface)
            .expect("second interface should retire");
        drop(control);
        broker.join().expect("broker should finish");
    }

    #[test]
    fn launcher_cancellation_retires_a_raced_started_stream() {
        let _test_guard = lock_pump_tests();
        let (client, broker_stream) = UnixStream::pair().expect("control pair should construct");
        let (start_seen_sender, start_seen_receiver) = mpsc::sync_channel(1);
        let broker = thread::spawn(move || {
            let mut transport = VmnetProviderTransport::new(broker_stream, MAX_PROVIDER_TIMEOUT)
                .expect("broker transport should construct");
            let mut state =
                VmnetControlBroker::new(session()).expect("broker state should construct");
            assert!(matches!(
                state
                    .receive(transport.receive().expect("hello should receive"))
                    .expect("hello should validate"),
                ControlBrokerEvent::Hello
            ));
            transport
                .send(state.hello_ack().expect("hello ack should construct"))
                .expect("hello ack should send");
            let start = state
                .receive(transport.receive().expect("start should receive"))
                .expect("start should validate");
            let ControlBrokerEvent::Start { interface, .. } = start else {
                panic!("start should be requested");
            };
            start_seen_sender.send(()).expect("start should publish");
            assert!(matches!(
                state
                    .receive(transport.receive().expect("cancel should receive"))
                    .expect("cancel should validate"),
                ControlBrokerEvent::Cancel {
                    reason: ProviderCancelReason::Launcher
                }
            ));
            let generation = VmnetGeneration::new(1).expect("generation should validate");
            let (client_data, mut owner_data) =
                UnixStream::pair().expect("raced data pair should construct");
            transport
                .send(
                    state
                        .started(generation, realized(), client_data)
                        .expect("raced started should construct"),
                )
                .expect("raced started should send");
            transport
                .send(
                    state
                        .cancelled(ProviderCleanup::Complete)
                        .expect("cancelled should construct"),
                )
                .expect("cancelled should send");
            let mut descriptor = libc::pollfd {
                fd: owner_data.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `descriptor` is one live poll entry whose stream remains
            // owned through the bounded wait.
            assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 1_000) }, 1);
            let mut byte = [0_u8; 1];
            assert_eq!(
                owner_data
                    .read(&mut byte)
                    .expect("retired stream should close"),
                0
            );
            interface
        });

        let control =
            ControlPumpHandle::connect(session(), client).expect("control should connect");
        let caller = control.clone();
        let start = thread::spawn(move || {
            caller.start(
                VmnetInterfaceId::new(1).expect("interface should validate"),
                VmnetPolicySlot::Shared,
                RequestedVmnetParameters::new(None, None).expect("request should validate"),
            )
        });
        start_seen_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("broker should observe start");
        control.cancel(ProviderCancelReason::Launcher);
        assert!(
            start
                .join()
                .expect("start caller should finish")
                .expect_err("cancelled start should fail")
                .cleanup_uncertain
        );
        drop(control);
        assert_eq!(
            broker.join().expect("broker should finish"),
            VmnetInterfaceId::new(1).expect("interface should validate")
        );
    }

    #[test]
    fn launcher_invalidation_closes_a_connecting_provider_stream() {
        let _test_guard = lock_pump_tests();
        let (client, mut provider) = UnixStream::pair().expect("control pair should construct");
        let inner = RemoteProviderSourceInner {
            session: session(),
            authority: VmnetAuthority::try_new(false, true, 1, &[])
                .expect("authority should validate"),
            grant: Mutex::new(RemoteProviderGrantState::Connecting(client)),
            next_interface: AtomicU32::new(1),
            cancel_reason: AtomicU8::new(CANCEL_NONE),
        };

        cancel_remote_source_inner(&inner, ProviderCancelReason::Launcher);
        assert_eq!(inner.cancel_reason.load(Ordering::Acquire), CANCEL_LAUNCHER);
        assert!(matches!(
            *inner.grant.lock().expect("source state should lock"),
            RemoteProviderGrantState::Terminal
        ));
        let mut descriptor = libc::pollfd {
            fd: provider.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `descriptor` is one live poll entry whose stream remains
        // owned through the bounded wait.
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 1_000) }, 1);
        let mut byte = [0_u8; 1];
        assert_eq!(
            provider
                .read(&mut byte)
                .expect("cancelled connecting stream should close"),
            0
        );
    }

    #[test]
    fn launcher_invalidation_terminalizes_a_ready_source_before_new_calls() {
        let _test_guard = lock_pump_tests();
        let (client, broker_stream) = UnixStream::pair().expect("control pair should construct");
        let broker = spawn_normal_broker(broker_stream);
        let control =
            ControlPumpHandle::connect(session(), client).expect("control should connect");
        let inner = RemoteProviderSourceInner {
            session: session(),
            authority: VmnetAuthority::try_new(false, true, 1, &[])
                .expect("authority should validate"),
            grant: Mutex::new(RemoteProviderGrantState::Ready(control)),
            next_interface: AtomicU32::new(1),
            cancel_reason: AtomicU8::new(CANCEL_NONE),
        };

        cancel_remote_source_inner(&inner, ProviderCancelReason::Launcher);

        assert_eq!(inner.cancel_reason.load(Ordering::Acquire), CANCEL_LAUNCHER);
        assert!(matches!(
            *inner.grant.lock().expect("source state should lock"),
            RemoteProviderGrantState::Terminal
        ));
        broker.join().expect("cancelled broker should finish");
    }

    #[test]
    fn expired_control_start_cancels_without_publishing_a_start() {
        let _test_guard = lock_pump_tests();
        let (client, broker_stream) = UnixStream::pair().expect("control pair should construct");
        let broker = spawn_control_cancel_observer(broker_stream, ProviderCancelReason::Worker);
        let control =
            ControlPumpHandle::connect(session(), client).expect("control should connect");
        let (sender, receiver) = mpsc::sync_channel(1);
        control
            .commands
            .try_send(ControlCommand::Start {
                interface: VmnetInterfaceId::new(1).expect("interface should validate"),
                slot: VmnetPolicySlot::Shared,
                requested: RequestedVmnetParameters::new(None, None)
                    .expect("request should validate"),
                completion: CommandCompletion {
                    live: Arc::new(AtomicBool::new(true)),
                    deadline: Instant::now()
                        .checked_sub(Duration::from_millis(1))
                        .expect("past deadline should construct"),
                    sender,
                },
            })
            .expect("expired command should enqueue");
        control.lifetime.wake.signal();

        broker.join().expect("broker should finish cancellation");
        drop(control);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn expired_control_stop_cancels_without_publishing_a_stop() {
        let _test_guard = lock_pump_tests();
        let (client, broker_stream) = UnixStream::pair().expect("control pair should construct");
        let broker = spawn_control_cancel_observer(broker_stream, ProviderCancelReason::Worker);
        let control =
            ControlPumpHandle::connect(session(), client).expect("control should connect");
        let (sender, receiver) = mpsc::sync_channel(1);
        control
            .commands
            .try_send(ControlCommand::Stop {
                interface: VmnetInterfaceId::new(1).expect("interface should validate"),
                completion: CommandCompletion {
                    live: Arc::new(AtomicBool::new(true)),
                    deadline: Instant::now()
                        .checked_sub(Duration::from_millis(1))
                        .expect("past deadline should construct"),
                    sender,
                },
            })
            .expect("expired command should enqueue");
        control.lifetime.wake.signal();

        broker.join().expect("broker should finish cancellation");
        drop(control);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn expired_data_commands_terminalize_without_publishing_operations() {
        let _test_guard = lock_pump_tests();
        assert_expired_data_command(|completion| DataCommand::Read {
            maximum: 1,
            completion,
        });
        let packet = [0x5a_u8; 60];
        let packets = VmnetPacketBatch::write(&[&packet]).expect("write batch should validate");
        assert_expired_data_command(|completion| DataCommand::Write {
            packets,
            completion,
        });
        assert_expired_data_command(|completion| DataCommand::Stop { completion });
    }

    #[test]
    fn dropping_the_last_control_handle_uses_worker_cancellation() {
        let _test_guard = lock_pump_tests();
        let (client, broker_stream) = UnixStream::pair().expect("control pair should construct");
        let broker = spawn_control_cancel_observer(broker_stream, ProviderCancelReason::Worker);
        let control =
            ControlPumpHandle::connect(session(), client).expect("control should connect");

        drop(control);

        broker
            .join()
            .expect("broker should observe worker cancellation");
    }

    #[test]
    fn full_command_queue_reports_retryable_backpressure_without_waiting() {
        let (commands, _receiver) = mpsc::sync_channel(1);
        let (_wake_reader, wake) = wake_pair().expect("wake pair should construct");
        let (occupied_sender, _occupied_receiver) = mpsc::sync_channel(1);
        commands
            .try_send(ControlCommand::Stop {
                interface: VmnetInterfaceId::new(1).expect("interface should validate"),
                completion: CommandCompletion {
                    live: Arc::new(AtomicBool::new(true)),
                    deadline: Instant::now()
                        .checked_add(CALL_TIMEOUT)
                        .expect("future deadline should construct"),
                    sender: occupied_sender,
                },
            })
            .expect("first command should fill the bounded queue");

        let failure = call_pump(
            &commands,
            &wake,
            VmnetStatus::SetupIncomplete,
            |completion| ControlCommand::Stop {
                interface: VmnetInterfaceId::new(2).expect("interface should validate"),
                completion,
            },
        )
        .expect_err("full queue should reject immediately");

        assert_eq!(failure.status, VmnetStatus::BufferExhausted);
        assert!(!failure.cleanup_uncertain);
    }

    #[test]
    fn policy_mapping_uses_only_authenticated_fixed_slots() {
        let authority = VmnetAuthority::try_new(true, true, 4, &["en0", "en7"])
            .expect("authority should validate");
        assert_eq!(
            policy_slot(authority, &VmnetInterfaceConfig::host()).expect("host should map"),
            VmnetPolicySlot::Host
        );
        assert_eq!(
            policy_slot(authority, &VmnetInterfaceConfig::shared()).expect("shared should map"),
            VmnetPolicySlot::Shared
        );
        assert_eq!(
            policy_slot(
                authority,
                &VmnetInterfaceConfig::bridged("en7").expect("bridge should validate"),
            )
            .expect("bridge should map"),
            VmnetPolicySlot::Bridge1
        );
        assert_eq!(
            policy_slot(
                authority,
                &VmnetInterfaceConfig::bridged("en8").expect("bridge should validate"),
            )
            .expect_err("unknown bridge should be denied")
            .status,
            VmnetStatus::NotAuthorized
        );
    }
}
