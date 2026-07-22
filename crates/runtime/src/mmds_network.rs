//! Authority-free MMDS packet detour and virtio-net packet I/O.

use std::collections::TryReserveError;
use std::fmt;
use std::net::Ipv4Addr;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use crate::memory::GuestMemory;
use crate::metrics::SharedMmdsMetrics;
use crate::mmds::MmdsStateHandle;
pub use crate::mmds_stack::{
    MmdsNetworkStackBuildError, MmdsNetworkStackCaptureDescriptor, MmdsNetworkStackCaptureError,
    MmdsNetworkStackError, MmdsNetworkStackHandle,
};
use crate::network::{
    GuestMacAddress, VIRTIO_NET_MAX_BUFFER_SIZE, VirtioNetworkBackendMetrics,
    VirtioNetworkRxPacket, VirtioNetworkRxPacketSource, VirtioNetworkRxPacketSourceError,
    VirtioNetworkTxFrame, VirtioNetworkTxPacketCommit, VirtioNetworkTxPacketDisposition,
    VirtioNetworkTxPacketSink, VirtioNetworkTxPacketSinkError, VirtioNetworkTxPacketStage,
};
use crate::network_packet::{VirtioNetworkPacketEnvelope, VirtioNetworkPacketPlan};
use crate::startup::{
    Arm64BootNetworkInterface, Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError,
    Arm64BootNetworkPacketIoProvider,
};

pub const DEFAULT_MMDS_VIRTIO_NETWORK_RX_BUFFER_LEN: usize = VIRTIO_NET_MAX_BUFFER_SIZE as usize;

#[derive(Debug)]
pub enum MmdsOnlyVirtioNetworkPacketIoBuildError {
    EmptyRxBuffer,
    RxBufferAllocation { len: usize, source: TryReserveError },
}

impl fmt::Display for MmdsOnlyVirtioNetworkPacketIoBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRxBuffer => {
                formatter.write_str("MMDS-only virtio-net RX buffer must not be empty")
            }
            Self::RxBufferAllocation { len, source } => write!(
                formatter,
                "failed to reserve MMDS-only virtio-net RX buffer of {len} bytes: {source}"
            ),
        }
    }
}

impl std::error::Error for MmdsOnlyVirtioNetworkPacketIoBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RxBufferAllocation { source, .. } => Some(source),
            Self::EmptyRxBuffer => None,
        }
    }
}

#[derive(Debug)]
pub enum MmdsOnlyVirtioNetworkPacketIoProviderBuildError {
    DuplicateInterfaceId { iface_id: String },
}

impl fmt::Display for MmdsOnlyVirtioNetworkPacketIoProviderBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateInterfaceId { iface_id } => {
                write!(
                    formatter,
                    "duplicate MMDS-only network interface id {iface_id}"
                )
            }
        }
    }
}

impl std::error::Error for MmdsOnlyVirtioNetworkPacketIoProviderBuildError {}

/// TX-side classifier and router for one shared interface-local MMDS session.
pub struct MmdsPacketDetour {
    stack: MmdsNetworkStackHandle,
}

impl fmt::Debug for MmdsPacketDetour {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsPacketDetour")
            .field("stack", &self.stack)
            .finish()
    }
}

impl MmdsPacketDetour {
    pub fn try_new(
        mmds_state: MmdsStateHandle,
        mmds_ipv4_address: Ipv4Addr,
        metrics: SharedMmdsMetrics,
    ) -> Result<Self, MmdsNetworkStackBuildError> {
        Ok(Self {
            stack: MmdsNetworkStackHandle::try_new(mmds_state, mmds_ipv4_address, metrics)?,
        })
    }

    #[doc(hidden)]
    pub fn try_new_for_test(
        mmds_state: MmdsStateHandle,
        mmds_ipv4_address: Ipv4Addr,
        metrics: SharedMmdsMetrics,
        initial_sequence_seed: u32,
    ) -> Result<Self, MmdsNetworkStackBuildError> {
        Ok(Self {
            stack: MmdsNetworkStackHandle::try_new_for_test(
                mmds_state,
                mmds_ipv4_address,
                metrics,
                initial_sequence_seed,
            )?,
        })
    }

    pub fn detour_packet(&mut self, packet: &[u8]) -> Result<bool, MmdsNetworkStackError> {
        self.stack.detour_frame(packet)
    }

    #[doc(hidden)]
    pub fn detour_packet_at(
        &mut self,
        packet: &[u8],
        now: u64,
    ) -> Result<bool, MmdsNetworkStackError> {
        self.stack.detour_frame_at(packet, now)
    }

