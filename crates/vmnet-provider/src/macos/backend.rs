use std::ops::Range;

use bangbang::host_network::vmnet::{
    StartedVmnetPacketIoBackend, StartedVmnetPacketIoInterface, SystemVmnetInterfaceBackend,
    VmnetError, VmnetInterfaceConfig, VmnetInterfaceStartDisposition, VmnetInterfaceStartError,
    VmnetPacketAvailableCallback, VmnetPacketIoBackend, VmnetPacketIoError, VmnetStatus,
};
use bangbang_runtime::network::GuestMacAddress;
use bangbang_session::credential::{CredentialPrefix, CredentialTarget};
use bangbang_session::vmnet_provider::{
    ProviderCleanup, ProviderStatus, RealizedVmnetParameters, VmnetPacketBatch,
};

use crate::owner::{
    OwnerBackend, OwnerCredentialError, OwnerCredentialOps, OwnerReadinessCallback,
    OwnerStartFailure,
};
use crate::policy::ResolvedVmnetPolicy;

#[derive(Debug)]
pub(super) struct SystemOwnerBackend {
    backend: StartedVmnetPacketIoBackend<SystemVmnetInterfaceBackend>,
    interface: StartedVmnetPacketIoInterface,
    packet_capacity: usize,
    read_maximum: u16,
}

impl OwnerBackend for SystemOwnerBackend {
    fn start(
        policy: &ResolvedVmnetPolicy,
    ) -> Result<(Self, RealizedVmnetParameters), OwnerStartFailure> {
        let requested = policy.requested();
        let config = match policy {
            ResolvedVmnetPolicy::Host { .. } => VmnetInterfaceConfig::host(),
            ResolvedVmnetPolicy::Shared { .. } => VmnetInterfaceConfig::shared(),
            ResolvedVmnetPolicy::Bridged { .. } => VmnetInterfaceConfig::bridged(
                policy
                    .bridge_name()
                    .ok_or_else(|| {
                        OwnerStartFailure::new(
                            ProviderStatus::PolicyDenied,
                            ProviderCleanup::Complete,
                        )
                    })?
                    .to_owned(),
            )
            .map_err(|_| {
                OwnerStartFailure::new(ProviderStatus::InvalidArgument, ProviderCleanup::Complete)
            })?,
        }
        .with_guest_mac(requested.mac().map(GuestMacAddress::from_bytes))
        .with_mtu(requested.mtu());
        let (backend, interface) =
            StartedVmnetPacketIoBackend::start(SystemVmnetInterfaceBackend::new(), &config)
                .map_err(map_start_error)?;
        let parameters = backend.parameters();
        let maximum_packet_bytes =
            u32::try_from(parameters.maximum_packet_size()).map_err(|_| {
                OwnerStartFailure::new(ProviderStatus::SetupIncomplete, ProviderCleanup::Uncertain)
            })?;
        let realized = RealizedVmnetParameters::new(
            parameters.realized_mac().octets(),
            parameters.effective_mtu(),
            maximum_packet_bytes,
        )
        .and_then(|value| value.with_backend_interface_id(parameters.interface_id()))
        .and_then(|value| {
            value.with_batch_limits(
                parameters.read_max_packets(),
                parameters.write_max_packets(),
            )
        })
        .and_then(|value| {
            value.with_direct_virtio_header(
                parameters.direct_virtio_header_available(),
                parameters.direct_virtio_header_enabled(),
            )
        })
        .map_err(|_| {
            OwnerStartFailure::new(ProviderStatus::SetupIncomplete, ProviderCleanup::Uncertain)
        })?;
        Ok((
            Self {
                backend,
                interface,
                packet_capacity: realized.packet_buffer_bytes(),
                read_maximum: realized.effective_read_max_packets(),
            },
            realized,
        ))
    }

    fn enable_readiness(&mut self, callback: OwnerReadinessCallback) -> Result<(), ProviderStatus> {
        let maximum = self.read_maximum;
        self.backend
            .enable_packet_available_callback(VmnetPacketAvailableCallback::new(move |estimate| {
                let estimate = estimate.unwrap_or(1).max(1).min(u64::from(maximum));
                callback(u16::try_from(estimate).unwrap_or(maximum));
            }))
            .map_err(|error| map_vmnet_error(&error))
    }

    fn read_packets(&mut self, maximum: u16) -> Result<VmnetPacketBatch, ProviderStatus> {
        let count = usize::from(maximum);
        let aggregate = self
            .packet_capacity
            .checked_mul(count)
            .ok_or(ProviderStatus::MemoryFailure)?;
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(aggregate)
            .map_err(|_| ProviderStatus::MemoryFailure)?;
        buffer.resize(aggregate, 0);
        let mut lengths = Vec::new();
        lengths
            .try_reserve_exact(count)
            .map_err(|_| ProviderStatus::MemoryFailure)?;
        lengths.resize(count, 0);
        let completed = self
            .backend
            .read_packet_batch(
                &mut self.interface,
                &mut buffer,
                self.packet_capacity,
                count,
                &mut lengths,
            )
            .map_err(|error| map_packet_error(&error))?;
        let mut packets = Vec::new();
        packets
            .try_reserve_exact(completed)
            .map_err(|_| ProviderStatus::MemoryFailure)?;
        for (index, length) in lengths.iter().copied().take(completed).enumerate() {
            let start = index
                .checked_mul(self.packet_capacity)
                .ok_or(ProviderStatus::BackendFailure)?;
            let end = start
                .checked_add(length)
                .ok_or(ProviderStatus::BackendFailure)?;
            packets.push(
                buffer
                    .get(start..end)
                    .ok_or(ProviderStatus::BackendFailure)?,
            );
        }
        VmnetPacketBatch::read(&packets).map_err(|_| ProviderStatus::BackendFailure)
    }

