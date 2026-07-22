// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static GUEST_BOOT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BOOT_MARKER: &[u8] = b"BANGBANG_BOOT_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_PCI_RNG_BOUND_MARKER: &[u8] = b"BANGBANG_VIRTIO_PCI_RNG_BOUND";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_PCI_RNG_IO_MARKER: &[u8] = b"BANGBANG_VIRTIO_PCI_RNG_IO_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_PCI_RNG_SUCCESS_MARKER: &[u8] = b"BANGBANG_VIRTIO_PCI_RNG_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_PCI_RNG_FAILURE_MARKER: &[u8] = b"BANGBANG_VIRTIO_PCI_RNG_FAIL";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_PCI_RNG_IRQ_BEFORE_BEGIN: &[u8] = b"BANGBANG_VIRTIO_PCI_RNG_IRQ_BEFORE_BEGIN";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_PCI_RNG_IRQ_BEFORE_END: &[u8] = b"BANGBANG_VIRTIO_PCI_RNG_IRQ_BEFORE_END";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_PCI_RNG_IRQ_AFTER_BEGIN: &[u8] = b"BANGBANG_VIRTIO_PCI_RNG_IRQ_AFTER_BEGIN";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_PCI_RNG_IRQ_AFTER_END: &[u8] = b"BANGBANG_VIRTIO_PCI_RNG_IRQ_AFTER_END";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BLOCK_READ_MARKER: &[u8] = b"BANGBANG_BLOCK_READ_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BLOCK_WRITE_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITE_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ROOTFS_READ_MARKER: &[u8] = b"BANGBANG_ROOTFS_READ_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SECONDARY_CPU_MARKER: &[u8] = b"BANGBANG_SECONDARY_CPU_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SMP_HOTPLUG_READY_MARKER: &[u8] = b"BBHOTREADY";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SMP_HOTPLUG_OFF_MARKER: &[u8] = b"BBHOTOFF";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SMP_HOTPLUG_DONE_MARKER: &[u8] = b"BBHOTDONE";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const DIRECT_ROOTFS_BOOT_MARKER: &[u8] = b"BANGBANG_DIRECT_ROOTFS_BOOT_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PVTIME_DISCOVERY_MARKER: &[u8] = b"BANGBANG_PVTIME_DISCOVERY_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PVTIME_CONTENTION_MARKER: &[u8] = b"BANGBANG_PVTIME_CONTENTION_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PVTIME_IDLE_MARKER: &[u8] = b"BANGBANG_PVTIME_IDLE_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PVTIME_FAILURE_MARKER: &[u8] = b"BANGBANG_PVTIME_FAIL_";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VMGENID_GUEST_CHECK_MARKER: &[u8] = b"BANGBANG_VMGENID_GUEST_CHECK_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PMEM_READ_FLUSH_MARKER: &[u8] = b"BANGBANG_PMEM_READ_FLUSH_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PMEM_ROOT_RO_MARKER: &[u8] = b"BANGBANG_PMEM_ROOT_RO_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PMEM_ROOT_RW_MARKER: &[u8] = b"BANGBANG_PMEM_ROOT_RW_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BLOCK_WRITEBACK_FLUSH_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITEBACK_FLUSH_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PCI_BLOCK_IDENTITIES_MARKER: &[u8] = b"BANGBANG_PCI_BLOCK_IDENTITIES_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PCI_PMEM_IDENTITIES_MARKER: &[u8] = b"BANGBANG_PCI_PMEM_IDENTITIES_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MMDS_FETCH_MARKER: &[u8] = b"BANGBANG_MMDS_FETCH_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MMDS_V2_RENEW_MARKER: &[u8] = b"BANGBANG_MMDS_V2_RENEW_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PCI_NETWORK_IDENTITIES_MARKER: &[u8] = b"BANGBANG_PCI_NETWORK_IDENTITIES_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_NETWORK_LARGE_TX_MARKER: &[u8] = b"BANGBANG_VIRTIO_NET_LARGE_TX_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_NETWORK_LARGE_RX_MARKER: &[u8] = b"BANGBANG_VIRTIO_NET_LARGE_RX_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_NETWORK_SEMANTICS_MARKER: &[u8] = b"BANGBANG_VIRTIO_NET_SEMANTICS_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PMEM_HOST_MARKER: &[u8] = b"BANGBANG_PMEM_HOST_MARKER";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PMEM_GUEST_FLUSH_MARKER: &[u8] = b"BANGBANG_PMEM_GUEST_FLUSH_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CACHE_FDT_GUEST_CHECK_MARKER: &[u8] = b"BANGBANG_CACHE_FDT_GUEST_CHECK_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CACHE_FDT_GUEST_FAILURE_MARKER: &[u8] = b"BANGBANG_CACHE_FDT_GUEST_CHECK_FAIL";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CPU_TEMPLATE_GUEST_CHECK_MARKER: &[u8] = b"BANGBANG_CPU_TEMPLATE_GUEST_CHECK_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CPU_TEMPLATE_GUEST_FAILURE_MARKER: &[u8] = b"BANGBANG_CPU_TEMPLATE_GUEST_CHECK_FAIL";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CACHE_REPORT_HEADER: &str = "BANGBANG_CACHE_REPORT_V1";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CPU_TEMPLATE_REPORT_HEADER: &str = "BANGBANG_ARM64_ID_SET_V1";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CACHE_REPORT_BACKING_SIZE: u64 = 64 * 1024;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PMEM_GUEST_FLUSH_OFFSET: u64 = 4096;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ROOTFS_OS_RELEASE_ID: &[u8] = b"ID=ubuntu";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ROOTFS_OS_RELEASE_CODENAME: &[u8] = b"VERSION_CODENAME=noble";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CMDLINE_BEGIN_MARKER: &[u8] = b"BANGBANG_CMDLINE_BEGIN";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CMDLINE_END_MARKER: &[u8] = b"BANGBANG_CMDLINE_END";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const INITRD_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/init";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VIRTIO_PCI_RNG_INITRD_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/pci-rng-init";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SMP_INITRD_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/smp-init";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SMP_HOTPLUG_INITRD_BOOT_ARGS: &str =
    "console=ttyS0 reboot=k panic=1 rdinit=/smp-hotplug-init";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const DIRECT_ROOTFS_BOOT_ARGS: &str =
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GUEST_BOOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SERIAL_MMIO_BASE: u64 = 0x4000_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const RTC_MMIO_BASE: u64 = 0x4000_1000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BLOCK_MMIO_BASE: u64 = 0x5000_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PMEM_MMIO_BASE: u64 = 0x5800_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const NETWORK_MMIO_BASE: u64 = 0x6000_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VSOCK_MMIO_BASE: u64 = 0x7000_0000;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Default)]
struct SignedGuestNetworkSemanticEvidence {
    maximum_emitted_packets: std::sync::atomic::AtomicUsize,
    mmds_payload_frames: std::sync::atomic::AtomicUsize,
    drop_ack_armed: std::sync::atomic::AtomicBool,
    dropped_ack: std::sync::atomic::AtomicBool,
    retransmission_observed: std::sync::atomic::AtomicBool,
    large_response_end_sequence: std::sync::Mutex<Option<u32>>,
    large_response_frames: std::sync::Mutex<Vec<SignedGuestMmdsFrameFingerprint>>,
    retransmission_candidate: std::sync::Mutex<Option<SignedGuestMmdsFrameFingerprint>>,
    errors: std::sync::Mutex<Vec<String>>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone, PartialEq, Eq)]