    /// Classifies a packet without mutating session, MMDS, or metric state.
    pub fn would_detour_packet(&self, packet: &[u8]) -> bool {
        self.stack.is_mmds_frame(packet)
    }

    pub fn stack(&self) -> MmdsNetworkStackHandle {
        self.stack.clone()
    }
}

#[derive(Debug)]
pub struct MmdsOnlyVirtioNetworkPacketIoProvider {
    #[doc(hidden)]
    pub entries: Vec<MmdsOnlyVirtioNetworkPacketIoProviderEntry>,
}

impl MmdsOnlyVirtioNetworkPacketIoProvider {
    pub fn new(
        entries: Vec<MmdsOnlyVirtioNetworkPacketIoProviderEntry>,
    ) -> Result<Self, MmdsOnlyVirtioNetworkPacketIoProviderBuildError> {
        for (index, entry) in entries.iter().enumerate() {
            if entries
                .iter()
                .skip(index + 1)
                .any(|candidate| candidate.iface_id == entry.iface_id)
            {
                return Err(
                    MmdsOnlyVirtioNetworkPacketIoProviderBuildError::DuplicateInterfaceId {
                        iface_id: entry.iface_id.clone(),
                    },
                );
            }
        }
        Ok(Self { entries })
    }
}

impl Arm64BootNetworkPacketIoProvider for MmdsOnlyVirtioNetworkPacketIoProvider {
    fn has_packet_readiness(&self, interface: Arm64BootNetworkInterface<'_>) -> bool {
        self.entries
            .iter()
            .find(|entry| entry.iface_id == interface.iface_id())
            .is_some_and(|entry| entry.packet_io.has_mmds_readiness())
    }

    fn packet_retry_after(&self, interface: Arm64BootNetworkInterface<'_>) -> Option<Duration> {
        self.entries
            .iter()
            .find(|entry| entry.iface_id == interface.iface_id())
            .and_then(|entry| entry.packet_io.mmds_retry_after())
    }

    fn packet_io(
        &mut self,
        interface: Arm64BootNetworkInterface<'_>,
    ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
        let iface_id = interface.iface_id();
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.iface_id == iface_id)
        else {
            return Err(Arm64BootNetworkPacketIoError::new(format!(
                "missing MMDS-only packet I/O for interface {iface_id}"
            )));
        };
        let MmdsOnlyVirtioNetworkPacketIo { tx_sink, rx_source } = &mut entry.packet_io;
        Ok(Arm64BootNetworkPacketIo::new(tx_sink, rx_source))
    }
}

#[derive(Debug)]
pub struct MmdsOnlyVirtioNetworkPacketIoProviderEntry {
    #[doc(hidden)]
    pub iface_id: String,
    #[doc(hidden)]
    pub packet_io: MmdsOnlyVirtioNetworkPacketIo,
}

impl MmdsOnlyVirtioNetworkPacketIoProviderEntry {
    pub fn new(iface_id: impl Into<String>, packet_io: MmdsOnlyVirtioNetworkPacketIo) -> Self {
        Self {
            iface_id: iface_id.into(),
            packet_io,
        }
    }
}

#[derive(Debug)]
pub struct MmdsOnlyVirtioNetworkPacketIo {
    #[doc(hidden)]
    pub tx_sink: MmdsOnlyVirtioNetworkTxPacketSink,
    #[doc(hidden)]
    pub rx_source: MmdsOnlyVirtioNetworkRxPacketSource,
}

impl MmdsOnlyVirtioNetworkPacketIo {
    pub fn new(
        mmds_detour: MmdsPacketDetour,
    ) -> Result<Self, MmdsOnlyVirtioNetworkPacketIoBuildError> {
        Self::with_guest_mac(mmds_detour, None)
    }

    pub fn with_guest_mac(
        mmds_detour: MmdsPacketDetour,
        guest_mac: Option<GuestMacAddress>,
    ) -> Result<Self, MmdsOnlyVirtioNetworkPacketIoBuildError> {
        let stack = mmds_detour.stack();
        Ok(Self {
            tx_sink: MmdsOnlyVirtioNetworkTxPacketSink {
                mmds_detour,
                staged_frame: None,
                guest_mac,
                backend_metrics: VirtioNetworkBackendMetrics::default(),
            },
            rx_source: MmdsOnlyVirtioNetworkRxPacketSource::new(
                stack,
                DEFAULT_MMDS_VIRTIO_NETWORK_RX_BUFFER_LEN,
            )?,
        })
    }