    fn write_packets(&mut self, packets: &VmnetPacketBatch) -> Result<u16, ProviderStatus> {
        let (buffer, ranges) = stage_write_batch(packets)?;
        let completed = self
            .backend
            .write_packet_batch(&mut self.interface, &buffer, &ranges)
            .map_err(|error| map_packet_error(&error))?;
        if completed > ranges.len() {
            return Err(ProviderStatus::BackendFailure);
        }
        u16::try_from(completed).map_err(|_| ProviderStatus::TooManyPackets)
    }

    fn stop(&mut self) -> ProviderCleanup {
        if self.backend.stop().is_ok() {
            ProviderCleanup::Complete
        } else {
            ProviderCleanup::Uncertain
        }
    }
}

fn stage_write_batch(
    packets: &VmnetPacketBatch,
) -> Result<(Vec<u8>, Vec<Range<usize>>), ProviderStatus> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(packets.aggregate_bytes())
        .map_err(|_| ProviderStatus::MemoryFailure)?;
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(packets.packet_count())
        .map_err(|_| ProviderStatus::MemoryFailure)?;
    for index in 0..packets.packet_count() {
        let packet = packets
            .packet(index)
            .ok_or(ProviderStatus::InvalidArgument)?;
        let start = buffer.len();
        buffer.extend_from_slice(packet);
        ranges.push(start..buffer.len());
    }
    Ok((buffer, ranges))
}

fn map_start_error(error: VmnetInterfaceStartError) -> OwnerStartFailure {
    match error {
        VmnetInterfaceStartError::Descriptor { .. } => {
            OwnerStartFailure::new(ProviderStatus::InvalidArgument, ProviderCleanup::Complete)
        }
        VmnetInterfaceStartError::Start {
            source,
            disposition,
        } => OwnerStartFailure::new(map_vmnet_error(&source), cleanup(disposition)),
        VmnetInterfaceStartError::Parameters { disposition, .. } => {
            OwnerStartFailure::new(ProviderStatus::SetupIncomplete, cleanup(disposition))
        }
    }
}

const fn cleanup(disposition: VmnetInterfaceStartDisposition) -> ProviderCleanup {
    match disposition {
        VmnetInterfaceStartDisposition::Retryable => ProviderCleanup::Complete,
        VmnetInterfaceStartDisposition::Terminal => ProviderCleanup::Uncertain,
    }
}

fn map_vmnet_error(error: &VmnetError) -> ProviderStatus {
    map_status(error.status())
}

fn map_packet_error(error: &VmnetPacketIoError) -> ProviderStatus {
    match error {
        VmnetPacketIoError::Vmnet { source } => map_vmnet_error(source),
        VmnetPacketIoError::InterfaceStopped
        | VmnetPacketIoError::UnexpectedPacketCount { .. }
        | VmnetPacketIoError::ReadPacketSizeExceedsBuffer { .. }
        | VmnetPacketIoError::InvalidBatch { .. } => ProviderStatus::BackendFailure,
    }
}

const fn map_status(status: VmnetStatus) -> ProviderStatus {
    match status {
        VmnetStatus::MemoryFailure => ProviderStatus::MemoryFailure,
        VmnetStatus::InvalidArgument => ProviderStatus::InvalidArgument,
        VmnetStatus::SetupIncomplete => ProviderStatus::SetupIncomplete,
        VmnetStatus::InvalidAccess => ProviderStatus::PolicyDenied,
        VmnetStatus::PacketTooBig => ProviderStatus::PacketTooBig,
        VmnetStatus::BufferExhausted => ProviderStatus::BufferExhausted,
        VmnetStatus::TooManyPackets => ProviderStatus::TooManyPackets,
        VmnetStatus::SharingServiceBusy => ProviderStatus::SharingServiceBusy,
        VmnetStatus::NotAuthorized => ProviderStatus::NotAuthorized,
        VmnetStatus::Success | VmnetStatus::Failure | VmnetStatus::Unknown(_) => {
            ProviderStatus::BackendFailure
        }
    }
}

#[derive(Debug)]
pub(super) struct SystemCredentialOps;

impl OwnerCredentialOps for SystemCredentialOps {
    fn transition(
        &mut self,
        target: CredentialTarget,
    ) -> Result<CredentialPrefix, OwnerCredentialError> {
        bangbang_session::macos::credential::transition_process(target)
            .map(|transition| transition.prefix())
            .map_err(|_| OwnerCredentialError)
    }

    fn attest(&mut self, target: CredentialTarget) -> Result<(), OwnerCredentialError> {
        bangbang_session::macos::credential::attest_current_process(target)
            .map(|_| ())
            .map_err(|_| OwnerCredentialError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_batch_staging_preserves_order_and_packet_boundaries() {
        let packets =
            VmnetPacketBatch::write(&[&[1, 2], &[3], &[4, 5, 6]]).expect("batch should validate");
        let (buffer, ranges) = stage_write_batch(&packets).expect("batch should stage");
        assert_eq!(buffer, [1, 2, 3, 4, 5, 6]);
        assert_eq!(ranges, [0..2, 2..3, 3..6]);
    }
}