struct SignedGuestMmdsFrameFingerprint {
    sequence_number: u32,
    first_sequence_after: u32,
    payload: Vec<u8>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl std::fmt::Debug for SignedGuestMmdsFrameFingerprint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignedGuestMmdsFrameFingerprint")
            .field("sequence_number", &self.sequence_number)
            .field("first_sequence_after", &self.first_sequence_after)
            .field("payload", &"[REDACTED]")
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl std::fmt::Debug for SignedGuestNetworkSemanticEvidence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignedGuestNetworkSemanticEvidence")
            .field("maximum_emitted_packets", &self.maximum_emitted_packets())
            .field("mmds_payload_frames", &self.mmds_payload_frames())
            .field("drop_ack_armed", &"<state>")
            .field("dropped_ack", &self.dropped_ack())
            .field("retransmission_observed", &self.retransmission_observed())
            .field("large_response_end_sequence", &"<state>")
            .field("large_response_frames", &"<redacted>")
            .field("retransmission_candidate", &"<redacted>")
            .field("errors", &"<redacted>")
            .finish()
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl SignedGuestNetworkSemanticEvidence {
    fn observe(
        &self,
        packet: &bangbang_runtime::network_packet::VirtioNetworkPacketPlan,
    ) -> Result<(), bangbang_runtime::network::VirtioNetworkTxPacketSinkError> {
        let packet_count = packet.emitted_packet_count().map_err(|source| {
            bangbang_runtime::network::VirtioNetworkTxPacketSinkError::new(source.to_string())
        })?;
        self.maximum_emitted_packets
            .fetch_max(packet_count, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn maximum_emitted_packets(&self) -> usize {
        self.maximum_emitted_packets
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn observe_mmds_rx_frame(&self, frame: &[u8]) {
        let Some(fingerprint) = signed_guest_mmds_payload_fingerprint(frame) else {
            return;
        };
        self.mmds_payload_frames
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let large_response_end_sequence = {
            let mut end_sequence = self
                .large_response_end_sequence
                .lock()
                .expect("signed MMDS response evidence lock should not be poisoned");
            if end_sequence.is_none() {
                *end_sequence = signed_guest_large_mmds_response_end_sequence(&fingerprint);
            }
            *end_sequence
        };
        let is_final_response_frame =
            large_response_end_sequence == Some(fingerprint.first_sequence_after);
        let mut frames = self
            .large_response_frames
            .lock()
            .expect("signed MMDS response-frame evidence lock should not be poisoned");
        if self.dropped_ack.load(std::sync::atomic::Ordering::Acquire)
            && signed_guest_mmds_payload_repeats(&frames, &fingerprint)
        {
            self.retransmission_observed
                .store(true, std::sync::atomic::Ordering::Release);
            self.drop_ack_armed
                .store(false, std::sync::atomic::Ordering::Release);
        } else if frames.len() < 256 && !frames.iter().any(|candidate| candidate == &fingerprint) {
            frames.push(fingerprint.clone());
        }
        drop(frames);
        if !is_final_response_frame {
            return;
        }

        let mut candidate = self
            .retransmission_candidate
            .lock()
            .expect("signed MMDS retransmission evidence lock should not be poisoned");
        match candidate.as_ref() {
            None => {
                *candidate = Some(fingerprint);
                self.drop_ack_armed
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            Some(candidate) if candidate == &fingerprint => {
                if self.dropped_ack.load(std::sync::atomic::Ordering::Acquire) {
                    self.retransmission_observed
                        .store(true, std::sync::atomic::Ordering::Release);
                }
            }
            Some(_) => {}
        }
    }

    fn should_drop_guest_ack(
        &self,
        packet: &bangbang_runtime::network_packet::VirtioNetworkPacketPlan,
    ) -> Result<bool, bangbang_runtime::network::VirtioNetworkTxPacketSinkError> {
        if !self
            .drop_ack_armed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(false);
        }
        let candidate_end = self
            .retransmission_candidate
            .lock()
            .expect("signed MMDS retransmission evidence lock should not be poisoned")
            .as_ref()
            .map(|candidate| candidate.first_sequence_after);
        let Some(candidate_end) = candidate_end else {
            return Ok(false);
        };
        let mut packet_count = 0_usize;
        let mut matching_ack = false;
        let _ = packet
            .visit_packets(
                bangbang_runtime::network_packet::VirtioNetworkPacketEnvelope::RawEthernet,
                |frame| {
                    packet_count = packet_count.saturating_add(1);
                    matching_ack =
                        signed_guest_pure_mmds_ack(frame).is_some_and(|acknowledgement| {
                            acknowledgement.wrapping_sub(candidate_end) < (1_u32 << 31)
                        });
                    std::ops::ControlFlow::<()>::Continue(())
                },
            )
            .map_err(|source| {
                bangbang_runtime::network::VirtioNetworkTxPacketSinkError::new(source.to_string())
            })?;
        let should_drop = packet_count == 1
            && matching_ack
            && !self
                .retransmission_observed
                .load(std::sync::atomic::Ordering::Acquire);
        if should_drop {
            self.dropped_ack
                .store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(should_drop)
    }

    fn mmds_payload_frames(&self) -> usize {
        self.mmds_payload_frames
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn dropped_ack(&self) -> bool {
        self.dropped_ack.load(std::sync::atomic::Ordering::Acquire)
    }

    fn retransmission_observed(&self) -> bool {
        self.retransmission_observed
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn record_error(&self, source: &bangbang_runtime::network::VirtioNetworkTxPacketSinkError) {
        let mut errors = self
            .errors
            .lock()
            .expect("signed network evidence lock should not be poisoned");
        if errors.len() < 8 {
            errors.push(source.to_string());
        }
    }

    fn errors(&self) -> Vec<String> {
        self.errors
            .lock()
            .expect("signed network evidence lock should not be poisoned")
            .clone()
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn signed_guest_mmds_tcp(frame: &[u8]) -> Option<&[u8]> {
    if u16::from_be_bytes(frame.get(12..14)?.try_into().ok()?) != 0x0800 {
        return None;
    }
    let ipv4 = frame.get(14..)?;
    let ipv4_header_len = usize::from(*ipv4.first()? & 0x0f).checked_mul(4)?;
    if ipv4_header_len < 20 || *ipv4.get(9)? != 6 {
        return None;
    }
    let tcp = ipv4.get(ipv4_header_len..)?;
    (u16::from_be_bytes(tcp.get(0..2)?.try_into().ok()?) == 80
        || u16::from_be_bytes(tcp.get(2..4)?.try_into().ok()?) == 80)
        .then_some(tcp)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn signed_guest_mmds_payload_fingerprint(frame: &[u8]) -> Option<SignedGuestMmdsFrameFingerprint> {
    let tcp = signed_guest_mmds_tcp(frame)?;
    if u16::from_be_bytes(tcp.get(0..2)?.try_into().ok()?) != 80 {
        return None;
    }
    let tcp_header_len = usize::from(*tcp.get(12)? >> 4).checked_mul(4)?;
    let payload = tcp.get(tcp_header_len..)?;
    if payload.is_empty() {
        return None;
    }
    let sequence_number = u32::from_be_bytes(tcp.get(4..8)?.try_into().ok()?);
    Some(SignedGuestMmdsFrameFingerprint {
        sequence_number,
        first_sequence_after: sequence_number.wrapping_add(u32::try_from(payload.len()).ok()?),
        payload: payload.to_vec(),
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn signed_guest_large_mmds_response_end_sequence(
    fingerprint: &SignedGuestMmdsFrameFingerprint,
) -> Option<u32> {
    const LARGE_RESPONSE_CONTENT_LEN: usize = 48 * 1024;

    let header_end = fingerprint
        .payload
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    let content_length = fingerprint
        .payload
        .get(..header_end)?
        .split(|byte| *byte == b'\n')
        .find_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let separator = line.iter().position(|byte| *byte == b':')?;
            let name = line.get(..separator)?;
            let value = line.get(separator.checked_add(1)?..)?;
            name.eq_ignore_ascii_case(b"content-length")
                .then(|| {
                    std::str::from_utf8(value)
                        .ok()?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
                .flatten()
        })?;
    if content_length != LARGE_RESPONSE_CONTENT_LEN {
        return None;
    }
    let response_len = header_end.checked_add(4)?.checked_add(content_length)?;
    Some(
        fingerprint
            .sequence_number
            .wrapping_add(u32::try_from(response_len).ok()?),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn signed_guest_mmds_payload_repeats(
    originals: &[SignedGuestMmdsFrameFingerprint],
    candidate: &SignedGuestMmdsFrameFingerprint,
) -> bool {
    originals.iter().any(|original| {
        let offset = candidate
            .sequence_number
            .wrapping_sub(original.sequence_number);
        let Ok(offset) = usize::try_from(offset) else {
            return false;
        };
        let Some(expected) = original.payload.get(offset..) else {
            return false;
        };
        let overlap_len = expected.len().min(candidate.payload.len());
        overlap_len > 0 && expected.get(..overlap_len) == candidate.payload.get(..overlap_len)
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn signed_guest_pure_mmds_ack(frame: &[u8]) -> Option<u32> {
    let tcp = signed_guest_mmds_tcp(frame)?;
    if u16::from_be_bytes(tcp.get(2..4)?.try_into().ok()?) != 80 {
        return None;
    }
    let tcp_header_len = usize::from(*tcp.get(12)? >> 4).checked_mul(4)?;
    let flags = *tcp.get(13)?;
    if tcp.get(tcp_header_len..)?.is_empty()
        && flags & 0x10 != 0
        && flags & (0x02 | 0x01 | 0x04 | 0x08) == 0
    {
        Some(u32::from_be_bytes(tcp.get(8..12)?.try_into().ok()?))
    } else {
        None
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug)]
struct SignedGuestMmdsTxPacketSink {
    inner: bangbang_runtime::mmds_network::MmdsOnlyVirtioNetworkTxPacketSink,
    evidence: std::sync::Arc<SignedGuestNetworkSemanticEvidence>,
    drop_staged_ack: bool,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl bangbang_runtime::network::VirtioNetworkTxPacketSink for SignedGuestMmdsTxPacketSink {
    fn transmit_frame(
        &mut self,
        memory: &bangbang_runtime::memory::GuestMemory,
        frame: &bangbang_runtime::network::VirtioNetworkTxFrame,
    ) -> Result<
        bangbang_runtime::network::VirtioNetworkTxPacketDisposition,
        bangbang_runtime::network::VirtioNetworkTxPacketSinkError,
    > {
        let packet = frame.prepare_packet(memory).map_err(|source| {
            bangbang_runtime::network::VirtioNetworkTxPacketSinkError::new(source.to_string())
        })?;
        self.evidence.observe(&packet)?;
        if self.evidence.should_drop_guest_ack(&packet)? {
            return Ok(bangbang_runtime::network::VirtioNetworkTxPacketDisposition::Detoured);
        }
        let result = self.inner.transmit_prepared_frame(memory, frame, &packet);
        if let Err(source) = &result {
            self.evidence.record_error(source);
        }
        result
    }

    fn transmit_prepared_frame(
        &mut self,
        memory: &bangbang_runtime::memory::GuestMemory,
        frame: &bangbang_runtime::network::VirtioNetworkTxFrame,
        packet: &bangbang_runtime::network_packet::VirtioNetworkPacketPlan,
    ) -> Result<
        bangbang_runtime::network::VirtioNetworkTxPacketDisposition,
        bangbang_runtime::network::VirtioNetworkTxPacketSinkError,
    > {
        self.evidence.observe(packet)?;
        if self.evidence.should_drop_guest_ack(packet)? {
            return Ok(bangbang_runtime::network::VirtioNetworkTxPacketDisposition::Detoured);
        }
        let result = self.inner.transmit_prepared_frame(memory, frame, packet);
        if let Err(source) = &result {
            self.evidence.record_error(source);
        }
        result
    }

    fn supports_staged_batch(&self) -> bool {
        true
    }

    fn stage_frame(
        &mut self,
        memory: &bangbang_runtime::memory::GuestMemory,
        frame: &bangbang_runtime::network::VirtioNetworkTxFrame,
    ) -> Result<
        bangbang_runtime::network::VirtioNetworkTxPacketStage,
        bangbang_runtime::network::VirtioNetworkTxPacketSinkError,
    > {
        let packet = frame.prepare_packet(memory).map_err(|source| {
            bangbang_runtime::network::VirtioNetworkTxPacketSinkError::new(source.to_string())
        })?;
        self.evidence.observe(&packet)?;
        if self.evidence.should_drop_guest_ack(&packet)? {
            self.drop_staged_ack = true;
            return Ok(
                bangbang_runtime::network::VirtioNetworkTxPacketStage::Staged {
                    flush_before_commit: false,
                },
            );
        }
        let result = self.inner.stage_prepared_frame(memory, frame, &packet);
        if let Err(source) = &result {
            self.evidence.record_error(source);
        }
        result
    }

    fn stage_prepared_frame(
        &mut self,
        memory: &bangbang_runtime::memory::GuestMemory,
        frame: &bangbang_runtime::network::VirtioNetworkTxFrame,
        packet: &bangbang_runtime::network_packet::VirtioNetworkPacketPlan,
    ) -> Result<
        bangbang_runtime::network::VirtioNetworkTxPacketStage,
        bangbang_runtime::network::VirtioNetworkTxPacketSinkError,
    > {
        self.evidence.observe(packet)?;
        if self.evidence.should_drop_guest_ack(packet)? {
            self.drop_staged_ack = true;
            return Ok(
                bangbang_runtime::network::VirtioNetworkTxPacketStage::Staged {
                    flush_before_commit: false,
                },
            );
        }
        let result = self.inner.stage_prepared_frame(memory, frame, packet);
        if let Err(source) = &result {
            self.evidence.record_error(source);
        }
        result
    }

    fn commit_staged_frame(&mut self) -> bangbang_runtime::network::VirtioNetworkTxPacketCommit {
        if std::mem::take(&mut self.drop_staged_ack) {
            return bangbang_runtime::network::VirtioNetworkTxPacketCommit::Immediate(Ok(
                bangbang_runtime::network::VirtioNetworkTxPacketDisposition::Detoured,
            ));
        }
        let result = self.inner.commit_staged_frame();
        if let bangbang_runtime::network::VirtioNetworkTxPacketCommit::Immediate(Err(source)) =
            &result
        {
            self.evidence.record_error(source);
        }
        result
    }

    fn discard_staged_frame(&mut self) {
        self.drop_staged_ack = false;
        self.inner.discard_staged_frame();
    }

    fn flush_staged_frames(
        &mut self,
        results: &mut Vec<
            Result<
                bangbang_runtime::network::VirtioNetworkTxPacketDisposition,
                bangbang_runtime::network::VirtioNetworkTxPacketSinkError,
            >,
        >,
    ) {
        self.inner.flush_staged_frames(results);
    }

    fn take_backend_metrics(&mut self) -> bangbang_runtime::network::VirtioNetworkBackendMetrics {
        self.inner.take_backend_metrics()
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug)]
struct SignedGuestMmdsRxPacketSource {
    inner: bangbang_runtime::mmds_network::MmdsOnlyVirtioNetworkRxPacketSource,
    evidence: std::sync::Arc<SignedGuestNetworkSemanticEvidence>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl bangbang_runtime::network::VirtioNetworkRxPacketSource for SignedGuestMmdsRxPacketSource {
    fn begin_rx_dispatch(&mut self) {
        self.inner.begin_rx_dispatch();
    }

    fn host_readiness_hint(&self) -> bool {
        self.inner.host_readiness_hint()
    }

    fn retry_after_tx_hint(&self) -> bool {
        self.inner.retry_after_tx_hint()
    }

    fn peek_packet(
        &mut self,
    ) -> Result<
        Option<bangbang_runtime::network::VirtioNetworkRxPacket<'_>>,
        bangbang_runtime::network::VirtioNetworkRxPacketSourceError,
    > {
        self.inner.peek_packet()
    }

    fn consume_packet(&mut self) {
        let frame = self
            .inner
            .peek_packet()
            .ok()
            .flatten()
            .map(|packet| packet.bytes().to_vec());
        if let Some(frame) = frame {
            self.evidence.observe_mmds_rx_frame(&frame);
        }
        self.inner.consume_packet();
    }

    fn take_backend_metrics(&mut self) -> bangbang_runtime::network::VirtioNetworkBackendMetrics {
        self.inner.take_backend_metrics()
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug)]
struct SignedGuestNetworkPacketIoProvider {
    tx_sink: SignedGuestMmdsTxPacketSink,
    rx_source: SignedGuestMmdsRxPacketSource,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl SignedGuestNetworkPacketIoProvider {
    fn new(
        packet_io: bangbang_runtime::mmds_network::MmdsOnlyVirtioNetworkPacketIo,
        evidence: std::sync::Arc<SignedGuestNetworkSemanticEvidence>,
    ) -> Self {
        Self {
            tx_sink: SignedGuestMmdsTxPacketSink {
                inner: packet_io.tx_sink,
                evidence: std::sync::Arc::clone(&evidence),
                drop_staged_ack: false,
            },
            rx_source: SignedGuestMmdsRxPacketSource {
                inner: packet_io.rx_source,
                evidence,
            },
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl bangbang_runtime::startup::Arm64BootNetworkPacketIoProvider
    for SignedGuestNetworkPacketIoProvider
{
    fn has_packet_readiness(
        &self,
        interface: bangbang_runtime::startup::Arm64BootNetworkInterface<'_>,
    ) -> bool {
        interface.iface_id() == "eth0" && self.rx_source.inner.has_mmds_readiness()
    }

    fn packet_retry_after(
        &self,
        interface: bangbang_runtime::startup::Arm64BootNetworkInterface<'_>,
    ) -> Option<std::time::Duration> {
        (interface.iface_id() == "eth0")
            .then(|| self.rx_source.inner.mmds_retry_after())
            .flatten()
    }

    fn packet_io(
        &mut self,
        interface: bangbang_runtime::startup::Arm64BootNetworkInterface<'_>,
    ) -> Result<
        bangbang_runtime::startup::Arm64BootNetworkPacketIo<'_>,
        bangbang_runtime::startup::Arm64BootNetworkPacketIoError,
    > {
        if interface.iface_id() != "eth0" {
            return Err(
                bangbang_runtime::startup::Arm64BootNetworkPacketIoError::new(format!(
                    "missing signed guest packet I/O for interface {}",
                    interface.iface_id()
                )),
            );
        }
        Ok(bangbang_runtime::startup::Arm64BootNetworkPacketIo::new(
            &mut self.tx_sink,
            &mut self.rx_source,
        ))
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_to_guest_marker() {
    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let observation = run_guest_boot_until_marker("guest-boot", BOOT_MARKER, |_| {});

    assert_guest_boot_observed_marker(&observation, BOOT_MARKER, "boot marker");
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, BLOCK_READ_MARKER),
        "guest boot test without a drive should not observe block-read marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, BLOCK_WRITE_MARKER),
        "guest boot test without a drive should not observe block-write marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, ROOTFS_READ_MARKER),
        "guest boot test without a drive should not observe rootfs-read marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_with_the_advertised_gicv2m_frame() {
    use std::num::NonZeroU32;

    use bangbang_hvf::HvfGicMsiConfiguration;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let observation = run_guest_boot_with_gic_msi_until_marker(
        "guest-gicv2m",
        BOOT_MARKER,
        HvfGicMsiConfiguration::new(NonZeroU32::new(1).expect("test MSI count should be nonzero")),
        |_| {},
    );

    assert_guest_boot_observed_marker(&observation, BOOT_MARKER, "boot marker");
    let msi = observation
        .gic_msi
        .expect("MSI-enabled guest boot should retain GICv2m metadata");
    let last_intid = msi
        .interrupt_range
        .base
        .checked_add(msi.interrupt_range.count - 1)
        .expect("validated GICv2m range should have an inclusive end");
    let range_marker = format!("SPI[{}:{}]", msi.interrupt_range.base, last_intid);

    assert!(
        bytes_contain_marker(&observation.serial_bytes, b"GICv2m: range"),
        "pinned Linux did not initialize the advertised GICv2m frame\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, range_marker.as_bytes()),
        "pinned Linux did not recognize the exact advertised GICv2m SPI range\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_and_enumerates_the_internal_pci_segment() {
    use std::num::NonZeroU32;

    use bangbang_hvf::HvfGicMsiConfiguration;
    use bangbang_runtime::startup::Arm64BootPciValidationConfig;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let initrd_path = env_path("BANGBANG_GUEST_INITRD_PATH");
    let observation = run_guest_boot_with_boot_source_gic_msi_and_pci_validation(
        "guest-pci-enumeration",
        BOOT_MARKER,
        Some(initrd_path),
        INITRD_BOOT_ARGS,
        Some(HvfGicMsiConfiguration::new(
            NonZeroU32::new(1).expect("test MSI count should be nonzero"),
        )),
        Some(Arm64BootPciValidationConfig::firecracker_test_endpoint()),
        |_| {},
    );

    assert_guest_boot_observed_marker(&observation, BOOT_MARKER, "boot marker");
    for identity in [
        b"pci 0000:00:00.0: [8086:0d57]",
        b"pci 0000:00:01.0: [0042:0000]",
    ] {
        assert!(
            bytes_contain_marker(&observation.serial_bytes, identity),
            "pinned Linux did not enumerate PCI identity {:?}\nserial output:\n{}",
            String::from_utf8_lossy(identity),
            String::from_utf8_lossy(&observation.serial_bytes)
        );
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_with_modern_virtio_pci_rng_and_distinct_msix_vectors() {
    use std::num::NonZeroU32;

    use bangbang_hvf::HvfGicMsiConfiguration;
    use bangbang_runtime::startup::Arm64BootPciValidationConfig;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let initrd_path = env_path("BANGBANG_GUEST_INITRD_PATH");
    let observation = run_guest_boot_with_boot_source_gic_msi_and_pci_validation(
        "guest-modern-virtio-pci-rng",
        VIRTIO_PCI_RNG_SUCCESS_MARKER,
        Some(initrd_path),
        VIRTIO_PCI_RNG_INITRD_BOOT_ARGS,
        Some(HvfGicMsiConfiguration::new(
            NonZeroU32::new(2).expect("queue and config vectors should be nonzero"),
        )),
        Some(Arm64BootPciValidationConfig::modern_virtio_rng()),
        |_| {},
    );

    for (marker, name) in [
        (
            VIRTIO_PCI_RNG_BOUND_MARKER,
            "virtio-pci/virtio-rng binding marker",
        ),
        (
            VIRTIO_PCI_RNG_IO_MARKER,
            "deterministic virtio-rng I/O marker",
        ),
        (
            VIRTIO_PCI_RNG_SUCCESS_MARKER,
            "modern virtio-pci success marker",
        ),
    ] {
        assert_guest_boot_observed_marker(&observation, marker, name);
    }
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, VIRTIO_PCI_RNG_FAILURE_MARKER),
        "modern virtio-pci guest emitted failure marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, b"pci 0000:00:01.0: [1af4:1044]"),
        "pinned Linux did not enumerate the modern virtio-rng PCI identity\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );

    let diagnostics = observation
        .pci_validation
        .as_ref()
        .expect("modern PCI validation should retain endpoint diagnostics");
    assert!(diagnostics.transport.device_activated);
    assert!(diagnostics.transport.driver_ready);
    assert!(diagnostics.transport.msix_enabled);
    assert!(!diagnostics.transport.msix_function_masked);
    assert!(diagnostics.transport.programmed_msix_entries >= 2);
    assert!(diagnostics.transport.unmasked_msix_entries >= 2);
    let queue_vector = diagnostics.transport.queue_vectors[0]
        .expect("Linux should assign the virtio-rng queue vector");
    let config_vector = diagnostics
        .transport
        .config_vector
        .expect("Linux should assign the virtio configuration vector");
    assert_ne!(queue_vector, config_vector);
    assert!(diagnostics.queue_deliveries >= 1);
    assert_eq!(diagnostics.config_deliveries, 1);
    let teardown = observation
        .pci_validation_teardown
        .expect("modern PCI validation should retain teardown evidence");
    assert!(teardown.endpoint_released);
    assert!(teardown.guest_bar_unpublished);
    assert!(teardown.pci_slot_reused);
    assert!(teardown.bar_range_reused);
    assert!(teardown.message_vectors_reused);
    assert!(teardown.stale_endpoint_rejected);

    let before = interrupt_counts_between(
        &observation.serial_bytes,
        VIRTIO_PCI_RNG_IRQ_BEFORE_BEGIN,
        VIRTIO_PCI_RNG_IRQ_BEFORE_END,
    );
    let after = interrupt_counts_between(
        &observation.serial_bytes,
        VIRTIO_PCI_RNG_IRQ_AFTER_BEGIN,
        VIRTIO_PCI_RNG_IRQ_AFTER_END,
    );
    let increased = after
        .iter()
        .filter(|(name, count)| {
            name.contains("virtio") && **count > before.get(*name).copied().unwrap_or(0)
        })
        .count();
    assert!(
        increased >= 2,
        "queue and configuration MSI-X counters should increase independently; before={before:?}, after={after:?}\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_and_executes_userspace_on_secondary_cpu() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::cpu::{
        CpuConfigArmRegisterModifier, CpuConfigArmRegisterWidth, CpuConfigInput,
        KVM_REG_ARM64_CORE_PC, KVM_REG_ARM64_CORE_PSTATE, kvm_reg_arm64_core_x,
    };
    use bangbang_runtime::machine::MachineConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let initrd_path = env_path("BANGBANG_GUEST_INITRD_PATH");
    let observation = run_guest_boot_with_boot_source(
        "guest-smp-secondary",
        SECONDARY_CPU_MARKER,
        Some(initrd_path),
        SMP_INITRD_BOOT_ARGS,
        |controller| {
            controller
                .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 128)))
                .expect("two-vCPU guest machine config should store");
            controller
                .handle_action(VmmAction::PutCpuConfig(CpuConfigInput::new(
                    Vec::new(),
                    vec![
                        CpuConfigArmRegisterModifier::new(
                            kvm_reg_arm64_core_x(0).expect("X0 should have a KVM identity"),
                            CpuConfigArmRegisterWidth::U64,
                            u64::MAX.into(),
                            0x1111_2222_3333_4444,
                        ),
                        CpuConfigArmRegisterModifier::new(
                            KVM_REG_ARM64_CORE_PC,
                            CpuConfigArmRegisterWidth::U64,
                            u64::MAX.into(),
                            0x2000,
                        ),
                        CpuConfigArmRegisterModifier::new(
                            KVM_REG_ARM64_CORE_PSTATE,
                            CpuConfigArmRegisterWidth::U64,
                            0xf000_0000,
                            0xa000_0000,
                        ),
                    ],
                    Vec::new(),
                )))
                .expect("boot-owned CPU-template modifiers should store");
        },
    );

    assert_guest_boot_observed_marker(
        &observation,
        SECONDARY_CPU_MARKER,
        "secondary CPU execution marker",
    );
    assert_eq!(observation.boot_diagnostics.vcpu_mpidrs, [0, 1]);
    assert!(
        observation.run_diagnostics.hvc_steps > 0,
        "two-vCPU guest boot should observe PSCI HVC work\n{}\nserial output:\n{}",
        GuestBootFailureReport::new(&observation.boot_diagnostics, &observation.run_diagnostics),
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_and_reenters_a_hotplugged_secondary_cpu() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::machine::MachineConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let initrd_path = env_path("BANGBANG_GUEST_INITRD_PATH");
    let observation = run_guest_boot_with_boot_source(
        "guest-smp-hotplug",
        SMP_HOTPLUG_DONE_MARKER,
        Some(initrd_path),
        SMP_HOTPLUG_INITRD_BOOT_ARGS,
        |controller| {
            controller
                .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 128)))
                .expect("two-vCPU guest machine config should store");
        },
    );

    for (marker, description) in [
        (SMP_HOTPLUG_READY_MARKER, "hotplug ready marker"),
        (SMP_HOTPLUG_OFF_MARKER, "CPU1 offline marker"),
        (SMP_HOTPLUG_DONE_MARKER, "CPU1 re-entry marker"),
    ] {
        assert_guest_boot_observed_marker(&observation, marker, description);
    }
    assert_eq!(observation.boot_diagnostics.vcpu_mpidrs, [0, 1]);
    assert!(
        observation.run_diagnostics.cpu_off_steps > 0,
        "guest hotplug should observe a non-returning CPU_OFF step\n{}\nserial output:\n{}",
        GuestBootFailureReport::new(&observation.boot_diagnostics, &observation.run_diagnostics),
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_and_reads_virtio_block_marker() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let backing = GuestBlockBacking::new(BLOCK_READ_MARKER);
    let observation =
        run_guest_boot_until_marker("guest-block-read", BLOCK_READ_MARKER, |controller| {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("data", "data", backing.path(), false)
                        .with_is_read_only(true),
                ))
                .expect("guest block read drive should configure");
        });

    assert_guest_boot_observed_marker(&observation, BLOCK_READ_MARKER, "block-read marker");
    assert!(
        bytes_contain_marker(&observation.serial_bytes, BOOT_MARKER),
        "guest block read test should still observe boot marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, BLOCK_WRITE_MARKER),
        "guest block read test with a read-only drive should not observe block-write marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, ROOTFS_READ_MARKER),
        "guest block read test with a raw marker drive should not observe rootfs-read marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_and_writes_virtio_block_marker() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let backing = GuestBlockBacking::zeroed();
    let observation =
        run_guest_boot_until_marker("guest-block-write", BLOCK_WRITE_MARKER, |controller| {
            controller
                .handle_action(VmmAction::PutDrive(DriveConfigInput::new(
                    "data",
                    "data",
                    backing.path(),
                    false,
                )))
                .expect("guest block write drive should configure");
        });

    assert_guest_boot_observed_marker(&observation, BLOCK_WRITE_MARKER, "block-write marker");
    assert!(
        bytes_contain_marker(&observation.serial_bytes, BOOT_MARKER),
        "guest block write test should still observe boot marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    let backing_bytes = backing.bytes();
    assert!(
        backing_bytes.starts_with(BLOCK_WRITE_MARKER),
        "guest block write test should mutate backing with marker {:?}; backing bytes: {:?}",
        String::from_utf8_lossy(BLOCK_WRITE_MARKER),
        backing_bytes
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, ROOTFS_READ_MARKER),
        "guest block write test with a raw writable drive should not observe rootfs-read marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_with_root_drive_boot_args() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let backing = GuestBlockBacking::new(BLOCK_READ_MARKER);
    let observation = run_guest_boot_until_marker(
        "guest-root-drive-cmdline",
        CMDLINE_END_MARKER,
        |controller| {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", backing.path(), true)
                        .with_is_read_only(true),
                ))
                .expect("guest root drive should configure");
        },
    );

    assert_guest_boot_observed_marker(&observation, CMDLINE_END_MARKER, "cmdline end marker");
    let cmdline = guest_cmdline_capture(&observation);
    assert_guest_cmdline_contains_arg(cmdline, b"root=/dev/vda");
    assert_guest_cmdline_contains_arg(cmdline, b"ro");
    assert_guest_cmdline_contains_arg(cmdline, b"rdinit=/init");
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, BLOCK_WRITE_MARKER),
        "guest root-drive cmdline test with a read-only drive should not observe block-write marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, ROOTFS_READ_MARKER),
        "guest root-drive cmdline test with a raw marker drive should not observe rootfs-read marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_and_reads_firecracker_rootfs() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_ROOTFS_PATH");
    let observation =
        run_guest_boot_until_marker("guest-rootfs-read", ROOTFS_READ_MARKER, |controller| {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", rootfs_path.as_path(), true)
                        .with_is_read_only(true),
                ))
                .expect("guest rootfs drive should configure");
        });

    assert_guest_boot_observed_marker(&observation, ROOTFS_READ_MARKER, "rootfs-read marker");
    assert!(
        bytes_contain_marker(&observation.serial_bytes, BOOT_MARKER),
        "guest rootfs read test should still observe boot marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, ROOTFS_OS_RELEASE_ID),
        "guest rootfs read test should observe os-release ID\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, ROOTFS_OS_RELEASE_CODENAME),
        "guest rootfs read test should observe os-release codename\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    let cmdline = guest_cmdline_capture(&observation);
    assert_guest_cmdline_contains_arg(cmdline, b"root=/dev/vda");
    assert_guest_cmdline_contains_arg(cmdline, b"ro");
    assert_guest_cmdline_contains_arg(cmdline, b"rdinit=/init");
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, BLOCK_WRITE_MARKER),
        "guest rootfs read test with a read-only rootfs should not observe block-write marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_from_ext4_rootfs() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let observation = run_guest_boot_without_initrd_until_marker(
        "guest-ext4-rootfs-boot",
        DIRECT_ROOTFS_BOOT_MARKER,
        DIRECT_ROOTFS_BOOT_ARGS,
        |controller| {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", rootfs_path.as_path(), true)
                        .with_is_read_only(true),
                ))
                .expect("guest ext4 rootfs drive should configure");
        },
    );

    assert_guest_boot_observed_marker(
        &observation,
        DIRECT_ROOTFS_BOOT_MARKER,
        "direct rootfs boot marker",
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, ROOTFS_OS_RELEASE_ID),
        "direct rootfs boot should read os-release ID from /\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, ROOTFS_OS_RELEASE_CODENAME),
        "direct rootfs boot should read os-release codename from /\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    let cmdline = guest_cmdline_capture(&observation);
    assert_guest_cmdline_contains_arg(cmdline, b"root=/dev/vda");
    assert_guest_cmdline_contains_arg(cmdline, b"ro");
    assert_guest_cmdline_contains_arg(cmdline, b"init=/bangbang-direct-rootfs-init");
    assert!(
        !guest_cmdline_contains_arg(cmdline, b"rdinit=/init"),
        "direct rootfs boot should not rely on the tiny initrd: {}",
        String::from_utf8_lossy(cmdline)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn certifies_linux_pvtime_contention_idle_and_paused_accounting() {
    use std::time::Duration;

    use bangbang_hvf::HvfArm64PvTimeContentionProbe;
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let boot_args = format!("{DIRECT_ROOTFS_BOOT_ARGS} bangbang.pvtime-check=1");
    let probe = HvfArm64PvTimeContentionProbe::new(Duration::from_millis(1));
    let observation = run_guest_pvtime_certification(
        "guest-pvtime-certification",
        PVTIME_IDLE_MARKER,
        &boot_args,
        probe,
        |controller| {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", rootfs_path.as_path(), true)
                        .with_is_read_only(true),
                ))
                .expect("PVTime certification rootfs drive should configure");
        },
    );

    assert_guest_boot_observed_marker(&observation, PVTIME_IDLE_MARKER, "PVTime idle marker");
    assert!(
        bytes_contain_marker(&observation.serial_bytes, PVTIME_DISCOVERY_MARKER),
        "pinned Linux did not confirm PVTime discovery\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, b"stolen time PV"),
        "pinned Linux did not emit its PVTime discovery message\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, PVTIME_FAILURE_MARKER),
        "PVTime guest certification emitted a failure marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );

    let before = decimal_marker_value(&observation.serial_bytes, b"BANGBANG_PVTIME_BEFORE=");
    let after = decimal_marker_value(&observation.serial_bytes, b"BANGBANG_PVTIME_AFTER=");
    let idle_before =
        decimal_marker_value(&observation.serial_bytes, b"BANGBANG_PVTIME_IDLE_BEFORE=");
    let idle_after =
        decimal_marker_value(&observation.serial_bytes, b"BANGBANG_PVTIME_IDLE_AFTER=");
    assert!(
        after > before,
        "Linux steal ticks should increase under contention"
    );
    assert!(after > 0, "Linux steal ticks should become nonzero");
    assert_eq!(
        idle_after, idle_before,
        "idle steal ticks should not change"
    );

    let (capture_before_pause, capture_after_pause) = observation
        .pvtime_captures
        .expect("PVTime certification should retain paused captures");
    assert_eq!(capture_before_pause, capture_after_pause);
    assert_eq!(capture_before_pause.vcpus().len(), 1);
    assert!(capture_before_pause.vcpus()[0].stolen_time_ns() > 0);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_two_cpu_linux_and_matches_cache_sysfs_to_retained_model() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let cache_report_backing = GuestBlockBacking::zeroed_with_size(CACHE_REPORT_BACKING_SIZE);
    let boot_args = format!("{DIRECT_ROOTFS_BOOT_ARGS} maxcpus=1 bangbang.cache-fdt-check=1");
    let observation = run_guest_boot_without_initrd_until_marker(
        "guest-cache-fdt-check",
        DIRECT_ROOTFS_BOOT_MARKER,
        &boot_args,
        |controller| {
            controller
                .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 128)))
                .expect("two-vCPU cache evidence machine config should store");
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", rootfs_path.as_path(), true)
                        .with_is_read_only(true),
                ))
                .expect("cache evidence rootfs drive should configure");
            controller
                .handle_action(VmmAction::PutDrive(DriveConfigInput::new(
                    "cache_report",
                    "cache_report",
                    cache_report_backing.path(),
                    false,
                )))
                .expect("cache evidence scratch drive should configure");
        },
    );

    assert_guest_boot_observed_marker(
        &observation,
        DIRECT_ROOTFS_BOOT_MARKER,
        "direct rootfs boot marker",
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, CACHE_FDT_GUEST_CHECK_MARKER),
        "cache evidence guest should flush a complete sysfs report before success\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, CACHE_FDT_GUEST_FAILURE_MARKER,),
        "cache evidence guest should not emit its fixed failure marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert_eq!(observation.boot_diagnostics.vcpu_mpidrs, [0, 1]);

    let actual = parse_guest_cache_report(&cache_report_backing.bytes());
    let expected = expected_guest_cache_report(&observation.cache_hierarchy, 2);
    assert_eq!(
        actual, expected,
        "Linux cache sysfs must exactly match the retained production FDT hierarchy"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn two_cpu_linux_observes_exact_custom_id_register_mask_results() {
    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let baseline_backing = GuestBlockBacking::zeroed();
    let custom_backing = GuestBlockBacking::zeroed();

    let baseline_observation = run_guest_cpu_template_report(
        "guest-cpu-template-baseline",
        &rootfs_path,
        &baseline_backing,
        false,
    );
    let custom_observation = run_guest_cpu_template_report(
        "guest-cpu-template-custom",
        &rootfs_path,
        &custom_backing,
        true,
    );
    for observation in [&baseline_observation, &custom_observation] {
        assert_guest_boot_observed_marker(
            observation,
            CPU_TEMPLATE_GUEST_CHECK_MARKER,
            "CPU-template guest report marker",
        );
        assert!(
            !bytes_contain_marker(&observation.serial_bytes, CPU_TEMPLATE_GUEST_FAILURE_MARKER),
            "CPU-template guest report must not emit its fixed failure marker\nserial output:\n{}",
            String::from_utf8_lossy(&observation.serial_bytes)
        );
        assert_eq!(observation.boot_diagnostics.vcpu_mpidrs, [0, 1]);
    }

    let baseline = parse_guest_cpu_template_report(&baseline_backing.bytes());
    let custom = parse_guest_cpu_template_report(&custom_backing.bytes());
    assert_eq!(
        baseline.iter().map(|record| record.cpu).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(
        custom.iter().map(|record| record.cpu).collect::<Vec<_>>(),
        [0, 1]
    );
    for register_index in 0..4 {
        assert!(
            baseline[0].values[register_index] == baseline[1].values[register_index],
            "baseline Linux ID-register view must agree across both vCPUs at register position {register_index}"
        );
        assert!(
            custom[0].values[register_index] == custom[1].values[register_index],
            "custom Linux ID-register view must agree across both vCPUs at register position {register_index}"
        );
    }

    let masks = [
        (0x000f_000f_0000_0000, 0),
        (0xf0ff_0fff_0000_f000, 0x1000),
        (0x00ff_f000_00ff_f00f, 0x0010_0001),
        (0x0000_000f_0000_0000, 0),
    ];
    for (baseline_record, custom_record) in baseline.iter().zip(&custom) {
        assert_eq!(baseline_record.cpu, custom_record.cpu);
        for (index, (filter, value)) in masks.into_iter().enumerate() {
            assert!(
                custom_record.values[index] == (baseline_record.values[index] & !filter) | value,
                "custom guest value must equal the exact baseline mask result for CPU {} register index {index}",
                baseline_record.cpu
            );
        }
    }

    for observation in [&baseline_observation, &custom_observation] {
        for record in baseline.iter().chain(&custom) {
            for value in record.values {
                let encoded = format!("{value:016x}");
                assert!(
                    !observation
                        .serial_bytes
                        .windows(encoded.len())
                        .any(|window| window == encoded.as_bytes()),
                    "guest serial output must not contain raw ID-register values"
                );
            }
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_guest_cpu_template_report(
    instance_id: &str,
    rootfs_path: &std::path::Path,
    report_backing: &GuestBlockBacking,
    custom: bool,
) -> GuestBootObservation {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;
    use bangbang_runtime::cpu::{
        CpuConfigArmRegisterModifier, CpuConfigArmRegisterWidth, CpuConfigInput,
        KVM_REG_ARM64_ID_AA64ISAR0_EL1, KVM_REG_ARM64_ID_AA64ISAR1_EL1,
        KVM_REG_ARM64_ID_AA64MMFR2_EL1, KVM_REG_ARM64_ID_AA64PFR0_EL1,
    };
    use bangbang_runtime::machine::MachineConfigInput;

    let boot_args = format!("{DIRECT_ROOTFS_BOOT_ARGS} maxcpus=1 bangbang.cpu-template-report=1");
    run_guest_boot_without_initrd_until_marker(
        instance_id,
        CPU_TEMPLATE_GUEST_CHECK_MARKER,
        &boot_args,
        |controller| {
            controller
                .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 128)))
                .expect("two-vCPU CPU-template evidence machine config should store");
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", rootfs_path, true)
                        .with_is_read_only(true),
                ))
                .expect("CPU-template evidence rootfs drive should configure");
            controller
                .handle_action(VmmAction::PutDrive(DriveConfigInput::new(
                    "cpu_report",
                    "cpu_report",
                    report_backing.path(),
                    false,
                )))
                .expect("CPU-template evidence scratch drive should configure");
            if custom {
                let modifier = |id, filter, value| {
                    CpuConfigArmRegisterModifier::new(
                        id,
                        CpuConfigArmRegisterWidth::U64,
                        filter,
                        value,
                    )
                };
                controller
                    .handle_action(VmmAction::PutCpuConfig(CpuConfigInput::new(
                        Vec::new(),
                        vec![
                            modifier(KVM_REG_ARM64_ID_AA64PFR0_EL1, 0x000f_000f_0000_0000, 0),
                            modifier(
                                KVM_REG_ARM64_ID_AA64ISAR0_EL1,
                                0xf0ff_0fff_0000_f000,
                                0x1000,
                            ),
                            modifier(
                                KVM_REG_ARM64_ID_AA64ISAR1_EL1,
                                0x00ff_f000_00ff_f00f,
                                0x0010_0001,
                            ),
                            modifier(KVM_REG_ARM64_ID_AA64MMFR2_EL1, 0x0000_000f_0000_0000, 0),
                        ],
                        Vec::new(),
                    )))
                    .expect("canonical guest CPU template should store");
            }
        },
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_exposes_vmgenid_to_guest() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let boot_args = format!("{DIRECT_ROOTFS_BOOT_ARGS} bangbang.vmgenid-check=1");
    let observation = run_guest_boot_without_initrd_until_marker(
        "guest-vmgenid-check",
        DIRECT_ROOTFS_BOOT_MARKER,
        &boot_args,
        |controller| {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", rootfs_path.as_path(), true)
                        .with_is_read_only(true),
                ))
                .expect("guest ext4 rootfs drive should configure");
        },
    );

    assert_guest_boot_observed_marker(
        &observation,
        DIRECT_ROOTFS_BOOT_MARKER,
        "direct rootfs boot marker",
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, VMGENID_GUEST_CHECK_MARKER),
        "direct rootfs boot should expose VMGenID to Linux\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    let cmdline = guest_cmdline_capture(&observation);
    assert_guest_cmdline_contains_arg(cmdline, b"bangbang.vmgenid-check=1");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_reads_and_flushes_virtio_pmem() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;
    use bangbang_runtime::pmem::PmemConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let pmem_backing = GuestPmemBacking::new(PMEM_HOST_MARKER);
    let boot_args = format!("{DIRECT_ROOTFS_BOOT_ARGS} bangbang.pmem-read-flush=1");
    let observation = run_guest_boot_without_initrd_until_marker(
        "guest-pmem-read-flush",
        DIRECT_ROOTFS_BOOT_MARKER,
        &boot_args,
        |controller| {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", rootfs_path.as_path(), true)
                        .with_is_read_only(true),
                ))
                .expect("guest ext4 rootfs drive should configure");
            controller
                .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
                    "pmem0",
                    pmem_backing.path_text(),
                )))
                .expect("guest pmem device should configure");
        },
    );

    assert_guest_boot_observed_marker(
        &observation,
        DIRECT_ROOTFS_BOOT_MARKER,
        "direct rootfs boot marker",
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, PMEM_READ_FLUSH_MARKER),
        "pmem read-flush boot should observe pmem marker\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert_eq!(
        pmem_backing.bytes_at(PMEM_GUEST_FLUSH_OFFSET, PMEM_GUEST_FLUSH_MARKER.len()),
        PMEM_GUEST_FLUSH_MARKER,
        "guest pmem flush should persist the guest marker to the host backing file"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_read_only_ext4_root_directly_from_mmio_pmem() {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::pmem::PmemConfigInput;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let boot_args = format!("{DIRECT_ROOTFS_BOOT_ARGS} rootwait bangbang.pmem-root=ro");
    let observation = run_guest_boot_without_initrd_until_marker(
        "guest-pmem-root-ro",
        PMEM_ROOT_RO_MARKER,
        &boot_args,
        |controller| {
            controller
                .handle_action(VmmAction::PutPmem(
                    PmemConfigInput::new("root_pmem", rootfs_path.to_string_lossy().into_owned())
                        .with_root_device(true)
                        .with_read_only(true),
                ))
                .expect("read-only pmem root should configure");
        },
    );

    assert_guest_boot_observed_marker(
        &observation,
        PMEM_ROOT_RO_MARKER,
        "read-only pmem root marker",
    );
    let cmdline = guest_cmdline_capture(&observation);
    assert_guest_cmdline_contains_arg(cmdline, b"root=/dev/pmem0");
    assert_guest_cmdline_contains_arg(cmdline, b"ro");
    assert_guest_cmdline_contains_arg(cmdline, b"bangbang.pmem-root=ro");
    assert!(
        !guest_cmdline_contains_arg(cmdline, b"root=/dev/vda"),
        "pmem root boot must not retain the block-root argument: {}",
        String::from_utf8_lossy(cmdline)
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, b"BANGBANG_PMEM_ROOT_FAIL_"),
        "read-only pmem root boot must not report a root validation failure\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_writable_ext4_root_directly_from_modern_pci_pmem() {
    use std::num::NonZeroU32;

    use bangbang_hvf::{HvfArm64BootPciDataDeviceKind, HvfGicMsiConfiguration};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::pmem::PmemConfigInput;
    use bangbang_runtime::startup::Arm64BootPciValidationConfig;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let source_rootfs = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let writable_rootfs = GuestWritableRootfs::copy_from(&source_rootfs)
        .expect("writable pmem root fixture should copy");
    let boot_args = format!(
        "{DIRECT_ROOTFS_BOOT_ARGS} rootwait bangbang.pmem-root=rw bangbang.expect-pci-data=1"
    );
    let observation = run_guest_boot_with_boot_source_gic_msi_and_pci_validation(
        "guest-pci-pmem-root-rw",
        PMEM_ROOT_RW_MARKER,
        None,
        &boot_args,
        Some(HvfGicMsiConfiguration::new(
            NonZeroU32::new(2).expect("one pmem endpoint needs two MSI-X routes"),
        )),
        Some(Arm64BootPciValidationConfig::data_devices()),
        |controller| {
            controller
                .handle_action(VmmAction::PutPmem(
                    PmemConfigInput::new("root_pmem", writable_rootfs.path_text())
                        .with_root_device(true),
                ))
                .expect("writable PCI pmem root should configure");
        },
    );

    assert_guest_boot_observed_marker(
        &observation,
        PMEM_ROOT_RW_MARKER,
        "writable PCI pmem root marker",
    );
    let cmdline = guest_cmdline_capture(&observation);
    assert_guest_cmdline_contains_arg(cmdline, b"root=/dev/pmem0");
    assert_guest_cmdline_contains_arg(cmdline, b"rw");
    assert_guest_cmdline_contains_arg(cmdline, b"bangbang.pmem-root=rw");
    assert!(
        bytes_contain_marker(&observation.serial_bytes, PCI_PMEM_IDENTITIES_MARKER),
        "writable pmem root should enumerate the modern PCI identity\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        !bytes_contain_marker(&observation.serial_bytes, b"BANGBANG_PMEM_ROOT_FAIL_"),
        "writable pmem root boot must not report a root validation failure\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    let diagnostics = observation
        .pci_data_devices
        .as_ref()
        .expect("writable PCI pmem root should retain endpoint diagnostics");
    assert_eq!(diagnostics.len(), 1);
    assert_pci_data_endpoint(
        &diagnostics[0],
        HvfArm64BootPciDataDeviceKind::Pmem,
        "root_pmem",
        1,
    );
    assert_eq!(observation.data_mmio_device_counts, (0, 0, 0));
    assert!(observation.pci_data_device_teardown);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_direct_rootfs_and_fsyncs_block_devices_over_modern_virtio_pci() {
    use std::num::NonZeroU32;

    use bangbang_hvf::{HvfArm64BootPciDataDeviceKind, HvfGicMsiConfiguration};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{DriveCacheType, DriveConfigInput};
    use bangbang_runtime::startup::Arm64BootPciValidationConfig;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let data_backing = GuestBlockBacking::zeroed();
    let boot_args = format!(
        "{DIRECT_ROOTFS_BOOT_ARGS} bangbang.block-writeback-flush=1 bangbang.expect-pci-data=1"
    );
    let observation = run_guest_boot_with_boot_source_gic_msi_and_pci_validation(
        "guest-pci-block-fsync",
        DIRECT_ROOTFS_BOOT_MARKER,
        None,
        &boot_args,
        Some(HvfGicMsiConfiguration::new(
            NonZeroU32::new(4).expect("two block endpoints need four MSI-X routes"),
        )),
        Some(Arm64BootPciValidationConfig::data_devices()),
        |controller| {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", rootfs_path.as_path(), true)
                        .with_is_read_only(true),
                ))
                .expect("PCI rootfs drive should configure");
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("data", "data", data_backing.path(), false)
                        .with_cache_type(DriveCacheType::Writeback),
                ))
                .expect("PCI writable data drive should configure");
        },
    );

    assert_guest_boot_observed_marker(
        &observation,
        DIRECT_ROOTFS_BOOT_MARKER,
        "PCI direct-rootfs boot marker",
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, BLOCK_WRITEBACK_FLUSH_MARKER),
        "PCI block guest did not complete write/fsync\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        data_backing
            .bytes()
            .starts_with(BLOCK_WRITEBACK_FLUSH_MARKER),
        "PCI block fsync marker should persist in the writable backing"
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, PCI_BLOCK_IDENTITIES_MARKER),
        "pinned Linux did not expose both modern PCI block identities in sysfs\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    let diagnostics = observation
        .pci_data_devices
        .as_ref()
        .expect("PCI block proof should retain endpoint diagnostics");
    assert_eq!(diagnostics.len(), 2);
    assert_pci_data_endpoint(
        &diagnostics[0],
        HvfArm64BootPciDataDeviceKind::Block,
        "rootfs",
        1,
    );
    assert_pci_data_endpoint(
        &diagnostics[1],
        HvfArm64BootPciDataDeviceKind::Block,
        "data",
        1,
    );
    assert_eq!(observation.data_mmio_device_counts, (0, 0, 0));
    assert!(observation.pci_data_device_teardown);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_direct_rootfs_and_flushes_pmem_over_modern_virtio_pci() {
    use std::num::NonZeroU32;

    use bangbang_hvf::{HvfArm64BootPciDataDeviceKind, HvfGicMsiConfiguration};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;
    use bangbang_runtime::pmem::PmemConfigInput;
    use bangbang_runtime::startup::Arm64BootPciValidationConfig;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let pmem_backing = GuestPmemBacking::new(PMEM_HOST_MARKER);
    let boot_args =
        format!("{DIRECT_ROOTFS_BOOT_ARGS} bangbang.pmem-read-flush=1 bangbang.expect-pci-data=1");
    let observation = run_guest_boot_with_boot_source_gic_msi_and_pci_validation(
        "guest-pci-pmem-flush",
        DIRECT_ROOTFS_BOOT_MARKER,
        None,
        &boot_args,
        Some(HvfGicMsiConfiguration::new(
            NonZeroU32::new(4).expect("block and pmem endpoints need four MSI-X routes"),
        )),
        Some(Arm64BootPciValidationConfig::data_devices()),
        |controller| {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("rootfs", "rootfs", rootfs_path.as_path(), true)
                        .with_is_read_only(true),
                ))
                .expect("PCI rootfs drive should configure");
            controller
                .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
                    "pmem0",
                    pmem_backing.path_text(),
                )))
                .expect("PCI pmem device should configure");
        },
    );

    assert_guest_boot_observed_marker(
        &observation,
        DIRECT_ROOTFS_BOOT_MARKER,
        "PCI pmem direct-rootfs boot marker",
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, PMEM_READ_FLUSH_MARKER),
        "PCI pmem guest did not complete read/flush\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert_eq!(
        pmem_backing.bytes_at(PMEM_GUEST_FLUSH_OFFSET, PMEM_GUEST_FLUSH_MARKER.len()),
        PMEM_GUEST_FLUSH_MARKER,
        "PCI pmem flush should persist the guest marker"
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, PCI_PMEM_IDENTITIES_MARKER),
        "pinned Linux did not expose modern PCI block and pmem identities in sysfs\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    let diagnostics = observation
        .pci_data_devices
        .as_ref()
        .expect("PCI pmem proof should retain endpoint diagnostics");
    assert_eq!(diagnostics.len(), 2);
    assert_pci_data_endpoint(
        &diagnostics[0],
        HvfArm64BootPciDataDeviceKind::Block,
        "rootfs",
        1,
    );
    assert_pci_data_endpoint(
        &diagnostics[1],
        HvfArm64BootPciDataDeviceKind::Pmem,
        "pmem0",
        1,
    );
    assert_eq!(observation.data_mmio_device_counts, (0, 0, 0));
    assert!(observation.pci_data_device_teardown);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn configure_signed_guest_network_semantics(
    controller: &mut bangbang_runtime::VmmController,
    rootfs_path: &std::path::Path,
) {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::DriveConfigInput;
    use bangbang_runtime::mmds::{MmdsConfigInput, MmdsContentInput, MmdsVersion};
    use bangbang_runtime::network::{
        NetworkInterfaceConfigInput, NetworkRateLimiterConfig, NetworkTokenBucketConfig,
    };

    controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", rootfs_path, true).with_is_read_only(true),
        ))
        .expect("signed network rootfs drive should configure");
    controller
        .handle_action(VmmAction::PutNetworkInterface(
            NetworkInterfaceConfigInput::new("eth0", "eth0", "vmnet:shared")
                .with_mtu(50_000)
                .with_rx_rate_limiter(NetworkRateLimiterConfig::new(
                    None,
                    Some(NetworkTokenBucketConfig::new(1, None, 20)),
                )),
        ))
        .expect("signed MMDS network interface should configure");
    controller
        .handle_action(VmmAction::PutMmdsConfig(
            MmdsConfigInput::new(vec!["eth0".to_string()]).with_version(MmdsVersion::V2),
        ))
        .expect("signed MMDS interface selection should configure");
    controller
        .handle_action(VmmAction::PutMmds(MmdsContentInput::new(
            serde_json::json!({
                "meta-data": {
                    "bangbang-marker": "BANGBANG_MMDS_GUEST_VALUE",
                    "bangbang-large": "z".repeat(48 * 1024)
                }
            }),
        )))
        .expect("signed MMDS semantic data should configure");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn signed_guest_network_packet_io(
    controller: &bangbang_runtime::VmmController,
    evidence: std::sync::Arc<SignedGuestNetworkSemanticEvidence>,
) -> SignedGuestNetworkPacketIoProvider {
    use bangbang_runtime::metrics::SharedMmdsMetrics;
    use bangbang_runtime::mmds::DEFAULT_MMDS_IPV4_ADDRESS;
    use bangbang_runtime::mmds_network::{MmdsOnlyVirtioNetworkPacketIo, MmdsPacketDetour};

    let detour = MmdsPacketDetour::try_new(
        controller.mmds_state_handle(),
        DEFAULT_MMDS_IPV4_ADDRESS,
        SharedMmdsMetrics::default(),
    )
    .expect("signed MMDS session should build");
    let packet_io =
        MmdsOnlyVirtioNetworkPacketIo::new(detour).expect("signed MMDS packet I/O should build");
    SignedGuestNetworkPacketIoProvider::new(packet_io, evidence)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn expected_signed_guest_network_features() -> u64 {
    use bangbang_runtime::network::{
        VIRTIO_FEATURE_VERSION_1, VIRTIO_NET_F_CSUM, VIRTIO_NET_F_GUEST_CSUM,
        VIRTIO_NET_F_GUEST_TSO4, VIRTIO_NET_F_GUEST_TSO6, VIRTIO_NET_F_GUEST_UFO,
        VIRTIO_NET_F_HOST_TSO4, VIRTIO_NET_F_HOST_TSO6, VIRTIO_NET_F_HOST_UFO,
        VIRTIO_NET_F_MRG_RXBUF, VIRTIO_NET_F_MTU, VIRTIO_RING_FEATURE_EVENT_IDX,
        VIRTIO_RING_FEATURE_INDIRECT_DESC,
    };

    [
        VIRTIO_NET_F_CSUM,
        VIRTIO_NET_F_GUEST_CSUM,
        VIRTIO_NET_F_MTU,
        VIRTIO_NET_F_GUEST_TSO4,
        VIRTIO_NET_F_GUEST_TSO6,
        VIRTIO_NET_F_GUEST_UFO,
        VIRTIO_NET_F_HOST_TSO4,
        VIRTIO_NET_F_HOST_TSO6,
        VIRTIO_NET_F_HOST_UFO,
        VIRTIO_NET_F_MRG_RXBUF,
        VIRTIO_RING_FEATURE_INDIRECT_DESC,
        VIRTIO_RING_FEATURE_EVENT_IDX,
        VIRTIO_FEATURE_VERSION_1,
    ]
    .into_iter()
    .fold(0, |features, feature| features | (1_u64 << feature))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_signed_guest_network_semantics(
    observation: &GuestBootObservation,
    evidence: &SignedGuestNetworkSemanticEvidence,
    negotiated_features: u64,
    transport: &str,
) {
    let metrics = observation
        .network_interface_metrics
        .expect("signed network proof should retain eth0 metrics");
    assert_guest_boot_observed_marker(
        observation,
        DIRECT_ROOTFS_BOOT_MARKER,
        &format!("{transport} network direct-rootfs boot marker"),
    );
    for (marker, description) in [
        (MMDS_FETCH_MARKER, "baseline MMDS fetch"),
        (MMDS_V2_RENEW_MARKER, "MMDS v2 token renewal"),
        (VIRTIO_NETWORK_LARGE_TX_MARKER, "large guest TCP transmit"),
        (VIRTIO_NETWORK_LARGE_RX_MARKER, "large merged receive"),
        (VIRTIO_NETWORK_SEMANTICS_MARKER, "virtio-net semantic proof"),
    ] {
        assert!(
            bytes_contain_marker(&observation.serial_bytes, marker),
            "{transport} guest did not complete {description}; negotiated_features={negotiated_features:#x}, maximum_emitted_packets={}, sink_errors={:?}, metrics={metrics:?}\nserial output:\n{}",
            evidence.maximum_emitted_packets(),
            evidence.errors(),
            String::from_utf8_lossy(&observation.serial_bytes)
        );
    }

    let expected_features = expected_signed_guest_network_features();
    assert_eq!(
        negotiated_features, expected_features,
        "{transport} Linux guest did not negotiate the complete published virtio-net feature set"
    );
    assert!(
        evidence.maximum_emitted_packets() > 1,
        "{transport} large guest TCP request did not exercise software segmentation"
    );
    assert!(
        evidence.mmds_payload_frames() > 1,
        "{transport} MMDS response did not exercise TCP segmentation"
    );
    assert!(
        evidence.dropped_ack(),
        "{transport} MMDS loss harness did not withhold a guest ACK"
    );
    assert!(
        evidence.retransmission_observed(),
        "{transport} MMDS session did not retransmit after the withheld ACK: {evidence:?}"
    );
    assert!(metrics.tx_bytes_count() > 3000);
    assert!(metrics.rx_bytes_count() > 48 * 1024);
    assert!(metrics.rx_rate_limiter_throttled() > 0);
    assert!(metrics.rx_rate_limiter_event_count() > 0);
    assert_eq!(metrics.rx_fails(), 0);
    assert_eq!(metrics.tx_fails(), 0);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_signed_mmio_guest_with_complete_virtio_network_semantics() {
    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let boot_args = format!("{DIRECT_ROOTFS_BOOT_ARGS} bangbang.virtio-net-semantics=1");
    let evidence = std::sync::Arc::new(SignedGuestNetworkSemanticEvidence::default());
    let provider_evidence = std::sync::Arc::clone(&evidence);
    let observation = run_guest_boot_with_boot_source_gic_msi_pci_validation_and_packet_io(
        "guest-mmio-network-semantics",
        DIRECT_ROOTFS_BOOT_MARKER,
        None,
        &boot_args,
        None,
        GuestBootPciIoSetup::new(None, move |controller: &bangbang_runtime::VmmController| {
            Some(signed_guest_network_packet_io(
                controller,
                provider_evidence,
            ))
        }),
        |controller| configure_signed_guest_network_semantics(controller, rootfs_path.as_path()),
    );

    let negotiated_features = observation
        .mmio_network_driver_features
        .expect("MMIO network proof should retain negotiated features");
    assert_signed_guest_network_semantics(&observation, &evidence, negotiated_features, "MMIO");
    assert_eq!(observation.data_mmio_device_counts, (1, 0, 1));
    assert!(observation.pci_data_devices.is_none());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_signed_pci_guest_with_complete_virtio_network_semantics() {
    use std::num::NonZeroU32;

    use bangbang_hvf::{HvfArm64BootPciDataDeviceKind, HvfGicMsiConfiguration};
    use bangbang_runtime::startup::Arm64BootPciValidationConfig;

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let rootfs_path = env_path("BANGBANG_GUEST_EXT4_ROOTFS_PATH");
    let boot_args = format!(
        "{DIRECT_ROOTFS_BOOT_ARGS} bangbang.virtio-net-semantics=1 bangbang.expect-pci-data=1"
    );
    let evidence = std::sync::Arc::new(SignedGuestNetworkSemanticEvidence::default());
    let provider_evidence = std::sync::Arc::clone(&evidence);
    let observation = run_guest_boot_with_boot_source_gic_msi_pci_validation_and_packet_io(
        "guest-pci-network-semantics",
        DIRECT_ROOTFS_BOOT_MARKER,
        None,
        &boot_args,
        Some(HvfGicMsiConfiguration::new(NonZeroU32::new(5).expect(
            "block and network endpoints need five MSI-X routes",
        ))),
        GuestBootPciIoSetup::new(
            Some(Arm64BootPciValidationConfig::data_devices()),
            move |controller: &bangbang_runtime::VmmController| {
                Some(signed_guest_network_packet_io(
                    controller,
                    provider_evidence,
                ))
            },
        ),
        |controller| configure_signed_guest_network_semantics(controller, rootfs_path.as_path()),
    );

    assert!(
        bytes_contain_marker(&observation.serial_bytes, PCI_NETWORK_IDENTITIES_MARKER),
        "pinned Linux did not expose modern PCI block and network identities in sysfs\nserial output:\n{}",
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    let diagnostics = observation
        .pci_data_devices
        .as_ref()
        .expect("PCI network proof should retain endpoint diagnostics");
    assert_eq!(diagnostics.len(), 2);
    assert_pci_data_endpoint(
        &diagnostics[0],
        HvfArm64BootPciDataDeviceKind::Block,
        "rootfs",
        1,
    );
    assert_pci_data_endpoint(
        &diagnostics[1],
        HvfArm64BootPciDataDeviceKind::Network,
        "eth0",
        2,
    );
    let negotiated_features = diagnostics[1].transport.driver_features;
    assert_signed_guest_network_semantics(&observation, &evidence, negotiated_features, "PCI");
    assert_eq!(observation.data_mmio_device_counts, (0, 0, 0));
    assert!(observation.pci_data_device_teardown);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_guest_boot_until_marker(
    instance_id: &str,
    marker: &[u8],
    configure_controller: impl FnOnce(&mut bangbang_runtime::VmmController),
) -> GuestBootObservation {
    let initrd_path = env_path("BANGBANG_GUEST_INITRD_PATH");
    run_guest_boot_with_boot_source(
        instance_id,
        marker,
        Some(initrd_path),
        INITRD_BOOT_ARGS,
        configure_controller,
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_guest_boot_with_gic_msi_until_marker(
    instance_id: &str,
    marker: &[u8],
    gic_msi: bangbang_hvf::HvfGicMsiConfiguration,
    configure_controller: impl FnOnce(&mut bangbang_runtime::VmmController),
) -> GuestBootObservation {
    let initrd_path = env_path("BANGBANG_GUEST_INITRD_PATH");
    run_guest_boot_with_boot_source_and_gic_msi(
        instance_id,
        marker,
        Some(initrd_path),
        INITRD_BOOT_ARGS,
        Some(gic_msi),
        configure_controller,
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_guest_boot_without_initrd_until_marker(
    instance_id: &str,
    marker: &[u8],
    boot_args: &str,
    configure_controller: impl FnOnce(&mut bangbang_runtime::VmmController),
) -> GuestBootObservation {
    run_guest_boot_with_boot_source(instance_id, marker, None, boot_args, configure_controller)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_guest_boot_with_boot_source(
    instance_id: &str,
    marker: &[u8],
    initrd_path: Option<std::path::PathBuf>,
    boot_args: &str,
    configure_controller: impl FnOnce(&mut bangbang_runtime::VmmController),
) -> GuestBootObservation {
    run_guest_boot_with_boot_source_and_gic_msi(
        instance_id,
        marker,
        initrd_path,
        boot_args,
        None,
        configure_controller,
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_guest_boot_with_boot_source_and_gic_msi(
    instance_id: &str,
    marker: &[u8],
    initrd_path: Option<std::path::PathBuf>,
    boot_args: &str,
    gic_msi: Option<bangbang_hvf::HvfGicMsiConfiguration>,
    configure_controller: impl FnOnce(&mut bangbang_runtime::VmmController),
) -> GuestBootObservation {
    run_guest_boot_with_boot_source_gic_msi_and_pci_validation(
        instance_id,
        marker,
        initrd_path,
        boot_args,
        gic_msi,
        None,
        configure_controller,
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_guest_boot_with_boot_source_gic_msi_and_pci_validation(
    instance_id: &str,
    marker: &[u8],
    initrd_path: Option<std::path::PathBuf>,
    boot_args: &str,
    gic_msi: Option<bangbang_hvf::HvfGicMsiConfiguration>,
    pci_validation: Option<bangbang_runtime::startup::Arm64BootPciValidationConfig>,
    configure_controller: impl FnOnce(&mut bangbang_runtime::VmmController),
) -> GuestBootObservation {
    run_guest_boot_with_boot_source_gic_msi_pci_validation_and_packet_io(
        instance_id,
        marker,
        initrd_path,
        boot_args,
        gic_msi,
        GuestBootPciIoSetup::new(pci_validation, |_: &bangbang_runtime::VmmController| {
            None::<bangbang_runtime::mmds_network::MmdsOnlyVirtioNetworkPacketIoProvider>
        }),
        configure_controller,
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_guest_pvtime_certification(
    instance_id: &str,
    marker: &[u8],
    boot_args: &str,
    probe: bangbang_hvf::HvfArm64PvTimeContentionProbe,
    configure_controller: impl FnOnce(&mut bangbang_runtime::VmmController),
) -> GuestBootObservation {
    run_guest_boot_with_boot_source_gic_msi_pci_validation_and_packet_io(
        instance_id,
        marker,
        None,
        boot_args,
        None,
        GuestBootPciIoSetup::new(None, |_: &bangbang_runtime::VmmController| {
            None::<bangbang_runtime::mmds_network::MmdsOnlyVirtioNetworkPacketIoProvider>
        })
        .with_pvtime_certification(GuestBootPvTimeCertification {
            probe,
            contention_marker: PVTIME_CONTENTION_MARKER,
            contention_disabled: false,
        }),
        configure_controller,
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct GuestBootPciIoSetup<F> {
    validation: Option<bangbang_runtime::startup::Arm64BootPciValidationConfig>,
    packet_io_factory: F,
    pvtime_certification: Option<GuestBootPvTimeCertification>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl<F> GuestBootPciIoSetup<F> {
    fn new(
        validation: Option<bangbang_runtime::startup::Arm64BootPciValidationConfig>,
        packet_io_factory: F,
    ) -> Self {
        Self {
            validation,
            packet_io_factory,
            pvtime_certification: None,
        }
    }

    fn with_pvtime_certification(mut self, certification: GuestBootPvTimeCertification) -> Self {
        self.pvtime_certification = Some(certification);
        self
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct GuestBootPvTimeCertification {
    probe: bangbang_hvf::HvfArm64PvTimeContentionProbe,
    contention_marker: &'static [u8],
    contention_disabled: bool,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_guest_boot_with_boot_source_gic_msi_pci_validation_and_packet_io<P, F>(
    instance_id: &str,
    marker: &[u8],
    initrd_path: Option<std::path::PathBuf>,
    boot_args: &str,
    gic_msi: Option<bangbang_hvf::HvfGicMsiConfiguration>,
    pci_io: GuestBootPciIoSetup<F>,
    configure_controller: impl FnOnce(&mut bangbang_runtime::VmmController),
) -> GuestBootObservation
where
    P: bangbang_runtime::startup::Arm64BootNetworkPacketIoProvider,
    F: FnOnce(&bangbang_runtime::VmmController) -> Option<P>,
{
    use std::num::NonZeroUsize;
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::rtc::RtcMmioLayout;
    use bangbang_runtime::serial::{SharedSerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::vsock::VsockMmioLayout;
    use bangbang_runtime::{VmmAction, VmmController};

    let GuestBootPciIoSetup {
        validation: pci_validation,
        packet_io_factory,
        mut pvtime_certification,
    } = pci_io;

    let kernel_path = env_path("BANGBANG_GUEST_KERNEL_PATH");
    let serial_output = SharedSerialOutputBuffer::default();
    let mut controller = VmmController::new(instance_id, "0.1.0", "bangbang");
    let mut boot_source = BootSourceConfigInput::new(kernel_path.clone()).with_boot_args(boot_args);
    if let Some(path) = initrd_path.as_ref() {
        boot_source = boot_source.with_initrd_path(path.clone());
    }
    controller
        .handle_action(VmmAction::PutBootSource(boot_source))
        .expect("guest boot test boot source should configure");
    configure_controller(&mut controller);
    let mut packet_io = packet_io_factory(&controller);
    let serial_address = GuestAddress::new(SERIAL_MMIO_BASE);
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(BLOCK_MMIO_BASE), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(PMEM_MMIO_BASE), MmioRegionId::new(500)),
        NetworkMmioLayout::new(
            GuestAddress::new(NETWORK_MMIO_BASE),
            MmioRegionId::new(1000),
        ),
        VsockMmioLayout::new(GuestAddress::new(VSOCK_MMIO_BASE), MmioRegionId::new(2000)),
        RtcMmioLayout::new(GuestAddress::new(RTC_MMIO_BASE), MmioRegionId::new(3000)),
    )
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(0),
        serial_address,
        SharedSerialOutput::from(serial_output.clone()),
    ));
    let config = match gic_msi {
        Some(configuration) => config.with_gic_msi(configuration),
        None => config,
    };
    let config = match pci_validation {
        Some(validation) => config.with_pci_validation(validation),
        None => config,
    };
    let config = match pvtime_certification.as_ref() {
        Some(certification) => config.with_pvtime_contention_probe(certification.probe.clone()),
        None => config,
    };
    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("guest boot test session should prepare");
    if pvtime_certification.is_some() {
        assert!(
            session.runtime_resources().pvtime_state.advertised(),
            "PVTime certification requires complete production owner configuration"
        );
    }
    let cache_hierarchy = session
        .arm64_fdt_cache_hierarchy()
        .expect("ordinary boot session should retain its cache hierarchy")
        .clone();
    let gic_msi = session.gic_metadata().msi;
    let boot_diagnostics =
        GuestBootDiagnostics::from_session(&session, kernel_path, initrd_path, serial_address);
    let data_mmio_device_counts = (
        session.runtime_resources().block_devices.len(),
        session.runtime_resources().pmem_mmio_devices.len(),
        session.runtime_resources().network_devices.len(),
    );
    validate_pre_run_boot_metadata(&session, &boot_diagnostics);
    let run_loop_control = session.run_loop_control();
    let stop_token = run_loop_control.stop_token();
    let watchdog = GuestBootWatchdog::spawn(run_loop_control.clone());
    let one_step = NonZeroUsize::new(1).expect("one-step limit should be non-zero");
    let started_at = Instant::now();
    let mut run_diagnostics = GuestBootRunDiagnostics::default();
    let mut terminal_outcome = None;

    while started_at.elapsed() < GUEST_BOOT_TIMEOUT {
        if serial_contains_marker(&serial_output, marker) {
            break;
        }

        let outcome = match packet_io.as_mut() {
            Some(packet_io) => session
                .run_loop_with_network_packet_io(&stop_token, one_step, packet_io)
                .expect("guest boot network run-loop should not fail before marker"),
            None => session
                .run_loop_with_observer(&stop_token, one_step, |step| {
                    run_diagnostics.record_step(step);
                })
                .expect("guest boot test run-loop should not fail before marker"),
        };
        run_diagnostics.record_loop_outcome(&outcome);
        if let Some(certification) = pvtime_certification.as_mut()
            && !certification.contention_disabled
            && serial_contains_marker(&serial_output, certification.contention_marker)
        {
            certification.probe.set_enabled(false);
            certification.contention_disabled = true;
        }
        session
            .dispatch_pci_validation_notifications()
            .expect("PCI validation device dispatch should succeed");
        if serial_contains_marker(&serial_output, VIRTIO_PCI_RNG_IO_MARKER) {
            session
                .trigger_pci_validation_config_interrupt()
                .expect("PCI validation configuration interrupt should succeed");
        }

        if serial_contains_marker(&serial_output, marker) {
            break;
        }

        if !run_diagnostics.loop_outcome_was_resumable(&outcome) {
            terminal_outcome = Some(outcome);
            break;
        }
    }

    let marker_observed = serial_contains_marker(&serial_output, marker);
    let stop_requested_after_loop = if marker_observed {
        false
    } else {
        let _ = run_loop_control.request_stop();
        true
    };
    let watchdog_timed_out = watchdog.finish();
    let elapsed = started_at.elapsed();
    let serial_bytes = serial_output
        .bytes()
        .expect("guest boot test serial output should read");
    run_diagnostics.finish(
        elapsed,
        marker_observed,
        stop_requested_after_loop,
        watchdog_timed_out,
        &serial_bytes,
        terminal_outcome.as_ref(),
    );
    let pvtime_captures =
        if let (true, Some(certification)) = (marker_observed, pvtime_certification.as_ref()) {
            assert!(
                certification.contention_disabled,
                "PVTime contention probe should be disabled before idle certification"
            );
            session
                .pause_idle_for_arm64_pvtime_capture()
                .expect("PVTime certification should establish an idle pause barrier");
            let before = session
                .capture_arm64_pvtime()
                .expect("first paused PVTime capture should succeed");
            std::thread::sleep(std::time::Duration::from_millis(250));
            let after = session
                .capture_arm64_pvtime()
                .expect("second paused PVTime capture should succeed");
            Some((before, after))
        } else {
            None
        };
    let mmio_network_driver_features = first_mmio_network_driver_features(&session);
    let network_interface_metrics = session
        .shared_network_interface_metrics()
        .per_interface("eth0")
        .map(|metrics| metrics.snapshot());
    let pci_validation = session
        .pci_validation_diagnostics()
        .map(|diagnostics| diagnostics.expect("PCI validation diagnostics should be available"));
    let pci_validation_teardown = session
        .teardown_pci_validation_endpoint()
        .expect("PCI validation endpoint teardown should succeed");
    let pci_data_devices = session
        .pci_data_device_diagnostics()
        .map(|diagnostics| diagnostics.expect("PCI data diagnostics should be available"));
    let pci_data_device_teardown = pci_data_devices.is_some();
    session
        .teardown_pci_data_devices()
        .expect("PCI data devices should tear down");
    session
        .shutdown()
        .expect("guest boot test session should shut down");

    GuestBootObservation {
        boot_diagnostics,
        run_diagnostics,
        serial_bytes,
        cache_hierarchy,
        gic_msi,
        pci_validation,
        pci_validation_teardown,
        pci_data_devices,
        pci_data_device_teardown,
        data_mmio_device_counts,
        pvtime_captures,
        mmio_network_driver_features,
        network_interface_metrics,
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn first_mmio_network_driver_features(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
) -> Option<u64> {
    let region_id = session
        .runtime_resources()
        .network_devices
        .first()?
        .registration
        .region_id();
    let dispatcher = session.mmio_dispatcher();
    let mut dispatcher = dispatcher
        .lock()
        .expect("signed network proof should lock the MMIO dispatcher");
    Some(
        bangbang_runtime::network::network_mmio_driver_features(&mut dispatcher, region_id)
            .expect("signed network proof should find its MMIO network handler"),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_guest_boot_observed_marker(
    observation: &GuestBootObservation,
    marker: &[u8],
    marker_name: &str,
) {
    assert!(
        !observation.run_diagnostics.watchdog_timed_out,
        "guest boot test watchdog canceled the vCPU run\n{}\nserial output:\n{}",
        GuestBootFailureReport::new(&observation.boot_diagnostics, &observation.run_diagnostics),
        String::from_utf8_lossy(&observation.serial_bytes)
    );
    assert!(
        bytes_contain_marker(&observation.serial_bytes, marker),
        "guest boot test did not observe {marker_name} {:?}\n{}\nserial output:\n{}",
        String::from_utf8_lossy(marker),
        GuestBootFailureReport::new(&observation.boot_diagnostics, &observation.run_diagnostics),
        String::from_utf8_lossy(&observation.serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_pci_data_endpoint(
    diagnostics: &bangbang_hvf::HvfArm64BootPciDataDeviceDiagnostics,
    kind: bangbang_hvf::HvfArm64BootPciDataDeviceKind,
    id: &str,
    queue_count: usize,
) {
    assert_eq!(diagnostics.kind, kind);
    assert_eq!(diagnostics.id, id);
    assert!(diagnostics.transport.device_activated);
    assert!(diagnostics.transport.driver_ready);
    assert!(diagnostics.transport.msix_enabled);
    assert!(!diagnostics.transport.msix_function_masked);
    assert_eq!(diagnostics.transport.queue_vectors.len(), queue_count);
    assert!(
        diagnostics
            .transport
            .queue_vectors
            .iter()
            .all(|vector| vector.is_some())
    );
    let config_vector = diagnostics
        .transport
        .config_vector
        .expect("PCI data endpoint should program its config vector");
    let mut distinct_vectors = diagnostics
        .transport
        .queue_vectors
        .iter()
        .map(|vector| vector.expect("PCI data queue vector should be programmed"))
        .collect::<Vec<_>>();
    distinct_vectors.push(config_vector);
    distinct_vectors.sort_unstable();
    distinct_vectors.dedup();
    assert_eq!(distinct_vectors.len(), queue_count + 1);
    assert_eq!(
        diagnostics.transport.programmed_msix_entries,
        queue_count + 1
    );
    assert_eq!(diagnostics.transport.unmasked_msix_entries, queue_count + 1);
    assert!(diagnostics.queue_deliveries >= 1);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct GuestBootObservation {
    boot_diagnostics: GuestBootDiagnostics,
    run_diagnostics: GuestBootRunDiagnostics,
    serial_bytes: Vec<u8>,
    cache_hierarchy: bangbang_runtime::fdt::Arm64FdtCacheHierarchy,
    gic_msi: Option<bangbang_hvf::HvfGicMsiMetadata>,
    pci_validation: Option<bangbang_hvf::HvfArm64BootPciValidationDiagnostics>,
    pci_validation_teardown: Option<bangbang_hvf::HvfArm64BootPciValidationTeardownEvidence>,
    pci_data_devices: Option<Vec<bangbang_hvf::HvfArm64BootPciDataDeviceDiagnostics>>,
    pci_data_device_teardown: bool,
    data_mmio_device_counts: (usize, usize, usize),
    pvtime_captures: Option<(
        bangbang_hvf::HvfArm64PvTimeCaptureState,
        bangbang_hvf::HvfArm64PvTimeCaptureState,
    )>,
    mmio_network_driver_features: Option<u64>,
    network_interface_metrics: Option<bangbang_runtime::metrics::NetworkInterfaceMetrics>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct GuestCpuTemplateReportRecord {
    cpu: u32,
    values: [u64; 4],
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn parse_guest_cpu_template_report(backing: &[u8]) -> Vec<GuestCpuTemplateReportRecord> {
    let report_end = backing
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(backing.len());
    assert!(
        backing[report_end..].iter().all(|byte| *byte == 0),
        "CPU-template report bytes after the bounded payload should remain zero"
    );
    let report =
        std::str::from_utf8(&backing[..report_end]).expect("CPU-template report should be UTF-8");
    let mut lines = report.lines();
    assert_eq!(
        lines.next(),
        Some(CPU_TEMPLATE_REPORT_HEADER),
        "CPU-template report should start with its version marker"
    );

    let mut records = Vec::new();
    while let Some(cpu_line) = lines.next() {
        let cpu = cpu_line
            .strip_prefix("cpu=")
            .expect("CPU-template member should begin with cpu=")
            .parse()
            .expect("CPU-template member index should be an unsigned integer");
        let mut values = [0_u64; 4];
        for (index, name) in ["pfr0=", "isar0=", "isar1=", "mmfr2="]
            .into_iter()
            .enumerate()
        {
            let line = lines
                .next()
                .expect("CPU-template member should contain all four registers");
            let value = line
                .strip_prefix(name)
                .expect("CPU-template register order should be canonical");
            assert_eq!(
                value.len(),
                16,
                "CPU-template register should contain exactly 64 bits"
            );
            assert!(
                value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "CPU-template register should contain lowercase hexadecimal digits"
            );
            values[index] = u64::from_str_radix(value, 16)
                .expect("CPU-template register should contain lowercase hexadecimal digits");
        }
        records.push(GuestCpuTemplateReportRecord { cpu, values });
    }
    assert!(
        !records.is_empty(),
        "CPU-template report should contain members"
    );
    assert!(
        records.windows(2).all(|pair| pair[0].cpu < pair[1].cpu),
        "CPU-template report members should be unique and ordered"
    );
    records
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GuestCacheReportRecord {
    cpu: u32,
    level: u8,
    cache_type: bangbang_runtime::fdt::Arm64FdtCacheType,
    size: u32,
    line_size: u32,
    sets: u32,
    ways: u32,
    shared_cpu_list: String,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn parse_guest_cache_report(backing: &[u8]) -> Vec<GuestCacheReportRecord> {
    let report_end = backing
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(backing.len());
    assert!(
        backing[report_end..].iter().all(|byte| *byte == 0),
        "cache report scratch bytes after the bounded report should remain zero"
    );
    let report = std::str::from_utf8(&backing[..report_end])
        .expect("cache report should contain only UTF-8 normalized fields");
    let mut lines = report.lines();
    assert_eq!(
        lines.next(),
        Some(CACHE_REPORT_HEADER),
        "cache report should start with the version marker"
    );

    let mut records = Vec::new();
    for line in lines {
        assert!(
            !line.is_empty(),
            "cache report should not contain empty records"
        );
        let fields = line.split('|').collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            8,
            "cache report record should have eight normalized fields: {line:?}"
        );
        let cache_type = match fields[2] {
            "D" => bangbang_runtime::fdt::Arm64FdtCacheType::Data,
            "I" => bangbang_runtime::fdt::Arm64FdtCacheType::Instruction,
            "U" => bangbang_runtime::fdt::Arm64FdtCacheType::Unified,
            other => panic!("cache report contains unknown type {other:?}"),
        };
        records.push(GuestCacheReportRecord {
            cpu: fields[0]
                .parse()
                .expect("cache report CPU should be an unsigned integer"),
            level: fields[1]
                .parse()
                .expect("cache report level should be an unsigned integer"),
            cache_type,
            size: fields[3]
                .parse()
                .expect("cache report size should be an unsigned integer"),
            line_size: fields[4]
                .parse()
                .expect("cache report line size should be an unsigned integer"),
            sets: fields[5]
                .parse()
                .expect("cache report set count should be an unsigned integer"),
            ways: fields[6]
                .parse()
                .expect("cache report way count should be an unsigned integer"),
            shared_cpu_list: fields[7].to_string(),
        });
    }
    assert!(
        !records.is_empty(),
        "cache report should contain cache records"
    );
    records.sort();
    assert!(
        records.windows(2).all(|pair| pair[0] != pair[1]),
        "cache report should not contain duplicate normalized records"
    );
    records
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn expected_guest_cache_report(
    hierarchy: &bangbang_runtime::fdt::Arm64FdtCacheHierarchy,
    vcpu_count: u32,
) -> Vec<GuestCacheReportRecord> {
    let mut records = Vec::new();
    for cpu in 0..vcpu_count {
        for cache in hierarchy.caches() {
            let share = cache.cpus_per_unit();
            let first = if cache.level() == 1 {
                cpu
            } else {
                (cpu / share) * share
            };
            let last = if cache.level() == 1 {
                cpu
            } else {
                first
                    .checked_add(share - 1)
                    .expect("validated cache sharing should not overflow")
                    .min(vcpu_count - 1)
            };
            let shared_cpu_list = if first == last {
                first.to_string()
            } else {
                format!("{first}-{last}")
            };
            records.push(GuestCacheReportRecord {
                cpu,
                level: cache.level(),
                cache_type: cache.cache_type(),
                size: cache.size(),
                line_size: cache.line_size(),
                sets: cache.sets(),
                ways: cache.ways(),
                shared_cpu_list,
            });
        }
    }
    records.sort();
    records
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct GuestBlockBacking {
    path: std::path::PathBuf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl GuestBlockBacking {
    fn new(marker: &[u8]) -> Self {
        use std::io::Write;

        let mut path = std::env::temp_dir();
        let unique = format!(
            "bangbang-guest-block-read-{}-{}.img",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("guest block backing timestamp should be after epoch")
                .as_nanos()
        );
        path.push(unique);
        let mut sector = vec![0; bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE as usize];
        assert!(
            marker.len() <= sector.len(),
            "guest block marker should fit in one sector"
        );
        sector[..marker.len()].copy_from_slice(marker);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("guest block backing should create");
        let backing = Self { path };
        file.write_all(&sector)
            .expect("guest block backing sector should write");
        backing
    }

    fn zeroed() -> Self {
        Self::new(&[])
    }

    fn zeroed_with_size(size: u64) -> Self {
        let backing = Self::zeroed();
        assert!(
            size >= bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
            "guest block backing should contain at least one sector"
        );
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&backing.path)
            .expect("guest block backing should reopen for resize");
        file.set_len(size)
            .expect("guest block backing should resize");
        backing
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.path).expect("guest block backing should read")
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for GuestBlockBacking {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct GuestPmemBacking {
    path: std::path::PathBuf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl GuestPmemBacking {
    fn new(marker: &[u8]) -> Self {
        use std::io::Write;

        let mut path = std::env::temp_dir();
        let unique = format!(
            "bangbang-guest-pmem-{}-{}.img",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("guest pmem backing timestamp should be after epoch")
                .as_nanos()
        );
        path.push(unique);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("guest pmem backing should create");
        file.set_len(bangbang_runtime::pmem::VIRTIO_PMEM_ALIGNMENT)
            .expect("guest pmem backing size should be set");
        file.write_all(marker)
            .expect("guest pmem host marker should write");

        Self { path }
    }

    fn path_text(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    fn bytes_at(&self, offset: u64, len: usize) -> Vec<u8> {
        use std::io::{Read, Seek, SeekFrom};

        let mut bytes = vec![0; len];
        let mut file = std::fs::File::open(&self.path).expect("guest pmem backing should open");
        file.seek(SeekFrom::Start(offset))
            .expect("guest pmem backing should seek");
        file.read_exact(&mut bytes)
            .expect("guest pmem backing should read");
        bytes
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for GuestPmemBacking {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct GuestWritableRootfs {
    path: std::path::PathBuf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl GuestWritableRootfs {
    fn copy_from(source: &std::path::Path) -> std::io::Result<Self> {
        let unique = format!(
            "bangbang-guest-pmem-root-{}-{}.ext4",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("guest pmem root timestamp should be after epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::copy(source, &path)?;
        Ok(Self { path })
    }

    fn path_text(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for GuestWritableRootfs {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct GuestBootWatchdog {
    done_sender: Option<std::sync::mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<bool>>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl GuestBootWatchdog {
    fn spawn(control: bangbang_hvf::HvfArm64BootRunLoopControl) -> Self {
        let (done_sender, done_receiver) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            if done_receiver.recv_timeout(GUEST_BOOT_TIMEOUT).is_err() {
                let _ = control.request_stop();
                true
            } else {
                false
            }
        });

        Self {
            done_sender: Some(done_sender),
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> bool {
        self.signal_done();
        self.join().expect("guest boot test watchdog should join")
    }

    fn signal_done(&mut self) {
        if let Some(done_sender) = self.done_sender.take() {
            let _ = done_sender.send(());
        }
    }

    fn join(&mut self) -> std::thread::Result<bool> {
        if let Some(handle) = self.handle.take() {
            handle.join()
        } else {
            Ok(false)
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for GuestBootWatchdog {
    fn drop(&mut self) {
        self.signal_done();
        let _ = self.join();
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug, Clone)]
struct GuestBootDiagnostics {
    kernel_path: std::path::PathBuf,
    initrd_path: Option<std::path::PathBuf>,
    boot_args: String,
    boot_pc: u64,
    fdt_address: u64,
    fdt_size: usize,
    initrd_address: Option<u64>,
    initrd_size: Option<u64>,
    serial_mmio_base: u64,
    serial_mmio_size: u64,
    serial_interrupt_line: u32,
    vcpu_mpidrs: Vec<u64>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl GuestBootDiagnostics {
    fn from_session(
        session: &bangbang_hvf::OwnedHvfArm64BootSession,
        kernel_path: std::path::PathBuf,
        initrd_path: Option<std::path::PathBuf>,
        expected_serial_address: bangbang_runtime::memory::GuestAddress,
    ) -> Self {
        let resources = session.runtime_resources();
        let boot_origin = resources
            .boot_origin
            .as_ref()
            .expect("ordinary guest boot should retain boot-origin metadata");
        let initrd = boot_origin.loaded_boot_source.initrd;
        let (initrd_address, initrd_size) = match initrd {
            Some(loaded) => (Some(loaded.address.raw_value()), Some(loaded.size)),
            None => (None, None),
        };
        let serial = resources
            .serial_device
            .as_ref()
            .expect("guest boot test serial device should be registered");
        assert_eq!(
            serial.region.range().start(),
            expected_serial_address,
            "guest boot test serial MMIO base should match test config"
        );
        assert_eq!(
            Some(serial.fdt_device.interrupt_line),
            session.serial_interrupt_line(),
            "guest boot test runtime and HVF serial interrupt metadata should match"
        );
        let boot_args = boot_origin
            .loaded_boot_source
            .command_line
            .as_str()
            .to_string();
        let boot_registers = session
            .boot_registers()
            .expect("ordinary guest boot should retain boot registers");

        Self {
            kernel_path,
            initrd_path,
            boot_args,
            boot_pc: boot_registers.kernel_entry.raw_value(),
            fdt_address: boot_origin.fdt.address.raw_value(),
            fdt_size: boot_origin.fdt.size,
            initrd_address,
            initrd_size,
            serial_mmio_base: serial.region.range().start().raw_value(),
            serial_mmio_size: serial.region.range().size(),
            serial_interrupt_line: serial.fdt_device.interrupt_line.raw_value(),
            vcpu_mpidrs: session.vcpu_mpidrs().to_vec(),
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GuestBootRunDiagnostics {
    run_loop_calls: usize,
    raw_steps: usize,
    completed_steps: usize,
    resumable_outcomes: usize,
    dirty_write_steps: usize,
    hvc_steps: usize,
    cpu_off_steps: usize,
    cpu_suspend_steps: usize,
    sys64_steps: usize,
    mmio_steps: usize,
    virtual_timer_steps: usize,
    canceled_steps: usize,
    unknown_steps: usize,
    terminal_outcome: Option<String>,
    last_step: Option<String>,
    last_mmio_step: Option<String>,
    elapsed: Option<std::time::Duration>,
    marker_observed: bool,
    stop_requested_after_loop: bool,
    watchdog_timed_out: bool,
    serial_byte_count: usize,
    serial_tail_hex: Option<String>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl GuestBootRunDiagnostics {
    fn record_step(&mut self, step: &bangbang_hvf::HvfVcpuRunStepOutcome) {
        self.raw_steps += 1;
        self.last_step = Some(format!("{step:?}"));
        match step {
            bangbang_hvf::HvfVcpuRunStepOutcome::Canceled => {
                self.canceled_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::DirtyWrite { .. } => {
                self.dirty_write_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::Hvc { .. } => {
                self.hvc_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::CpuOff { .. } => {
                self.cpu_off_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::CpuSuspend { .. } => {
                self.cpu_suspend_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::GuestShutdown { .. }
            | bangbang_hvf::HvfVcpuRunStepOutcome::GuestReset { .. } => {}
            bangbang_hvf::HvfVcpuRunStepOutcome::Sys64 { .. } => {
                self.sys64_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::Mmio { .. } => {
                self.mmio_steps += 1;
                self.last_mmio_step = Some(format!("{step:?}"));
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::VtimerActivated => {
                self.virtual_timer_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::Unknown { .. } => {
                self.unknown_steps += 1;
            }
        }
    }

    fn record_loop_outcome(&mut self, outcome: &bangbang_hvf::HvfArm64BootRunLoopOutcome) {
        self.run_loop_calls += 1;
        self.completed_steps += run_loop_completed_steps(outcome);
        if self.loop_outcome_was_resumable(outcome) {
            self.resumable_outcomes += 1;
        } else {
            self.terminal_outcome = Some(format!("{outcome:?}"));
        }
    }

    fn loop_outcome_was_resumable(
        &self,
        outcome: &bangbang_hvf::HvfArm64BootRunLoopOutcome,
    ) -> bool {
        matches!(
            outcome,
            bangbang_hvf::HvfArm64BootRunLoopOutcome::StepLimitReached { .. }
                | bangbang_hvf::HvfArm64BootRunLoopOutcome::Wakeup { .. }
        )
    }

    fn finish(
        &mut self,
        elapsed: std::time::Duration,
        marker_observed: bool,
        stop_requested_after_loop: bool,
        watchdog_timed_out: bool,
        serial_bytes: &[u8],
        terminal_outcome: Option<&bangbang_hvf::HvfArm64BootRunLoopOutcome>,
    ) {
        self.elapsed = Some(elapsed);
        self.marker_observed = marker_observed;
        self.stop_requested_after_loop = stop_requested_after_loop;
        self.watchdog_timed_out = watchdog_timed_out;
        self.serial_byte_count = serial_bytes.len();
        self.serial_tail_hex = Some(serial_tail_hex(serial_bytes));
        if let Some(outcome) = terminal_outcome {
            self.terminal_outcome = Some(format!("{outcome:?}"));
        }
    }

    fn timeout_classification(&self) -> &'static str {
        if self.marker_observed {
            "marker-observed"
        } else if self.watchdog_timed_out {
            "watchdog-canceled-in-flight-vcpu-run"
        } else if self.terminal_outcome.is_some() {
            "terminal-run-loop-outcome"
        } else if self.run_loop_calls > 0 && self.run_loop_calls == self.resumable_outcomes {
            "outer-timeout-after-handled-steps"
        } else {
            "outer-timeout-without-terminal-outcome"
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug)]
struct GuestBootFailureReport<'a> {
    boot: &'a GuestBootDiagnostics,
    run: &'a GuestBootRunDiagnostics,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl<'a> GuestBootFailureReport<'a> {
    const fn new(boot: &'a GuestBootDiagnostics, run: &'a GuestBootRunDiagnostics) -> Self {
        Self { boot, run }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl std::fmt::Display for GuestBootFailureReport<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let elapsed = self
            .run
            .elapsed
            .map(|duration| format!("{duration:?}"))
            .unwrap_or_else(|| "unknown".to_string());
        let terminal = self.run.terminal_outcome.as_deref().unwrap_or("none");
        let last_step = self.run.last_step.as_deref().unwrap_or("none");
        let last_mmio_step = self.run.last_mmio_step.as_deref().unwrap_or("none");
        let serial_tail_hex = self.run.serial_tail_hex.as_deref().unwrap_or("none");

        writeln!(f, "guest boot diagnostics:")?;
        writeln!(f, "  classification: {}", self.run.timeout_classification())?;
        writeln!(f, "  elapsed: {elapsed}")?;
        writeln!(f, "  marker observed: {}", self.run.marker_observed)?;
        writeln!(f, "  watchdog timed out: {}", self.run.watchdog_timed_out)?;
        writeln!(
            f,
            "  stop requested after loop: {}",
            self.run.stop_requested_after_loop
        )?;
        writeln!(f, "  serial bytes captured: {}", self.run.serial_byte_count)?;
        writeln!(f, "  run-loop calls: {}", self.run.run_loop_calls)?;
        writeln!(
            f,
            "  completed run-loop steps: {}",
            self.run.completed_steps
        )?;
        writeln!(f, "  raw observed steps: {}", self.run.raw_steps)?;
        writeln!(f, "  resumable outcomes: {}", self.run.resumable_outcomes)?;
        writeln!(
            f,
            "  raw step counts: dirty_write={}, hvc={}, cpu_off={}, cpu_suspend={}, sys64={}, mmio={}, vtimer={}, canceled={}, unknown={}",
            self.run.dirty_write_steps,
            self.run.hvc_steps,
            self.run.cpu_off_steps,
            self.run.cpu_suspend_steps,
            self.run.sys64_steps,
            self.run.mmio_steps,
            self.run.virtual_timer_steps,
            self.run.canceled_steps,
            self.run.unknown_steps
        )?;
        writeln!(f, "  terminal outcome: {terminal}")?;
        writeln!(f, "  last raw step: {last_step}")?;
        writeln!(f, "  last MMIO step: {last_mmio_step}")?;
        writeln!(f, "  serial tail hex: {serial_tail_hex}")?;
        writeln!(f, "  kernel path: {}", self.boot.kernel_path.display())?;
        match self.boot.initrd_path.as_ref() {
            Some(path) => writeln!(f, "  initrd path: {}", path.display())?,
            None => writeln!(f, "  initrd path: none")?,
        }
        writeln!(f, "  boot args: {}", self.boot.boot_args)?;
        writeln!(f, "  boot PC: 0x{:x}", self.boot.boot_pc)?;
        writeln!(f, "  vCPU MPIDRs: {:?}", self.boot.vcpu_mpidrs)?;
        writeln!(
            f,
            "  FDT: address=0x{:x}, size={}",
            self.boot.fdt_address, self.boot.fdt_size
        )?;
        match (self.boot.initrd_address, self.boot.initrd_size) {
            (Some(address), Some(size)) => {
                writeln!(f, "  initrd: address=0x{address:x}, size={size}")?;
            }
            (None, None) => writeln!(f, "  initrd: none")?,
            _ => writeln!(f, "  initrd: inconsistent metadata")?,
        }
        writeln!(
            f,
            "  serial: base=0x{:x}, size={}, interrupt_line={}",
            self.boot.serial_mmio_base, self.boot.serial_mmio_size, self.boot.serial_interrupt_line
        )
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn validate_pre_run_boot_metadata(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
    diagnostics: &GuestBootDiagnostics,
) {
    use device_tree::DeviceTree;

    let resources = session.runtime_resources();
    let boot_origin = resources
        .boot_origin
        .as_ref()
        .expect("ordinary guest boot should retain boot-origin metadata");
    assert_eq!(
        boot_origin.loaded_boot_source.command_line.as_str(),
        diagnostics.boot_args,
        "guest boot test boot args should match diagnostics"
    );
    assert_eq!(
        boot_origin
            .loaded_boot_source
            .initrd
            .map(|loaded| loaded.address.raw_value()),
        diagnostics.initrd_address,
        "guest boot test loaded initrd address should match diagnostics"
    );

    let mut fdt_bytes = vec![0; boot_origin.fdt.size];
    session
        .guest_memory()
        .expect("guest boot test memory should be mapped")
        .read_slice(&mut fdt_bytes, boot_origin.fdt.address)
        .expect("guest boot test FDT bytes should read");
    let tree = DeviceTree::load(&fdt_bytes).expect("guest boot test FDT should parse");
    let chosen = tree
        .find("/chosen")
        .expect("guest boot test FDT should contain /chosen");
    assert_eq!(chosen.prop_str("bootargs").unwrap(), diagnostics.boot_args);
    match (diagnostics.initrd_address, diagnostics.initrd_size) {
        (Some(address), Some(size)) => {
            assert_eq!(chosen.prop_u64("linux,initrd-start").unwrap(), address);
            assert_eq!(chosen.prop_u64("linux,initrd-end").unwrap(), address + size);
        }
        (None, None) => {
            assert!(!chosen.has_prop("linux,initrd-start"));
            assert!(!chosen.has_prop("linux,initrd-end"));
        }
        _ => panic!("guest boot test initrd diagnostics should be internally consistent"),
    }

    let gic = session.gic_metadata();
    let intc = tree
        .find("/intc")
        .expect("guest boot test FDT should contain the GIC node");
    assert_eq!(intc.prop_str("compatible").unwrap(), "arm,gic-v3");
    assert_eq!(session.gic_msi_signaler().is_some(), gic.msi.is_some());
    match gic.msi {
        Some(msi) => {
            assert!(!intc.has_prop("msi-controller"));
            assert!(!intc.has_prop("mbi-ranges"));
            assert!(!intc.has_prop("mbi-alias"));
            assert!(!intc.has_prop("#msi-cells"));
            assert_eq!(intc.children.len(), 1);
            let frame_path = format!("/intc/v2m@{:x}", msi.region.base);
            let frame = tree
                .find(&frame_path)
                .unwrap_or_else(|| panic!("guest boot test FDT should contain {frame_path}"));
            assert_eq!(frame.prop_str("compatible").unwrap(), "arm,gic-v2m-frame");
            assert!(
                frame
                    .prop_raw("msi-controller")
                    .expect("MSI controller property should exist")
                    .is_empty()
            );
            assert_eq!(frame.prop_u32("phandle").unwrap(), 3);
            assert_eq!(
                prop_u64_cells(frame, "reg"),
                [msi.region.base, msi.region.size]
            );
            assert!(!frame.has_prop("arm,msi-base-spi"));
            assert!(!frame.has_prop("arm,msi-num-spis"));
            assert!(!frame.has_prop("#msi-cells"));
            assert!(frame.children.is_empty());
            assert!(tree.find("/intc/its").is_none());
        }
        None => {
            assert!(!intc.has_prop("msi-controller"));
            assert!(!intc.has_prop("mbi-ranges"));
            assert!(!intc.has_prop("mbi-alias"));
            assert!(!intc.has_prop("#msi-cells"));
            assert!(intc.children.is_empty());
        }
    }

    let pci_validation = resources.pci_validation.as_ref();
    let pci_data_devices = session.pci_data_device_diagnostics().is_some();
    if pci_validation.is_some() || pci_data_devices {
        if let Some(validation) = pci_validation {
            assert_eq!(
                validation
                    .segment()
                    .with_segment(|segment| segment.function_count())
                    .expect("PCI validation segment should remain accessible"),
                2,
                "PCI validation segment should retain the host bridge and endpoint"
            );
        }
        let pci = tree
            .find("/pci@70000000")
            .expect("PCI-enabled FDT should contain the ECAM host node");
        assert_eq!(pci.prop_str("compatible").unwrap(), "pci-host-ecam-generic");
        assert_eq!(pci.prop_str("device_type").unwrap(), "pci");
        assert_eq!(prop_u64_cells(pci, "reg"), [0x7000_0000, 0x10_0000]);
        assert_eq!(prop_u32_cells(pci, "bus-range"), [0, 0]);
        assert_eq!(pci.prop_u32("linux,pci-domain").unwrap(), 0);
        assert_eq!(pci.prop_u32("#address-cells").unwrap(), 3);
        assert_eq!(pci.prop_u32("#size-cells").unwrap(), 2);
        assert_eq!(pci.prop_u32("#interrupt-cells").unwrap(), 1);
        assert_eq!(pci.prop_u32("msi-parent").unwrap(), 3);
        assert_eq!(
            prop_u32_cells(pci, "ranges"),
            [
                0x0200_0000,
                0,
                0x4000_3000,
                0,
                0x4000_3000,
                0,
                0x2fff_d000,
                0x0300_0000,
                0x40,
                0,
                0x40,
                0,
                0x40,
                0,
            ]
        );
        for property in ["interrupt-map", "interrupt-map-mask", "dma-coherent"] {
            assert!(
                pci.prop_raw(property)
                    .expect("empty PCI property should exist")
                    .is_empty()
            );
        }
        assert!(!pci.has_prop("msi-map"));
        assert!(!pci.has_prop("iommu-map"));
        assert!(tree.find("/intc/its").is_none());
    } else {
        assert!(
            tree.find("/pci@70000000").is_none(),
            "ordinary guest boot should not publish PCI"
        );
    }

    assert_eq!(session.vcpu_count(), diagnostics.vcpu_mpidrs.len());
    assert_eq!(session.vcpu_mpidrs(), diagnostics.vcpu_mpidrs);
    for mpidr in &diagnostics.vcpu_mpidrs {
        let cpu_path = format!("/cpus/cpu@{mpidr:x}");
        let cpu = tree
            .find(&cpu_path)
            .unwrap_or_else(|| panic!("guest boot test FDT should contain {cpu_path}"));
        assert_eq!(cpu.prop_u64("reg").unwrap(), *mpidr);
        assert_eq!(cpu.prop_str("enable-method").unwrap(), "psci");
    }

    let serial_node_path = format!("/uart@{:x}", diagnostics.serial_mmio_base);
    let serial = tree
        .find(&serial_node_path)
        .expect("guest boot test FDT should contain serial node");
    assert_eq!(serial.prop_str("compatible").unwrap(), "ns16550a");
    assert_eq!(
        prop_u64_cells(serial, "reg"),
        [diagnostics.serial_mmio_base, diagnostics.serial_mmio_size]
    );
    assert_eq!(
        prop_u32_cells(serial, "interrupts"),
        [0, diagnostics.serial_interrupt_line - 32, 1]
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_loop_completed_steps(outcome: &bangbang_hvf::HvfArm64BootRunLoopOutcome) -> usize {
    match outcome {
        bangbang_hvf::HvfArm64BootRunLoopOutcome::StepLimitReached { steps }
        | bangbang_hvf::HvfArm64BootRunLoopOutcome::Wakeup { steps }
        | bangbang_hvf::HvfArm64BootRunLoopOutcome::Stopped { steps }
        | bangbang_hvf::HvfArm64BootRunLoopOutcome::Canceled { steps }
        | bangbang_hvf::HvfArm64BootRunLoopOutcome::GuestShutdown { steps }
        | bangbang_hvf::HvfArm64BootRunLoopOutcome::GuestReset { steps }
        | bangbang_hvf::HvfArm64BootRunLoopOutcome::Unknown { steps, .. } => *steps,
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn prop_u32_cells(node: &device_tree::Node, name: &str) -> Vec<u32> {
    let raw = node.prop_raw(name).expect("property should exist");
    assert_eq!(raw.len() % 4, 0, "{name} property should contain u32 cells");

    raw.chunks_exact(4)
        .map(|chunk| u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn prop_u64_cells(node: &device_tree::Node, name: &str) -> Vec<u64> {
    let raw = node.prop_raw(name).expect("property should exist");
    assert_eq!(raw.len() % 8, 0, "{name} property should contain u64 cells");

    raw.chunks_exact(8)
        .map(|chunk| {
            u64::from_be_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ])
        })
        .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn env_path(name: &str) -> std::path::PathBuf {
    std::env::var_os(name)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| panic!("{name} must be set by scripts/run-integration-tests.sh"))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn serial_contains_marker(
    output: &bangbang_runtime::serial::SharedSerialOutputBuffer,
    marker: &[u8],
) -> bool {
    output
        .bytes()
        .expect("guest boot test serial output should read")
        .windows(marker.len())
        .any(|window| window == marker)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn bytes_contain_marker(bytes: &[u8], marker: &[u8]) -> bool {
    bytes.windows(marker.len()).any(|window| window == marker)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn decimal_marker_value(bytes: &[u8], prefix: &[u8]) -> u64 {
    let line = bytes
        .split(|byte| matches!(byte, b'\n' | b'\r' | 0))
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| {
            panic!(
                "guest serial output did not contain decimal marker {:?}\nserial output:\n{}",
                String::from_utf8_lossy(prefix),
                String::from_utf8_lossy(bytes)
            )
        });
    let value = std::str::from_utf8(line).expect("decimal marker should be UTF-8");
    value
        .parse::<u64>()
        .expect("decimal marker should contain one u64")
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn interrupt_counts_between(
    bytes: &[u8],
    begin: &[u8],
    end: &[u8],
) -> std::collections::BTreeMap<String, u64> {
    let snapshot = bytes_between_markers(bytes, begin, end).unwrap_or_else(|| {
        panic!(
            "guest serial output did not contain marker-bounded /proc/interrupts snapshot\nserial output:\n{}",
            String::from_utf8_lossy(bytes)
        )
    });
    let snapshot = String::from_utf8_lossy(snapshot);
    snapshot
        .trim_matches(char::from(0))
        .lines()
        .filter_map(|line| {
            let (_irq, values) = line.split_once(':')?;
            let mut fields = values.split_whitespace();
            let count = fields.next()?.parse::<u64>().ok()?;
            let name = fields.last()?.to_string();
            Some((name, count))
        })
        .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn guest_cmdline_capture(observation: &GuestBootObservation) -> &[u8] {
    bytes_between_markers(
        &observation.serial_bytes,
        CMDLINE_BEGIN_MARKER,
        CMDLINE_END_MARKER,
    )
    .unwrap_or_else(|| {
        panic!(
            "guest serial output did not contain marker-bounded /proc/cmdline\nserial output:\n{}",
            String::from_utf8_lossy(&observation.serial_bytes)
        )
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn bytes_between_markers<'a>(bytes: &'a [u8], begin: &[u8], end: &[u8]) -> Option<&'a [u8]> {
    let begin_start = bytes
        .windows(begin.len())
        .position(|window| window == begin)?;
    let content_start = begin_start + begin.len();
    let content = &bytes[content_start..];
    let end_start = content
        .windows(end.len())
        .position(|window| window == end)?;

    Some(&content[..end_start])
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_guest_cmdline_contains_arg(cmdline: &[u8], expected: &[u8]) {
    assert!(
        guest_cmdline_contains_arg(cmdline, expected),
        "guest /proc/cmdline did not contain argument {:?}\ncmdline bytes:\n{}",
        String::from_utf8_lossy(expected),
        String::from_utf8_lossy(cmdline)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn guest_cmdline_contains_arg(cmdline: &[u8], expected: &[u8]) -> bool {
    cmdline
        .split(|byte| byte.is_ascii_whitespace() || *byte == 0)
        .any(|arg| arg == expected)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn serial_tail_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut tail = String::new();
    let start = bytes.len().saturating_sub(64);
    for (index, byte) in bytes[start..].iter().copied().enumerate() {
        if index > 0 {
            tail.push(' ');
        }
        write!(&mut tail, "{byte:02x}").expect("hex tail write should not fail");
    }
    tail
}

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod tests {
    use super::{
        GuestBootDiagnostics, GuestBootFailureReport, GuestBootRunDiagnostics,
        bytes_between_markers, bytes_contain_marker, guest_cmdline_contains_arg,
        run_loop_completed_steps,
    };

    #[test]
    fn guest_boot_run_diagnostics_classifies_outer_timeout_after_handled_steps() {
        let mut diagnostics = GuestBootRunDiagnostics::default();
        let outcome = bangbang_hvf::HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 };
        diagnostics.record_step(&bangbang_hvf::HvfVcpuRunStepOutcome::VtimerActivated);
        diagnostics.record_loop_outcome(&outcome);
        diagnostics.finish(
            std::time::Duration::from_secs(30),
            false,
            true,
            false,
            &[],
            None,
        );

        assert_eq!(
            diagnostics.timeout_classification(),
            "outer-timeout-after-handled-steps"
        );
        assert_eq!(diagnostics.run_loop_calls, 1);
        assert_eq!(diagnostics.resumable_outcomes, 1);
        assert_eq!(diagnostics.virtual_timer_steps, 1);
        assert_eq!(diagnostics.last_mmio_step, None);
    }

    #[test]
    fn guest_boot_run_diagnostics_classifies_watchdog_cancellation() {
        let mut diagnostics = GuestBootRunDiagnostics::default();
        let outcome = bangbang_hvf::HvfArm64BootRunLoopOutcome::Stopped { steps: 1 };
        diagnostics.record_step(&bangbang_hvf::HvfVcpuRunStepOutcome::Canceled);
        diagnostics.record_loop_outcome(&outcome);
        diagnostics.finish(
            std::time::Duration::from_secs(30),
            false,
            true,
            true,
            &[],
            Some(&outcome),
        );

        assert_eq!(
            diagnostics.timeout_classification(),
            "watchdog-canceled-in-flight-vcpu-run"
        );
        assert_eq!(
            diagnostics.terminal_outcome.as_deref(),
            Some("Stopped { steps: 1 }")
        );
    }

    #[test]
    fn guest_boot_failure_report_includes_boot_and_run_context() {
        let boot = GuestBootDiagnostics {
            kernel_path: "/tmp/vmlinux".into(),
            initrd_path: Some("/tmp/initrd.cpio".into()),
            boot_args: super::INITRD_BOOT_ARGS.to_string(),
            boot_pc: 0x8020_0000,
            fdt_address: 0x87e0_0000,
            fdt_size: 4096,
            initrd_address: Some(0x87df_f000),
            initrd_size: Some(512),
            serial_mmio_base: 0x4000_0000,
            serial_mmio_size: 4096,
            serial_interrupt_line: 32,
            vcpu_mpidrs: vec![0],
        };
        let mut run = GuestBootRunDiagnostics::default();
        run.finish(
            std::time::Duration::from_secs(30),
            false,
            true,
            false,
            &[],
            None,
        );

        let report = GuestBootFailureReport::new(&boot, &run).to_string();

        assert!(report.contains("classification: outer-timeout-without-terminal-outcome"));
        assert!(report.contains("kernel path: /tmp/vmlinux"));
        assert!(report.contains("boot args: console=ttyS0 reboot=k panic=1 rdinit=/init"));
        assert!(report.contains("serial: base=0x40000000, size=4096, interrupt_line=32"));
    }

    #[test]
    fn completed_steps_reads_all_run_loop_outcome_variants() {
        assert_eq!(
            run_loop_completed_steps(
                &bangbang_hvf::HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 7 }
            ),
            7
        );
        assert_eq!(
            run_loop_completed_steps(&bangbang_hvf::HvfArm64BootRunLoopOutcome::Wakeup {
                steps: 8
            }),
            8
        );
        assert_eq!(
            run_loop_completed_steps(&bangbang_hvf::HvfArm64BootRunLoopOutcome::Stopped {
                steps: 2
            }),
            2
        );
        assert_eq!(
            run_loop_completed_steps(&bangbang_hvf::HvfArm64BootRunLoopOutcome::Canceled {
                steps: 3
            }),
            3
        );
        assert_eq!(
            run_loop_completed_steps(&bangbang_hvf::HvfArm64BootRunLoopOutcome::GuestShutdown {
                steps: 4
            }),
            4
        );
        assert_eq!(
            run_loop_completed_steps(&bangbang_hvf::HvfArm64BootRunLoopOutcome::GuestReset {
                steps: 5
            }),
            5
        );
        assert_eq!(
            run_loop_completed_steps(&bangbang_hvf::HvfArm64BootRunLoopOutcome::Unknown {
                steps: 6,
                reason: 99
            }),
            6
        );
    }

    #[test]
    fn marker_match_accepts_tty_crlf_translation() {
        assert!(bytes_contain_marker(
            b"BANGBANG_BOOT_OK\r\n",
            super::BOOT_MARKER
        ));
    }

    #[test]
    fn bytes_between_markers_extracts_payload() {
        assert_eq!(
            bytes_between_markers(b"prefix BEGINpayloadEND suffix", b"BEGIN", b"END"),
            Some(&b"payload"[..])
        );
        assert_eq!(
            bytes_between_markers(b"prefix BEGINpayload suffix", b"BEGIN", b"END"),
            None
        );
    }

    #[test]
    fn guest_cmdline_arg_match_requires_exact_token() {
        let cmdline = b"console=ttyS0 root=/dev/vda ro\0";

        assert!(guest_cmdline_contains_arg(cmdline, b"root=/dev/vda"));
        assert!(guest_cmdline_contains_arg(cmdline, b"ro"));
        assert!(!guest_cmdline_contains_arg(cmdline, b"root=/dev"));
        assert!(!guest_cmdline_contains_arg(cmdline, b"r"));
    }
}