    pub fn has_mmds_readiness(&self) -> bool {
        self.rx_source.has_mmds_readiness()
    }

    pub fn mmds_retry_after(&self) -> Option<Duration> {
        self.rx_source.mmds_retry_after()
    }

    #[doc(hidden)]
    pub fn mmds_retry_after_at(&self, now: Instant) -> Option<Duration> {
        self.rx_source.mmds_retry_after_at(now)
    }

    #[doc(hidden)]
    pub fn capture_mmds_ready_at(&self, now: Instant) -> bool {
        self.rx_source.stack.has_ready_frame_at_instant(now)
    }

    #[doc(hidden)]
    pub fn capture_cached_rx_len(&self) -> Option<usize> {
        self.rx_source.cached_len
    }

    #[doc(hidden)]
    pub fn capture_tx_transaction_active(&self) -> bool {
        self.tx_sink.staged_frame.is_some()
    }

    #[doc(hidden)]
    pub fn capture_mmds_stack(&self) -> MmdsNetworkStackHandle {
        self.rx_source.stack.clone()
    }
}

pub struct MmdsOnlyVirtioNetworkTxPacketSink {
    mmds_detour: MmdsPacketDetour,
    staged_frame: Option<StagedMmdsOnlyTxFrame>,
    guest_mac: Option<GuestMacAddress>,
    backend_metrics: VirtioNetworkBackendMetrics,
}

impl fmt::Debug for MmdsOnlyVirtioNetworkTxPacketSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsOnlyVirtioNetworkTxPacketSink")
            .field("mmds_detour", &"<configured>")
            .field("staged_frame", &self.staged_frame.is_some())
            .field("guest_mac", &self.guest_mac.map(|_| "<configured>"))
            .finish()
    }
}

struct StagedMmdsOnlyTxFrame {
    packet: VirtioNetworkPacketPlan,
    disposition: VirtioNetworkTxPacketDisposition,
}

impl fmt::Debug for StagedMmdsOnlyTxFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagedMmdsOnlyTxFrame")
            .field("packet", &"[REDACTED]")
            .field("disposition", &self.disposition)
            .finish()
    }
}

impl MmdsOnlyVirtioNetworkTxPacketSink {
    fn prepare_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<StagedMmdsOnlyTxFrame, VirtioNetworkTxPacketSinkError> {
        let plan = frame
            .prepare_packet(memory)
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        self.prepare_owned_packet_plan(plan)
    }

    fn classify_packet_plan(
        &self,
        plan: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let mut disposition = None;
        let visit = plan
            .visit_packets(VirtioNetworkPacketEnvelope::RawEthernet, |packet| {
                let current = if self.mmds_detour.would_detour_packet(packet) {
                    VirtioNetworkTxPacketDisposition::Detoured
                } else {
                    VirtioNetworkTxPacketDisposition::Forwarded
                };
                if disposition.is_some_and(|previous| previous != current) {
                    return ControlFlow::Break(VirtioNetworkTxPacketSinkError::new(
                        "MMDS classification changed within one normalized TX frame",
                    ));
                }
                disposition = Some(current);
                ControlFlow::Continue(())
            })
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        if let ControlFlow::Break(source) = visit {
            return Err(source);
        }
        disposition.ok_or_else(|| {
            VirtioNetworkTxPacketSinkError::new("MMDS-only normalization emitted no TX packet")
        })
    }

    fn prepare_owned_packet_plan(
        &mut self,
        packet: VirtioNetworkPacketPlan,
    ) -> Result<StagedMmdsOnlyTxFrame, VirtioNetworkTxPacketSinkError> {
        let disposition = self.classify_packet_plan(&packet)?;
        self.observe_source_mac(&packet);
        Ok(StagedMmdsOnlyTxFrame {
            packet,
            disposition,
        })
    }

    fn prepare_borrowed_packet_plan(
        &mut self,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<StagedMmdsOnlyTxFrame, VirtioNetworkTxPacketSinkError> {
        let disposition = self.classify_packet_plan(packet)?;
        let packet = packet
            .try_clone_owned()
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        self.observe_source_mac(&packet);
        Ok(StagedMmdsOnlyTxFrame {
            packet,
            disposition,
        })
    }

    fn observe_source_mac(&mut self, packet: &VirtioNetworkPacketPlan) {
        if let (Some(expected), Some(observed)) = (self.guest_mac, packet.source_mac())
            && expected.octets() != observed
        {
            self.backend_metrics.record_spoofed_mac();
        }
    }

    fn commit_frame(
        &mut self,
        staged: StagedMmdsOnlyTxFrame,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let expected_detour = staged.disposition == VirtioNetworkTxPacketDisposition::Detoured;
        let visit = staged
            .packet
            .visit_packets(
                VirtioNetworkPacketEnvelope::RawEthernet,
                |packet| match self
                    .mmds_detour
                    .detour_packet(packet)
                    .map_err(tx_mmds_detour_error)
                {
                    Ok(detoured) if detoured == expected_detour => ControlFlow::Continue(()),
                    Ok(_) => ControlFlow::Break(VirtioNetworkTxPacketSinkError::new(
                        "MMDS side-effect classification changed at commit",
                    )),
                    Err(source) => ControlFlow::Break(source),
                },
            )
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        if let ControlFlow::Break(source) = visit {
            return Err(source);
        }
        Ok(staged.disposition)
    }
}

impl VirtioNetworkTxPacketSink for MmdsOnlyVirtioNetworkTxPacketSink {
    fn transmit_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let staged = self.prepare_frame(memory, frame)?;
        self.commit_frame(staged)
    }

    fn transmit_prepared_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let staged = self.prepare_borrowed_packet_plan(packet)?;
        self.commit_frame(staged)
    }

    fn supports_staged_batch(&self) -> bool {
        true
    }

    fn stage_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        if self.staged_frame.is_some() {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "MMDS-only TX staging already owns an uncommitted frame",
            ));
        }
        let staged = self.prepare_frame(memory, frame)?;
        self.staged_frame = Some(staged);
        Ok(VirtioNetworkTxPacketStage::Staged {
            flush_before_commit: true,
        })
    }

    fn stage_prepared_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        if self.staged_frame.is_some() {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "MMDS-only TX staging already owns an uncommitted frame",
            ));
        }
        let staged = self.prepare_borrowed_packet_plan(packet)?;
        self.staged_frame = Some(staged);
        Ok(VirtioNetworkTxPacketStage::Staged {
            flush_before_commit: true,
        })
    }

    fn commit_staged_frame(&mut self) -> VirtioNetworkTxPacketCommit {
        let result = self
            .staged_frame
            .take()
            .ok_or_else(|| {
                VirtioNetworkTxPacketSinkError::new("MMDS-only TX commit has no staged frame")
            })
            .and_then(|staged| self.commit_frame(staged));
        VirtioNetworkTxPacketCommit::Immediate(result)
    }

    fn discard_staged_frame(&mut self) {
        self.staged_frame = None;
    }

    fn take_backend_metrics(&mut self) -> VirtioNetworkBackendMetrics {
        std::mem::take(&mut self.backend_metrics)
    }
}

pub struct MmdsOnlyVirtioNetworkRxPacketSource {
    stack: MmdsNetworkStackHandle,
    read_buffer: Vec<u8>,
    cached_len: Option<usize>,
}

impl fmt::Debug for MmdsOnlyVirtioNetworkRxPacketSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsOnlyVirtioNetworkRxPacketSource")
            .field("stack", &self.stack)
            .field("read_buffer", &"[REDACTED]")
            .field("read_buffer_len", &self.read_buffer.len())
            .field("cached_len", &self.cached_len)
            .finish()
    }
}

impl MmdsOnlyVirtioNetworkRxPacketSource {
    fn new(
        stack: MmdsNetworkStackHandle,
        rx_buffer_len: usize,
    ) -> Result<Self, MmdsOnlyVirtioNetworkPacketIoBuildError> {
        if rx_buffer_len == 0 {
            return Err(MmdsOnlyVirtioNetworkPacketIoBuildError::EmptyRxBuffer);
        }
        let mut read_buffer = Vec::new();
        read_buffer
            .try_reserve_exact(rx_buffer_len)
            .map_err(
                |source| MmdsOnlyVirtioNetworkPacketIoBuildError::RxBufferAllocation {
                    len: rx_buffer_len,
                    source,
                },
            )?;
        read_buffer.resize(rx_buffer_len, 0);
        Ok(Self {
            stack,
            read_buffer,
            cached_len: None,
        })
    }

    fn cached_packet(&self) -> Option<VirtioNetworkRxPacket<'_>> {
        let len = self.cached_len?;
        self.read_buffer.get(..len).map(VirtioNetworkRxPacket::new)
    }

    /// Returns persistent readiness for immediate or retained MMDS output.
    pub fn has_mmds_readiness(&self) -> bool {
        self.cached_len.is_some() || self.stack.has_ready_frame()
    }

    /// Returns the next protocol retry delay when no output is immediately ready.
    pub fn mmds_retry_after(&self) -> Option<Duration> {
        if self.cached_len.is_some() {
            None
        } else {
            self.stack.retry_after()
        }
    }

    fn mmds_retry_after_at(&self, now: Instant) -> Option<Duration> {
        if self.cached_len.is_some() {
            None
        } else {
            self.stack.retry_after_at_instant(now)
        }
    }
}

impl VirtioNetworkRxPacketSource for MmdsOnlyVirtioNetworkRxPacketSource {
    fn retry_after_tx_hint(&self) -> bool {
        self.has_mmds_readiness()
    }

    fn peek_packet(
        &mut self,
    ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
        if self.cached_len.is_some() {
            return Ok(self.cached_packet());
        }
        self.cached_len = self
            .stack
            .copy_next_frame_into(&mut self.read_buffer)
            .map_err(rx_mmds_stack_error)?;
        Ok(self.cached_packet())
    }

    fn consume_packet(&mut self) {
        if let Some(len) = self.cached_len.take() {
            let _ = self.stack.consume_frame(len);
        }
    }
}

fn tx_mmds_detour_error(source: MmdsNetworkStackError) -> VirtioNetworkTxPacketSinkError {
    VirtioNetworkTxPacketSinkError::new(format!("MMDS packet detour failed: {source}"))
}

fn rx_mmds_stack_error(source: MmdsNetworkStackError) -> VirtioNetworkRxPacketSourceError {
    VirtioNetworkRxPacketSourceError::new(format!("MMDS network stack output failed: {source}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mmds::DEFAULT_MMDS_IPV4_ADDRESS;
    use crate::network::VirtioNetworkTxHeader;

    fn test_detour() -> MmdsPacketDetour {
        MmdsPacketDetour::try_new_for_test(
            MmdsStateHandle::default(),
            DEFAULT_MMDS_IPV4_ADDRESS,
            SharedMmdsMetrics::default(),
            7,
        )
        .expect("test MMDS stack should build")
    }

    #[test]
    fn staged_mmds_tx_frame_debug_redacts_packet_bytes() {
        let token_value = "private-staged-token-value-that-must-not-appear";
        let mut packet = vec![0; 14];
        packet.extend_from_slice(token_value.as_bytes());
        let staged = StagedMmdsOnlyTxFrame {
            packet: VirtioNetworkPacketPlan::prepare(VirtioNetworkTxHeader::new(), 0, packet)
                .expect("test Ethernet packet should validate"),
            disposition: VirtioNetworkTxPacketDisposition::Forwarded,
        };
        let debug_output = format!("{staged:?}");
        assert!(!debug_output.contains(token_value));
        assert!(debug_output.contains("[REDACTED]"));
    }

    #[test]
    fn mmds_only_tx_observes_configured_source_mac_without_filtering() {
        let expected = GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 1]);
        let mut packet_io =
            MmdsOnlyVirtioNetworkPacketIo::with_guest_mac(test_detour(), Some(expected))
                .expect("MMDS-only packet I/O should build");
        let mut packet = vec![0; 14];
        packet[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 2]);
        let plan = VirtioNetworkPacketPlan::prepare(VirtioNetworkTxHeader::new(), 0, packet)
            .expect("plain Ethernet packet should validate");
        let staged = packet_io
            .tx_sink
            .prepare_borrowed_packet_plan(&plan)
            .expect("spoof observation must not filter the packet");
        assert_eq!(
            staged.disposition,
            VirtioNetworkTxPacketDisposition::Forwarded
        );
        assert_eq!(
            packet_io
                .tx_sink
                .take_backend_metrics()
                .tx_spoofed_mac_count(),
            1
        );
    }

    #[test]
    fn provider_rejects_duplicate_interface_ids() {
        let first = MmdsOnlyVirtioNetworkPacketIoProviderEntry::new(
            "eth0",
            MmdsOnlyVirtioNetworkPacketIo::new(test_detour()).expect("packet I/O should build"),
        );
        let second = MmdsOnlyVirtioNetworkPacketIoProviderEntry::new(
            "eth0",
            MmdsOnlyVirtioNetworkPacketIo::new(test_detour()).expect("packet I/O should build"),
        );
        assert!(matches!(
            MmdsOnlyVirtioNetworkPacketIoProvider::new(vec![first, second]),
            Err(MmdsOnlyVirtioNetworkPacketIoProviderBuildError::DuplicateInterfaceId {
                iface_id
            }) if iface_id == "eth0"
        ));
    }
}
