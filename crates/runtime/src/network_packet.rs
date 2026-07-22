//! Bounded, backend-neutral virtio-net packet normalization.

use std::collections::TryReserveError;
use std::fmt;
use std::ops::{ControlFlow, Range};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::network::{
    VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_TSO4, VIRTIO_NET_F_HOST_TSO6, VIRTIO_NET_F_HOST_UFO,
    VIRTIO_NET_HDR_F_DATA_VALID, VIRTIO_NET_HDR_F_NEEDS_CSUM, VIRTIO_NET_HDR_F_RSC_INFO,
    VIRTIO_NET_HDR_GSO_ECN, VIRTIO_NET_HDR_GSO_NONE, VIRTIO_NET_HDR_GSO_TCPV4,
    VIRTIO_NET_HDR_GSO_TCPV6, VIRTIO_NET_HDR_GSO_UDP, VIRTIO_NET_MAX_BUFFER_SIZE,
    VIRTIO_NET_TX_HEADER_SIZE, VirtioNetworkTxHeader,
};

const ETHERNET_HEADER_LEN: usize = 14;
const ETHERNET_SOURCE_OFFSET: usize = 6;
const ETHERNET_ADDRESS_LEN: usize = 6;
const ETHERNET_ETHERTYPE_OFFSET: usize = 12;
const ETHERNET_VLAN_HEADER_LEN: usize = 4;
const ETHERNET_ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERNET_ETHERTYPE_IPV6: u16 = 0x86dd;
const ETHERNET_ETHERTYPE_VLAN: u16 = 0x8100;
const ETHERNET_ETHERTYPE_PROVIDER_VLAN: u16 = 0x88a8;
const ETHERNET_ETHERTYPE_QINQ: u16 = 0x9100;
const MAX_VLAN_HEADERS: usize = 2;

const IPV4_MIN_HEADER_LEN: usize = 20;
const IPV4_TOTAL_LENGTH_OFFSET: usize = 2;
const IPV4_IDENTIFICATION_OFFSET: usize = 4;
const IPV4_FRAGMENT_OFFSET: usize = 6;
const IPV4_PROTOCOL_OFFSET: usize = 9;
const IPV4_CHECKSUM_OFFSET: usize = 10;
const IPV4_SOURCE_OFFSET: usize = 12;
const IPV4_DESTINATION_OFFSET: usize = 16;
const IPV4_MORE_FRAGMENTS: u16 = 0x2000;
const IPV4_FRAGMENT_VALUE_MASK: u16 = 0xbfff;
const IPV4_OPTION_END: u8 = 0;
const IPV4_OPTION_NO_OPERATION: u8 = 1;
const IPV4_OPTION_COPY_FLAG: u8 = 0x80;

const IPV6_HEADER_LEN: usize = 40;
const IPV6_PAYLOAD_LENGTH_OFFSET: usize = 4;
const IPV6_NEXT_HEADER_OFFSET: usize = 6;
const IPV6_SOURCE_OFFSET: usize = 8;
const IPV6_DESTINATION_OFFSET: usize = 24;
const IPV6_FRAGMENT_HEADER_LEN: usize = 8;
const IPV6_FRAGMENT_NEXT_HEADER: u8 = 44;
const IPV6_FRAGMENT_MORE: u16 = 1;
const MAX_IPV6_EXTENSION_HEADERS: usize = 8;

const IP_PROTOCOL_TCP: u8 = 6;
const IP_PROTOCOL_UDP: u8 = 17;
const TCP_MIN_HEADER_LEN: usize = 20;
const TCP_SEQUENCE_OFFSET: usize = 4;
const TCP_DATA_OFFSET_FIELD: usize = 12;
const TCP_FLAGS_OFFSET: usize = 13;
const TCP_CHECKSUM_OFFSET: usize = 16;
const TCP_FLAG_FIN: u8 = 0x01;
const TCP_FLAG_SYN: u8 = 0x02;
const TCP_FLAG_RST: u8 = 0x04;
const TCP_FLAG_PSH: u8 = 0x08;
const TCP_FLAG_URG: u8 = 0x20;
const TCP_FLAG_ECE: u8 = 0x40;
const TCP_FLAG_CWR: u8 = 0x80;
const UDP_HEADER_LEN: usize = 8;
const UDP_LENGTH_OFFSET: usize = 4;
const UDP_CHECKSUM_OFFSET: usize = 6;
const MAX_SOFTWARE_SEGMENTS: usize = u16::MAX as usize;

static NEXT_IPV6_FRAGMENT_IDENTIFICATION: AtomicU32 = AtomicU32::new(1);

/// The immutable packet envelope selected for one host backend generation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VirtioNetworkPacketEnvelope {
    /// The host backend reads and writes ordinary Ethernet frames.
    #[default]
    RawEthernet,
    /// Every host packet is prefixed by one canonical 12-byte virtio-net header.
    DirectVirtioHeader,
}

impl VirtioNetworkPacketEnvelope {
    /// Returns the number of host-envelope bytes surrounding an Ethernet frame.
    pub const fn header_len(self) -> usize {
        match self {
            Self::RawEthernet => 0,
            Self::DirectVirtioHeader => VIRTIO_NET_TX_HEADER_SIZE as usize,
        }
    }
}

/// Contiguous packets emitted from one guest frame, plus exact packet ranges.
#[derive(Clone, PartialEq, Eq)]
pub struct VirtioNetworkEmittedPackets {
    bytes: Vec<u8>,
    ranges: Vec<Range<usize>>,
    source_mac: Option<[u8; ETHERNET_ADDRESS_LEN]>,
}

impl fmt::Debug for VirtioNetworkEmittedPackets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkEmittedPackets")
            .field("bytes", &"[REDACTED]")
            .field("bytes_len", &self.bytes.len())
            .field("ranges", &self.ranges)
            .field("source_mac", &self.source_mac)
            .finish()
    }
}

impl VirtioNetworkEmittedPackets {
    /// Returns the storage containing every emitted packet.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns one range per packet in emission order.
    pub fn ranges(&self) -> &[Range<usize>] {
        &self.ranges
    }

    /// Returns the Ethernet source address when the frame contains one.
    pub const fn source_mac(&self) -> Option<[u8; ETHERNET_ADDRESS_LEN]> {
        self.source_mac
    }

    /// Returns the number of emitted host packets.
    pub fn packet_count(&self) -> usize {
        self.ranges.len()
    }

    /// Returns whether no host packet was emitted.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

/// A validated and bounded normalization plan for one guest-owned frame.
#[derive(Clone, PartialEq, Eq)]
pub struct VirtioNetworkPacketPlan {
    packet: Vec<u8>,
    kind: VirtioNetworkPacketPlanKind,
    source_mac: Option<[u8; ETHERNET_ADDRESS_LEN]>,
}

impl fmt::Debug for VirtioNetworkPacketPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkPacketPlan")
            .field("packet", &"[REDACTED]")
            .field("packet_len", &self.packet.len())
            .field("kind", &self.kind)
            .field("source_mac", &self.source_mac)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VirtioNetworkPacketPlanKind {
    Single {
        checksum: Option<GenericChecksumPlan>,
    },
    TcpSegmentation(TcpSegmentationPlan),
    UdpFragmentation(UdpFragmentationPlan),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GenericChecksumPlan {
    start: usize,
    field_offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TcpSegmentationPlan {
    ip: IpPacketLayout,
    payload_offset: usize,
    segment_payload_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UdpFragmentationPlan {
    ip: IpPacketLayout,
    fragment_payload_len: usize,
    ipv6_fragment_identification: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IpPacketLayout {
    version: IpVersion,
    ip_offset: usize,
    transport_offset: usize,
    protocol: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpVersion {
    V4 { header_len: usize },
    V6,
}

impl VirtioNetworkPacketPlan {
    /// Validates a virtio header and creates one bounded software plan.
    pub fn prepare(
        header: VirtioNetworkTxHeader,
        negotiated_features: u64,
        packet: Vec<u8>,
    ) -> Result<Self, VirtioNetworkPacketPlanError> {
        validate_packet_bound(packet.len())?;
        if packet.is_empty() {
            return Err(VirtioNetworkPacketPlanError::EmptyPacket);
        }
        if header.num_buffers() != 0 {
            return Err(VirtioNetworkPacketPlanError::TransmitNumBuffers {
                num_buffers: header.num_buffers(),
            });
        }
        let forbidden_flags =
            header.flags() & (VIRTIO_NET_HDR_F_DATA_VALID | VIRTIO_NET_HDR_F_RSC_INFO);
        if forbidden_flags != 0 {
            return Err(VirtioNetworkPacketPlanError::GuestForbiddenFlags {
                flags: forbidden_flags,
            });
        }

        let needs_checksum = header.flags() & VIRTIO_NET_HDR_F_NEEDS_CSUM != 0;
        let raw_gso_type = header.gso_type();
        let gso_ecn = raw_gso_type & VIRTIO_NET_HDR_GSO_ECN != 0;
        let gso_type = raw_gso_type & !VIRTIO_NET_HDR_GSO_ECN;
        let kind = match gso_type {
            VIRTIO_NET_HDR_GSO_NONE => {
                if gso_ecn {
                    return Err(VirtioNetworkPacketPlanError::UnsupportedGsoEcn);
                }
                if header.gso_size() != 0 {
                    return Err(VirtioNetworkPacketPlanError::UnexpectedGsoSize {
                        gso_size: header.gso_size(),
                    });
                }
                let checksum = if needs_checksum {
                    require_feature(negotiated_features, VIRTIO_NET_F_CSUM)?;
                    Some(prepare_checksum_request(&packet, header)?)
                } else if header.checksum_start() != 0 || header.checksum_offset() != 0 {
                    return Err(VirtioNetworkPacketPlanError::UnexpectedChecksumFields);
                } else {
                    None
                };
                VirtioNetworkPacketPlanKind::Single { checksum }
            }
            VIRTIO_NET_HDR_GSO_TCPV4 | VIRTIO_NET_HDR_GSO_TCPV6 => {
                if gso_ecn {
                    return Err(VirtioNetworkPacketPlanError::UnsupportedGsoEcn);
                }
                if !needs_checksum {
                    return Err(VirtioNetworkPacketPlanError::GsoWithoutChecksum);
                }
                require_feature(negotiated_features, VIRTIO_NET_F_CSUM)?;
                let required_feature = if gso_type == VIRTIO_NET_HDR_GSO_TCPV4 {
                    VIRTIO_NET_F_HOST_TSO4
                } else {
                    VIRTIO_NET_F_HOST_TSO6
                };
                require_feature(negotiated_features, required_feature)?;
                VirtioNetworkPacketPlanKind::TcpSegmentation(prepare_tcp_segmentation(
                    &packet, header, gso_type,
                )?)
            }
            VIRTIO_NET_HDR_GSO_UDP => {
                if gso_ecn {
                    return Err(VirtioNetworkPacketPlanError::UnsupportedGsoEcn);
                }
                if !needs_checksum {
                    return Err(VirtioNetworkPacketPlanError::GsoWithoutChecksum);
                }
                require_feature(negotiated_features, VIRTIO_NET_F_CSUM)?;
                require_feature(negotiated_features, VIRTIO_NET_F_HOST_UFO)?;
                VirtioNetworkPacketPlanKind::UdpFragmentation(prepare_udp_fragmentation(
                    &packet, header,
                )?)
            }
            _ => {
                return Err(VirtioNetworkPacketPlanError::UnsupportedGsoType {
                    gso_type: raw_gso_type,
                });
            }
        };
        let source_mac = ethernet_source_mac(&packet);
        Ok(Self {
            packet,
            kind,
            source_mac,
        })
    }

    /// Materializes normalized packets in bounded contiguous storage.
    pub fn emit(
        &self,
        envelope: VirtioNetworkPacketEnvelope,
    ) -> Result<VirtioNetworkEmittedPackets, VirtioNetworkPacketPlanError> {
        let packet_count = self.packet_count()?;
        let estimated_len = self.estimated_emitted_len(packet_count, envelope)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(estimated_len).map_err(|source| {
            VirtioNetworkPacketPlanError::PacketStorageAllocation {
                len: estimated_len,
                source,
            }
        })?;
        let mut ranges = Vec::new();
        ranges.try_reserve_exact(packet_count).map_err(|source| {
            VirtioNetworkPacketPlanError::PacketRangesAllocation {
                packet_count,
                source,
            }
        })?;

        let visit = self.visit_packets(envelope, |packet| {
            match append_packet(
                &mut bytes,
                &mut ranges,
                VirtioNetworkPacketEnvelope::RawEthernet,
                packet,
            ) {
                Ok(()) => ControlFlow::Continue(()),
                Err(source) => ControlFlow::Break(source),
            }
        })?;
        if let ControlFlow::Break(source) = visit {
            return Err(source);
        }

        Ok(VirtioNetworkEmittedPackets {
            bytes,
            ranges,
            source_mac: self.source_mac,
        })
    }

    /// Visits normalized packets one at a time without retaining an expanded
    /// segment vector. The callback may stop generation with its own value.
    pub fn visit_packets<B>(
        &self,
        envelope: VirtioNetworkPacketEnvelope,
        mut visitor: impl FnMut(&[u8]) -> ControlFlow<B>,
    ) -> Result<ControlFlow<B>, VirtioNetworkPacketPlanError> {
        match self.kind {
            VirtioNetworkPacketPlanKind::Single { checksum } => {
                if let Some(checksum) = checksum {
                    let mut packet = try_copy_packet(&self.packet)?;
                    complete_generic_checksum(&mut packet, checksum)?;
                    visit_packet(envelope, &packet, &mut visitor)
                } else {
                    visit_packet(envelope, &self.packet, &mut visitor)
                }
            }
            VirtioNetworkPacketPlanKind::TcpSegmentation(plan) => {
                visit_tcp_segments(&self.packet, plan, envelope, &mut visitor)
            }
            VirtioNetworkPacketPlanKind::UdpFragmentation(plan) => {
                visit_udp_fragments(&self.packet, plan, envelope, &mut visitor)
            }
        }
    }

    /// Returns the exact number of normalized host packets this plan emits.
    pub fn emitted_packet_count(&self) -> Result<usize, VirtioNetworkPacketPlanError> {
        self.packet_count()
    }

    /// Returns the exact aggregate byte count for the selected host envelope.
    pub fn emitted_len(
        &self,
        envelope: VirtioNetworkPacketEnvelope,
    ) -> Result<usize, VirtioNetworkPacketPlanError> {
        let packet_count = self.packet_count()?;
        self.estimated_emitted_len(packet_count, envelope)
    }

    /// Fallibly copies the bounded owned source packet and immutable plan.
    pub fn try_clone_owned(&self) -> Result<Self, VirtioNetworkPacketPlanError> {
        Ok(Self {
            packet: try_copy_packet(&self.packet)?,
            kind: self.kind.clone(),
            source_mac: self.source_mac,
        })
    }

    /// Returns the observed Ethernet source address, when present.
    pub const fn source_mac(&self) -> Option<[u8; ETHERNET_ADDRESS_LEN]> {
        self.source_mac
    }

    fn packet_count(&self) -> Result<usize, VirtioNetworkPacketPlanError> {
        let packet_count = match self.kind {
            VirtioNetworkPacketPlanKind::Single { .. } => 1,
            VirtioNetworkPacketPlanKind::TcpSegmentation(plan) => {
                let payload_len = self.packet.len().saturating_sub(plan.payload_offset);
                payload_len.max(1).div_ceil(plan.segment_payload_len)
            }
            VirtioNetworkPacketPlanKind::UdpFragmentation(plan) => {
                let datagram_len = self.packet.len().saturating_sub(plan.ip.transport_offset);
                datagram_len.max(1).div_ceil(plan.fragment_payload_len)
            }
        };
        if packet_count == 0 || packet_count > MAX_SOFTWARE_SEGMENTS {
            return Err(VirtioNetworkPacketPlanError::TooManyPackets { packet_count });
        }
        Ok(packet_count)
    }

    fn estimated_emitted_len(
        &self,
        packet_count: usize,
        envelope: VirtioNetworkPacketEnvelope,
    ) -> Result<usize, VirtioNetworkPacketPlanError> {
        let packet_bytes = match self.kind {
            VirtioNetworkPacketPlanKind::Single { .. } => Some(self.packet.len()),
            VirtioNetworkPacketPlanKind::TcpSegmentation(plan) => plan
                .payload_offset
                .checked_mul(packet_count.saturating_sub(1))
                .and_then(|repeated_headers| self.packet.len().checked_add(repeated_headers)),
            VirtioNetworkPacketPlanKind::UdpFragmentation(plan) => {
                let datagram_len = self.packet.len().checked_sub(plan.ip.transport_offset);
                if packet_count == 1 {
                    Some(self.packet.len())
                } else {
                    let per_fragment_header = match plan.ip.version {
                        IpVersion::V4 { header_len } => plan.ip.ip_offset + header_len,
                        IpVersion::V6 => {
                            plan.ip.ip_offset + IPV6_HEADER_LEN + IPV6_FRAGMENT_HEADER_LEN
                        }
                    };
                    datagram_len.and_then(|datagram_len| {
                        per_fragment_header
                            .checked_mul(packet_count)
                            .and_then(|headers| datagram_len.checked_add(headers))
                    })
                }
            }
        };
        packet_bytes
            .and_then(|len| {
                envelope
                    .header_len()
                    .checked_mul(packet_count)
                    .and_then(|headers| len.checked_add(headers))
            })
            .ok_or(VirtioNetworkPacketPlanError::EmittedLengthOverflow)
    }
}

fn try_copy_packet(packet: &[u8]) -> Result<Vec<u8>, VirtioNetworkPacketPlanError> {
    try_copy_packet_with_extra_capacity(packet, 0)
}

fn try_copy_packet_with_extra_capacity(
    packet: &[u8],
    extra_capacity: usize,
) -> Result<Vec<u8>, VirtioNetworkPacketPlanError> {
    let len = packet
        .len()
        .checked_add(extra_capacity)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let mut copy = Vec::new();
    copy.try_reserve_exact(len)
        .map_err(|source| VirtioNetworkPacketPlanError::PacketStorageAllocation { len, source })?;
    copy.extend_from_slice(packet);
    Ok(copy)
}

fn visit_packet<B>(
    envelope: VirtioNetworkPacketEnvelope,
    packet: &[u8],
    visitor: &mut impl FnMut(&[u8]) -> ControlFlow<B>,
) -> Result<ControlFlow<B>, VirtioNetworkPacketPlanError> {
    if envelope == VirtioNetworkPacketEnvelope::RawEthernet {
        return Ok(visitor(packet));
    }
    let len = packet
        .len()
        .checked_add(VIRTIO_NET_TX_HEADER_SIZE as usize)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let mut enveloped = Vec::new();
    enveloped
        .try_reserve_exact(len)
        .map_err(|source| VirtioNetworkPacketPlanError::PacketStorageAllocation { len, source })?;
    enveloped.resize(VIRTIO_NET_TX_HEADER_SIZE as usize, 0);
    enveloped.extend_from_slice(packet);
    Ok(visitor(&enveloped))
}

/// Packet validation or bounded software-normalization failure.
#[derive(Debug)]
pub enum VirtioNetworkPacketPlanError {
    EmptyPacket,
    FrameTooLarge {
        len: usize,
        max: u64,
    },
    TransmitNumBuffers {
        num_buffers: u16,
    },
    GuestForbiddenFlags {
        flags: u8,
    },
    UnsupportedGsoType {
        gso_type: u8,
    },
    UnsupportedGsoEcn,
    UnexpectedGsoSize {
        gso_size: u16,
    },
    UnexpectedChecksumFields,
    GsoWithoutChecksum,
    FeatureNotNegotiated {
        feature: u32,
    },
    ChecksumRange,
    MalformedEthernet,
    UnsupportedNetworkProtocol,
    MalformedIpv4,
    MalformedIpv6,
    FragmentedInput,
    UnsupportedIpv6Extension {
        next_header: u8,
    },
    TooManyIpv6Extensions,
    TransportProtocolMismatch {
        expected: u8,
        actual: u8,
    },
    ChecksumLayoutMismatch,
    MalformedTcp,
    TcpControlSegment,
    MalformedUdp,
    GsoSizeTooSmall {
        gso_size: u16,
    },
    UfoGsoSizeUnaligned {
        gso_size: u16,
    },
    TooManyPackets {
        packet_count: usize,
    },
    PacketLengthOverflow,
    EmittedLengthOverflow,
    PacketStorageAllocation {
        len: usize,
        source: TryReserveError,
    },
    PacketRangesAllocation {
        packet_count: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtioNetworkPacketPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPacket => formatter.write_str("virtio-net TX packet is empty"),
            Self::FrameTooLarge { len, max } => {
                write!(formatter, "virtio-net TX packet length {len} exceeds {max}")
            }
            Self::TransmitNumBuffers { num_buffers } => write!(
                formatter,
                "virtio-net TX num_buffers must be zero, got {num_buffers}"
            ),
            Self::GuestForbiddenFlags { flags } => write!(
                formatter,
                "virtio-net TX header contains guest-forbidden flags 0x{flags:02x}"
            ),
            Self::UnsupportedGsoType { gso_type } => {
                write!(
                    formatter,
                    "unsupported virtio-net GSO type 0x{gso_type:02x}"
                )
            }
            Self::UnsupportedGsoEcn => {
                formatter.write_str("virtio-net ECN segmentation was not negotiated")
            }
            Self::UnexpectedGsoSize { gso_size } => write!(
                formatter,
                "virtio-net non-GSO packet has nonzero GSO size {gso_size}"
            ),
            Self::UnexpectedChecksumFields => formatter
                .write_str("virtio-net packet without checksum offload has checksum fields"),
            Self::GsoWithoutChecksum => {
                formatter.write_str("virtio-net GSO packet does not request checksum completion")
            }
            Self::FeatureNotNegotiated { feature } => {
                write!(
                    formatter,
                    "virtio-net feature bit {feature} was not negotiated"
                )
            }
            Self::ChecksumRange => {
                formatter.write_str("virtio-net checksum range is outside the packet")
            }
            Self::MalformedEthernet => formatter.write_str("malformed Ethernet packet"),
            Self::UnsupportedNetworkProtocol => {
                formatter.write_str("checksum/GSO packet is not IPv4 or IPv6")
            }
            Self::MalformedIpv4 => formatter.write_str("malformed IPv4 packet"),
            Self::MalformedIpv6 => formatter.write_str("malformed IPv6 packet"),
            Self::FragmentedInput => formatter.write_str("offloaded input packet is fragmented"),
            Self::UnsupportedIpv6Extension { next_header } => write!(
                formatter,
                "unsupported IPv6 extension or transport header {next_header}"
            ),
            Self::TooManyIpv6Extensions => {
                formatter.write_str("IPv6 packet has too many extension headers")
            }
            Self::TransportProtocolMismatch { expected, actual } => write!(
                formatter,
                "virtio-net packet protocol {actual} does not match required protocol {expected}"
            ),
            Self::ChecksumLayoutMismatch => {
                formatter.write_str("virtio-net checksum offsets do not match the packet")
            }
            Self::MalformedTcp => formatter.write_str("malformed TCP packet"),
            Self::TcpControlSegment => {
                formatter.write_str("TCP control segment cannot be software-segmented")
            }
            Self::MalformedUdp => formatter.write_str("malformed UDP packet"),
            Self::GsoSizeTooSmall { gso_size } => {
                write!(formatter, "virtio-net GSO size {gso_size} is too small")
            }
            Self::UfoGsoSizeUnaligned { gso_size } => write!(
                formatter,
                "virtio-net UFO GSO size {gso_size} is not aligned to eight bytes"
            ),
            Self::TooManyPackets { packet_count } => write!(
                formatter,
                "virtio-net software normalization would emit {packet_count} packets"
            ),
            Self::PacketLengthOverflow => {
                formatter.write_str("normalized network packet length overflowed")
            }
            Self::EmittedLengthOverflow => {
                formatter.write_str("aggregate normalized packet length overflowed")
            }
            Self::PacketStorageAllocation { len, source } => write!(
                formatter,
                "failed to reserve {len} bytes for normalized packets: {source}"
            ),
            Self::PacketRangesAllocation {
                packet_count,
                source,
            } => write!(
                formatter,
                "failed to reserve metadata for {packet_count} normalized packets: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioNetworkPacketPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PacketStorageAllocation { source, .. }
            | Self::PacketRangesAllocation { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn validate_packet_bound(len: usize) -> Result<(), VirtioNetworkPacketPlanError> {
    let max_payload = VIRTIO_NET_MAX_BUFFER_SIZE
        .checked_sub(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
        .ok_or(VirtioNetworkPacketPlanError::FrameTooLarge {
            len,
            max: VIRTIO_NET_MAX_BUFFER_SIZE,
        })?;
    if u64::try_from(len).map_or(true, |len| len > max_payload) {
        return Err(VirtioNetworkPacketPlanError::FrameTooLarge {
            len,
            max: max_payload,
        });
    }
    Ok(())
}

fn require_feature(features: u64, feature: u32) -> Result<(), VirtioNetworkPacketPlanError> {
    let enabled = 1_u64
        .checked_shl(feature)
        .is_some_and(|mask| features & mask != 0);
    if enabled {
        Ok(())
    } else {
        Err(VirtioNetworkPacketPlanError::FeatureNotNegotiated { feature })
    }
}

fn prepare_tcp_segmentation(
    packet: &[u8],
    header: VirtioNetworkTxHeader,
    gso_type: u8,
) -> Result<TcpSegmentationPlan, VirtioNetworkPacketPlanError> {
    let segment_payload_len = usize::from(header.gso_size());
    if segment_payload_len == 0 {
        return Err(VirtioNetworkPacketPlanError::GsoSizeTooSmall {
            gso_size: header.gso_size(),
        });
    }
    let ip = parse_ip_packet(packet)?;
    let expected_version = if gso_type == VIRTIO_NET_HDR_GSO_TCPV4 {
        IpVersionDiscriminant::V4
    } else {
        IpVersionDiscriminant::V6
    };
    if ip.version.discriminant() != expected_version {
        return Err(VirtioNetworkPacketPlanError::TransportProtocolMismatch {
            expected: if expected_version == IpVersionDiscriminant::V4 {
                4
            } else {
                6
            },
            actual: if ip.version.discriminant() == IpVersionDiscriminant::V4 {
                4
            } else {
                6
            },
        });
    }
    require_transport(ip, IP_PROTOCOL_TCP)?;
    validate_checksum_layout(header, ip.transport_offset, TCP_CHECKSUM_OFFSET)?;
    let tcp_header_len = tcp_header_len(packet, ip.transport_offset)?;
    let payload_offset = ip
        .transport_offset
        .checked_add(tcp_header_len)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    if payload_offset > packet.len() {
        return Err(VirtioNetworkPacketPlanError::MalformedTcp);
    }
    let flags = packet_byte(packet, ip.transport_offset + TCP_FLAGS_OFFSET)
        .ok_or(VirtioNetworkPacketPlanError::MalformedTcp)?;
    if flags & (TCP_FLAG_SYN | TCP_FLAG_RST | TCP_FLAG_URG | TCP_FLAG_ECE | TCP_FLAG_CWR) != 0 {
        return Err(VirtioNetworkPacketPlanError::TcpControlSegment);
    }
    Ok(TcpSegmentationPlan {
        ip,
        payload_offset,
        segment_payload_len,
    })
}

fn prepare_udp_fragmentation(
    packet: &[u8],
    header: VirtioNetworkTxHeader,
) -> Result<UdpFragmentationPlan, VirtioNetworkPacketPlanError> {
    let requested = usize::from(header.gso_size());
    if requested < 8 {
        return Err(VirtioNetworkPacketPlanError::GsoSizeTooSmall {
            gso_size: header.gso_size(),
        });
    }
    if !requested.is_multiple_of(8) {
        return Err(VirtioNetworkPacketPlanError::UfoGsoSizeUnaligned {
            gso_size: header.gso_size(),
        });
    }
    let fragment_payload_len = requested;
    let ip = parse_ip_packet(packet)?;
    require_transport(ip, IP_PROTOCOL_UDP)?;
    validate_checksum_layout(header, ip.transport_offset, UDP_CHECKSUM_OFFSET)?;
    validate_udp(packet, ip.transport_offset)?;
    if matches!(ip.version, IpVersion::V6) && ip.transport_offset != ip.ip_offset + IPV6_HEADER_LEN
    {
        return Err(VirtioNetworkPacketPlanError::UnsupportedIpv6Extension {
            next_header: ip.protocol,
        });
    }
    Ok(UdpFragmentationPlan {
        ip,
        fragment_payload_len,
        ipv6_fragment_identification: match ip.version {
            IpVersion::V4 { .. } => 0,
            IpVersion::V6 => NEXT_IPV6_FRAGMENT_IDENTIFICATION.fetch_add(1, Ordering::Relaxed),
        },
    })
}

fn validate_ipv4_options(
    packet: &[u8],
    ip_offset: usize,
    header_len: usize,
) -> Result<(), VirtioNetworkPacketPlanError> {
    let options_start = ip_offset
        .checked_add(IPV4_MIN_HEADER_LEN)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let options_end = ip_offset
        .checked_add(header_len)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let options = packet
        .get(options_start..options_end)
        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
    let mut cursor = 0;
    while cursor < options.len() {
        match options
            .get(cursor)
            .copied()
            .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?
        {
            IPV4_OPTION_END => {
                return if options
                    .get(cursor..)
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?
                    .iter()
                    .all(|byte| *byte == IPV4_OPTION_END)
                {
                    Ok(())
                } else {
                    Err(VirtioNetworkPacketPlanError::MalformedIpv4)
                };
            }
            IPV4_OPTION_NO_OPERATION => cursor += 1,
            _ => {
                let option_len = options
                    .get(cursor + 1)
                    .copied()
                    .map(usize::from)
                    .filter(|option_len| *option_len >= 2)
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
                cursor = cursor
                    .checked_add(option_len)
                    .filter(|end| *end <= options.len())
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
            }
        }
    }
    Ok(())
}

fn neutralize_noncopied_ipv4_fragment_options(
    packet: &mut [u8],
    ip_offset: usize,
    header_len: usize,
) -> Result<(), VirtioNetworkPacketPlanError> {
    let options_start = ip_offset
        .checked_add(IPV4_MIN_HEADER_LEN)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let options_end = ip_offset
        .checked_add(header_len)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let options = packet
        .get_mut(options_start..options_end)
        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
    let mut cursor = 0;
    while cursor < options.len() {
        let option_type = options
            .get(cursor)
            .copied()
            .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
        match option_type {
            IPV4_OPTION_END => return Ok(()),
            IPV4_OPTION_NO_OPERATION => cursor += 1,
            _ => {
                let option_len = options
                    .get(cursor + 1)
                    .copied()
                    .map(usize::from)
                    .filter(|option_len| *option_len >= 2)
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
                let option_end = cursor
                    .checked_add(option_len)
                    .filter(|end| *end <= options.len())
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
                if option_type & IPV4_OPTION_COPY_FLAG == 0 {
                    options
                        .get_mut(cursor..option_end)
                        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?
                        .fill(IPV4_OPTION_NO_OPERATION);
                }
                cursor = option_end;
            }
        }
    }
    Ok(())
}

fn prepare_checksum_request(
    packet: &[u8],
    header: VirtioNetworkTxHeader,
) -> Result<GenericChecksumPlan, VirtioNetworkPacketPlanError> {
    let checksum_start = usize::from(header.checksum_start());
    let checksum_offset = usize::from(header.checksum_offset());
    let field_offset = checksum_start
        .checked_add(checksum_offset)
        .ok_or(VirtioNetworkPacketPlanError::ChecksumRange)?;
    if field_offset
        .checked_add(2)
        .is_none_or(|end| end > packet.len())
    {
        return Err(VirtioNetworkPacketPlanError::ChecksumRange);
    }
    Ok(GenericChecksumPlan {
        start: checksum_start,
        field_offset,
    })
}

fn validate_checksum_layout(
    header: VirtioNetworkTxHeader,
    transport_offset: usize,
    checksum_offset: usize,
) -> Result<(), VirtioNetworkPacketPlanError> {
    if usize::from(header.checksum_start()) != transport_offset
        || usize::from(header.checksum_offset()) != checksum_offset
    {
        return Err(VirtioNetworkPacketPlanError::ChecksumLayoutMismatch);
    }
    Ok(())
}

fn parse_ip_packet(packet: &[u8]) -> Result<IpPacketLayout, VirtioNetworkPacketPlanError> {
    let (ethertype, ip_offset) = parse_ethernet(packet)?;
    match ethertype {
        ETHERNET_ETHERTYPE_IPV4 => parse_ipv4(packet, ip_offset),
        ETHERNET_ETHERTYPE_IPV6 => parse_ipv6(packet, ip_offset),
        _ => Err(VirtioNetworkPacketPlanError::UnsupportedNetworkProtocol),
    }
}

fn parse_ethernet(packet: &[u8]) -> Result<(u16, usize), VirtioNetworkPacketPlanError> {
    if packet.len() < ETHERNET_HEADER_LEN {
        return Err(VirtioNetworkPacketPlanError::MalformedEthernet);
    }
    let mut ethertype = read_u16_be(packet, ETHERNET_ETHERTYPE_OFFSET)
        .ok_or(VirtioNetworkPacketPlanError::MalformedEthernet)?;
    let mut ip_offset = ETHERNET_HEADER_LEN;
    for _ in 0..MAX_VLAN_HEADERS {
        if !matches!(
            ethertype,
            ETHERNET_ETHERTYPE_VLAN | ETHERNET_ETHERTYPE_PROVIDER_VLAN | ETHERNET_ETHERTYPE_QINQ
        ) {
            break;
        }
        ethertype = read_u16_be(packet, ip_offset + 2)
            .ok_or(VirtioNetworkPacketPlanError::MalformedEthernet)?;
        ip_offset = ip_offset
            .checked_add(ETHERNET_VLAN_HEADER_LEN)
            .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    }
    if matches!(
        ethertype,
        ETHERNET_ETHERTYPE_VLAN | ETHERNET_ETHERTYPE_PROVIDER_VLAN | ETHERNET_ETHERTYPE_QINQ
    ) {
        return Err(VirtioNetworkPacketPlanError::MalformedEthernet);
    }
    Ok((ethertype, ip_offset))
}

fn parse_ipv4(
    packet: &[u8],
    ip_offset: usize,
) -> Result<IpPacketLayout, VirtioNetworkPacketPlanError> {
    let version_ihl =
        packet_byte(packet, ip_offset).ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
    if version_ihl >> 4 != 4 {
        return Err(VirtioNetworkPacketPlanError::MalformedIpv4);
    }
    let header_len = usize::from(version_ihl & 0x0f) * 4;
    if header_len < IPV4_MIN_HEADER_LEN {
        return Err(VirtioNetworkPacketPlanError::MalformedIpv4);
    }
    let ip_end = ip_offset
        .checked_add(header_len)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    if ip_end > packet.len() {
        return Err(VirtioNetworkPacketPlanError::MalformedIpv4);
    }
    validate_ipv4_options(packet, ip_offset, header_len)?;
    let total_len = usize::from(
        read_u16_be(packet, ip_offset + IPV4_TOTAL_LENGTH_OFFSET)
            .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?,
    );
    if total_len < header_len || ip_offset.checked_add(total_len) != Some(packet.len()) {
        return Err(VirtioNetworkPacketPlanError::MalformedIpv4);
    }
    let fragment = read_u16_be(packet, ip_offset + IPV4_FRAGMENT_OFFSET)
        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
    if fragment & IPV4_FRAGMENT_VALUE_MASK != 0 {
        return Err(VirtioNetworkPacketPlanError::FragmentedInput);
    }
    let protocol = packet_byte(packet, ip_offset + IPV4_PROTOCOL_OFFSET)
        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
    Ok(IpPacketLayout {
        version: IpVersion::V4 { header_len },
        ip_offset,
        transport_offset: ip_end,
        protocol,
    })
}

fn parse_ipv6(
    packet: &[u8],
    ip_offset: usize,
) -> Result<IpPacketLayout, VirtioNetworkPacketPlanError> {
    let version =
        packet_byte(packet, ip_offset).ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)? >> 4;
    if version != 6 {
        return Err(VirtioNetworkPacketPlanError::MalformedIpv6);
    }
    let payload_len = usize::from(
        read_u16_be(packet, ip_offset + IPV6_PAYLOAD_LENGTH_OFFSET)
            .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?,
    );
    if ip_offset
        .checked_add(IPV6_HEADER_LEN)
        .and_then(|len| len.checked_add(payload_len))
        != Some(packet.len())
    {
        return Err(VirtioNetworkPacketPlanError::MalformedIpv6);
    }
    let mut next_header = packet_byte(packet, ip_offset + IPV6_NEXT_HEADER_OFFSET)
        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?;
    let mut cursor = ip_offset
        .checked_add(IPV6_HEADER_LEN)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    for extension_index in 0..=MAX_IPV6_EXTENSION_HEADERS {
        match next_header {
            IP_PROTOCOL_TCP | IP_PROTOCOL_UDP => {
                return Ok(IpPacketLayout {
                    version: IpVersion::V6,
                    ip_offset,
                    transport_offset: cursor,
                    protocol: next_header,
                });
            }
            0 | 43 | 60 => {
                if extension_index == MAX_IPV6_EXTENSION_HEADERS {
                    return Err(VirtioNetworkPacketPlanError::TooManyIpv6Extensions);
                }
                let following = packet_byte(packet, cursor)
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?;
                let units = usize::from(
                    packet_byte(packet, cursor + 1)
                        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?,
                );
                let extension_len = units
                    .checked_add(1)
                    .and_then(|units| units.checked_mul(8))
                    .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
                cursor = checked_advance(packet, cursor, extension_len)
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?;
                next_header = following;
            }
            IPV6_FRAGMENT_NEXT_HEADER => {
                return Err(VirtioNetworkPacketPlanError::FragmentedInput);
            }
            51 => {
                if extension_index == MAX_IPV6_EXTENSION_HEADERS {
                    return Err(VirtioNetworkPacketPlanError::TooManyIpv6Extensions);
                }
                let following = packet_byte(packet, cursor)
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?;
                let units = usize::from(
                    packet_byte(packet, cursor + 1)
                        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?,
                );
                let extension_len = units
                    .checked_add(2)
                    .and_then(|units| units.checked_mul(4))
                    .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
                cursor = checked_advance(packet, cursor, extension_len)
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?;
                next_header = following;
            }
            _ => {
                return Err(VirtioNetworkPacketPlanError::UnsupportedIpv6Extension { next_header });
            }
        }
    }
    Err(VirtioNetworkPacketPlanError::TooManyIpv6Extensions)
}

fn checked_advance(packet: &[u8], offset: usize, len: usize) -> Option<usize> {
    offset.checked_add(len).filter(|end| *end <= packet.len())
}

fn require_transport(ip: IpPacketLayout, expected: u8) -> Result<(), VirtioNetworkPacketPlanError> {
    if ip.protocol == expected {
        Ok(())
    } else {
        Err(VirtioNetworkPacketPlanError::TransportProtocolMismatch {
            expected,
            actual: ip.protocol,
        })
    }
}

fn tcp_header_len(
    packet: &[u8],
    transport_offset: usize,
) -> Result<usize, VirtioNetworkPacketPlanError> {
    let data_offset = packet_byte(packet, transport_offset + TCP_DATA_OFFSET_FIELD)
        .ok_or(VirtioNetworkPacketPlanError::MalformedTcp)?
        >> 4;
    let header_len = usize::from(data_offset) * 4;
    if header_len < TCP_MIN_HEADER_LEN
        || transport_offset
            .checked_add(header_len)
            .is_none_or(|end| end > packet.len())
    {
        return Err(VirtioNetworkPacketPlanError::MalformedTcp);
    }
    Ok(header_len)
}

fn validate_udp(
    packet: &[u8],
    transport_offset: usize,
) -> Result<(), VirtioNetworkPacketPlanError> {
    let expected_len = packet.len().saturating_sub(transport_offset);
    let udp_len = usize::from(
        read_u16_be(packet, transport_offset + UDP_LENGTH_OFFSET)
            .ok_or(VirtioNetworkPacketPlanError::MalformedUdp)?,
    );
    if expected_len < UDP_HEADER_LEN || udp_len != expected_len {
        return Err(VirtioNetworkPacketPlanError::MalformedUdp);
    }
    Ok(())
}

fn complete_generic_checksum(
    packet: &mut [u8],
    plan: GenericChecksumPlan,
) -> Result<(), VirtioNetworkPacketPlanError> {
    let bytes = packet
        .get(plan.start..)
        .ok_or(VirtioNetworkPacketPlanError::ChecksumRange)?;
    let checksum = checksum_finish(checksum_add(0, bytes));
    write_u16_be(
        packet,
        plan.field_offset,
        if checksum == 0 { u16::MAX } else { checksum },
    )
}

fn write_transport_checksum(
    packet: &mut [u8],
    ip: IpPacketLayout,
    checksum_offset: usize,
    map_zero_to_all_ones: bool,
) -> Result<(), VirtioNetworkPacketPlanError> {
    let checksum_field = ip
        .transport_offset
        .checked_add(checksum_offset)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    write_u16_be(packet, checksum_field, 0)?;
    let transport = packet
        .get(ip.transport_offset..)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let mut sum = pseudo_header_sum(packet, ip, transport.len())?;
    sum = checksum_add(sum, transport);
    let mut checksum = checksum_finish(sum);
    if checksum == 0 && map_zero_to_all_ones {
        checksum = u16::MAX;
    }
    write_u16_be(packet, checksum_field, checksum)
}

fn pseudo_header_sum(
    packet: &[u8],
    ip: IpPacketLayout,
    transport_len: usize,
) -> Result<u64, VirtioNetworkPacketPlanError> {
    match ip.version {
        IpVersion::V4 { .. } => {
            let source = packet
                .get(ip.ip_offset + IPV4_SOURCE_OFFSET..ip.ip_offset + IPV4_SOURCE_OFFSET + 4)
                .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
            let destination = packet
                .get(
                    ip.ip_offset + IPV4_DESTINATION_OFFSET
                        ..ip.ip_offset + IPV4_DESTINATION_OFFSET + 4,
                )
                .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
            let transport_len = u16::try_from(transport_len)
                .map_err(|_| VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
            Ok(checksum_add(checksum_add(0, source), destination)
                + u64::from(ip.protocol)
                + u64::from(transport_len))
        }
        IpVersion::V6 => {
            let source = packet
                .get(ip.ip_offset + IPV6_SOURCE_OFFSET..ip.ip_offset + IPV6_SOURCE_OFFSET + 16)
                .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?;
            let destination = packet
                .get(
                    ip.ip_offset + IPV6_DESTINATION_OFFSET
                        ..ip.ip_offset + IPV6_DESTINATION_OFFSET + 16,
                )
                .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?;
            let transport_len = u32::try_from(transport_len)
                .map_err(|_| VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
            let length_bytes = transport_len.to_be_bytes();
            Ok(checksum_add(
                checksum_add(checksum_add(0, source), destination),
                &length_bytes,
            ) + u64::from(ip.protocol))
        }
    }
}

fn visit_tcp_segments<B>(
    packet: &[u8],
    plan: TcpSegmentationPlan,
    envelope: VirtioNetworkPacketEnvelope,
    visitor: &mut impl FnMut(&[u8]) -> ControlFlow<B>,
) -> Result<ControlFlow<B>, VirtioNetworkPacketPlanError> {
    let payload = packet
        .get(plan.payload_offset..)
        .ok_or(VirtioNetworkPacketPlanError::MalformedTcp)?;
    let chunk_count = payload.len().max(1).div_ceil(plan.segment_payload_len);
    let original_sequence = read_u32_be(packet, plan.ip.transport_offset + TCP_SEQUENCE_OFFSET)
        .ok_or(VirtioNetworkPacketPlanError::MalformedTcp)?;
    let original_flags = packet_byte(packet, plan.ip.transport_offset + TCP_FLAGS_OFFSET)
        .ok_or(VirtioNetworkPacketPlanError::MalformedTcp)?;
    let mut payload_offset = 0_usize;
    for index in 0..chunk_count {
        let chunk_start = index
            .checked_mul(plan.segment_payload_len)
            .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
        let chunk_end = chunk_start
            .saturating_add(plan.segment_payload_len)
            .min(payload.len());
        let chunk = payload
            .get(chunk_start..chunk_end)
            .ok_or(VirtioNetworkPacketPlanError::MalformedTcp)?;
        let header = packet
            .get(..plan.payload_offset)
            .ok_or(VirtioNetworkPacketPlanError::MalformedTcp)?;
        let mut segment = try_copy_packet_with_extra_capacity(header, chunk.len())?;
        segment.extend_from_slice(chunk);
        update_ip_packet_length(&mut segment, plan.ip)?;
        let sequence_delta = u32::try_from(payload_offset)
            .map_err(|_| VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
        write_u32_be(
            &mut segment,
            plan.ip.transport_offset + TCP_SEQUENCE_OFFSET,
            original_sequence.wrapping_add(sequence_delta),
        )?;
        let is_last = index + 1 == chunk_count;
        let flags = if is_last {
            original_flags
        } else {
            original_flags & !(TCP_FLAG_FIN | TCP_FLAG_PSH)
        };
        write_byte(
            &mut segment,
            plan.ip.transport_offset + TCP_FLAGS_OFFSET,
            flags,
        )?;
        if let IpVersion::V4 { .. } = plan.ip.version {
            let identification =
                read_u16_be(packet, plan.ip.ip_offset + IPV4_IDENTIFICATION_OFFSET)
                    .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
            let segment_index =
                u16::try_from(index).map_err(|_| VirtioNetworkPacketPlanError::TooManyPackets {
                    packet_count: chunk_count,
                })?;
            write_u16_be(
                &mut segment,
                plan.ip.ip_offset + IPV4_IDENTIFICATION_OFFSET,
                identification.wrapping_add(segment_index),
            )?;
            write_ipv4_header_checksum(&mut segment, plan.ip)?;
        }
        write_transport_checksum(&mut segment, plan.ip, TCP_CHECKSUM_OFFSET, false)?;
        if let ControlFlow::Break(value) = visit_packet(envelope, &segment, visitor)? {
            return Ok(ControlFlow::Break(value));
        }
        payload_offset = payload_offset
            .checked_add(chunk.len())
            .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    }
    Ok(ControlFlow::Continue(()))
}

fn visit_udp_fragments<B>(
    packet: &[u8],
    plan: UdpFragmentationPlan,
    envelope: VirtioNetworkPacketEnvelope,
    visitor: &mut impl FnMut(&[u8]) -> ControlFlow<B>,
) -> Result<ControlFlow<B>, VirtioNetworkPacketPlanError> {
    let mut checksummed = try_copy_packet(packet)?;
    write_transport_checksum(&mut checksummed, plan.ip, UDP_CHECKSUM_OFFSET, true)?;
    let datagram = checksummed
        .get(plan.ip.transport_offset..)
        .ok_or(VirtioNetworkPacketPlanError::MalformedUdp)?;
    if datagram.len() <= plan.fragment_payload_len {
        return visit_packet(envelope, &checksummed, visitor);
    }
    let chunks = datagram.chunks(plan.fragment_payload_len);
    let chunk_count = datagram.len().max(1).div_ceil(plan.fragment_payload_len);
    for (index, chunk) in chunks.enumerate() {
        let fragment_offset = index
            .checked_mul(plan.fragment_payload_len)
            .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
        let is_last = index + 1 == chunk_count;
        let fragment = match plan.ip.version {
            IpVersion::V4 { header_len } => emit_ipv4_udp_fragment(
                &checksummed,
                plan.ip,
                header_len,
                chunk,
                fragment_offset,
                is_last,
            )?,
            IpVersion::V6 => emit_ipv6_udp_fragment(
                &checksummed,
                plan.ip,
                plan.ipv6_fragment_identification,
                chunk,
                fragment_offset,
                is_last,
            )?,
        };
        if let ControlFlow::Break(value) = visit_packet(envelope, &fragment, visitor)? {
            return Ok(ControlFlow::Break(value));
        }
    }
    Ok(ControlFlow::Continue(()))
}

fn emit_ipv4_udp_fragment(
    packet: &[u8],
    ip: IpPacketLayout,
    header_len: usize,
    chunk: &[u8],
    fragment_offset: usize,
    is_last: bool,
) -> Result<Vec<u8>, VirtioNetworkPacketPlanError> {
    let header_end = ip
        .ip_offset
        .checked_add(header_len)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let header = packet
        .get(..header_end)
        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
    let mut fragment = try_copy_packet_with_extra_capacity(header, chunk.len())?;
    if fragment_offset != 0 {
        neutralize_noncopied_ipv4_fragment_options(&mut fragment, ip.ip_offset, header_len)?;
    }
    fragment.extend_from_slice(chunk);
    update_ip_packet_length(&mut fragment, ip)?;
    let offset_units = u16::try_from(fragment_offset / 8)
        .map_err(|_| VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    if offset_units > 0x1fff {
        return Err(VirtioNetworkPacketPlanError::PacketLengthOverflow);
    }
    let fragment_field = offset_units | if is_last { 0 } else { IPV4_MORE_FRAGMENTS };
    write_u16_be(
        &mut fragment,
        ip.ip_offset + IPV4_FRAGMENT_OFFSET,
        fragment_field,
    )?;
    write_ipv4_header_checksum(&mut fragment, ip)?;
    Ok(fragment)
}

fn emit_ipv6_udp_fragment(
    packet: &[u8],
    ip: IpPacketLayout,
    identification: u32,
    chunk: &[u8],
    fragment_offset: usize,
    is_last: bool,
) -> Result<Vec<u8>, VirtioNetworkPacketPlanError> {
    let base_end = ip
        .ip_offset
        .checked_add(IPV6_HEADER_LEN)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let header = packet
        .get(..base_end)
        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv6)?;
    let extra_capacity = IPV6_FRAGMENT_HEADER_LEN
        .checked_add(chunk.len())
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let mut fragment = try_copy_packet_with_extra_capacity(header, extra_capacity)?;
    write_byte(
        &mut fragment,
        ip.ip_offset + IPV6_NEXT_HEADER_OFFSET,
        IPV6_FRAGMENT_NEXT_HEADER,
    )?;
    fragment.push(IP_PROTOCOL_UDP);
    fragment.push(0);
    let offset = u16::try_from(fragment_offset)
        .map_err(|_| VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let offset_and_flags = (offset & 0xfff8) | if is_last { 0 } else { IPV6_FRAGMENT_MORE };
    fragment.extend_from_slice(&offset_and_flags.to_be_bytes());
    fragment.extend_from_slice(&identification.to_be_bytes());
    fragment.extend_from_slice(chunk);
    update_ip_packet_length(&mut fragment, ip)?;
    Ok(fragment)
}

fn update_ip_packet_length(
    packet: &mut [u8],
    ip: IpPacketLayout,
) -> Result<(), VirtioNetworkPacketPlanError> {
    match ip.version {
        IpVersion::V4 { .. } => {
            let total_len = packet.len().saturating_sub(ip.ip_offset);
            let total_len = u16::try_from(total_len)
                .map_err(|_| VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
            write_u16_be(packet, ip.ip_offset + IPV4_TOTAL_LENGTH_OFFSET, total_len)
        }
        IpVersion::V6 => {
            let payload_len = packet
                .len()
                .saturating_sub(ip.ip_offset.saturating_add(IPV6_HEADER_LEN));
            let payload_len = u16::try_from(payload_len)
                .map_err(|_| VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
            write_u16_be(
                packet,
                ip.ip_offset + IPV6_PAYLOAD_LENGTH_OFFSET,
                payload_len,
            )
        }
    }
}

fn write_ipv4_header_checksum(
    packet: &mut [u8],
    ip: IpPacketLayout,
) -> Result<(), VirtioNetworkPacketPlanError> {
    let IpVersion::V4 { header_len } = ip.version else {
        return Ok(());
    };
    let checksum_offset = ip
        .ip_offset
        .checked_add(IPV4_CHECKSUM_OFFSET)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    write_u16_be(packet, checksum_offset, 0)?;
    let header = packet
        .get(ip.ip_offset..ip.ip_offset + header_len)
        .ok_or(VirtioNetworkPacketPlanError::MalformedIpv4)?;
    let checksum = checksum_finish(checksum_add(0, header));
    write_u16_be(packet, checksum_offset, checksum)
}

fn append_packet(
    bytes: &mut Vec<u8>,
    ranges: &mut Vec<Range<usize>>,
    envelope: VirtioNetworkPacketEnvelope,
    packet: &[u8],
) -> Result<(), VirtioNetworkPacketPlanError> {
    let start = bytes.len();
    if envelope == VirtioNetworkPacketEnvelope::DirectVirtioHeader {
        bytes.extend_from_slice(&[0; VIRTIO_NET_TX_HEADER_SIZE as usize]);
    }
    bytes.extend_from_slice(packet);
    let end = bytes.len();
    if end <= start {
        return Err(VirtioNetworkPacketPlanError::PacketLengthOverflow);
    }
    ranges.push(start..end);
    Ok(())
}

fn ethernet_source_mac(packet: &[u8]) -> Option<[u8; ETHERNET_ADDRESS_LEN]> {
    let source =
        packet.get(ETHERNET_SOURCE_OFFSET..ETHERNET_SOURCE_OFFSET + ETHERNET_ADDRESS_LEN)?;
    source.try_into().ok()
}

fn checksum_add(mut sum: u64, bytes: &[u8]) -> u64 {
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        if let Ok(word) = <[u8; 2]>::try_from(chunk) {
            sum += u64::from(u16::from_be_bytes(word));
        }
    }
    if let Some(byte) = chunks.remainder().first() {
        sum += u64::from(*byte) << 8;
    }
    sum
}

fn checksum_finish(mut sum: u64) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn packet_byte(packet: &[u8], offset: usize) -> Option<u8> {
    packet.get(offset).copied()
}

fn read_u16_be(packet: &[u8], offset: usize) -> Option<u16> {
    let bytes = packet.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_be_bytes(bytes.try_into().ok()?))
}

fn read_u32_be(packet: &[u8], offset: usize) -> Option<u32> {
    let bytes = packet.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

fn write_byte(
    packet: &mut [u8],
    offset: usize,
    value: u8,
) -> Result<(), VirtioNetworkPacketPlanError> {
    let destination = packet
        .get_mut(offset)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    *destination = value;
    Ok(())
}

fn write_u16_be(
    packet: &mut [u8],
    offset: usize,
    value: u16,
) -> Result<(), VirtioNetworkPacketPlanError> {
    let end = offset
        .checked_add(2)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let destination = packet
        .get_mut(offset..end)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    destination.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn write_u32_be(
    packet: &mut [u8],
    offset: usize,
    value: u32,
) -> Result<(), VirtioNetworkPacketPlanError> {
    let end = offset
        .checked_add(4)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    let destination = packet
        .get_mut(offset..end)
        .ok_or(VirtioNetworkPacketPlanError::PacketLengthOverflow)?;
    destination.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpVersionDiscriminant {
    V4,
    V6,
}

impl IpVersion {
    const fn discriminant(self) -> IpVersionDiscriminant {
        match self {
            Self::V4 { .. } => IpVersionDiscriminant::V4,
            Self::V6 => IpVersionDiscriminant::V6,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_MAC: [u8; ETHERNET_ADDRESS_LEN] = [0x06, 0x01, 0x02, 0x03, 0x04, 0x05];
    const TCP_ACK: u8 = 0x10;

    fn feature_bits(features: &[u32]) -> u64 {
        features
            .iter()
            .copied()
            .fold(0, |bits, feature| bits | (1_u64 << feature))
    }

    fn assert_debug_redacts(debug_output: &str, protected_value: &str) {
        let byte_sequence = protected_value
            .as_bytes()
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        assert!(!debug_output.contains(protected_value));
        assert!(!debug_output.contains(&byte_sequence));
        assert!(debug_output.contains("[REDACTED]"));
    }

    fn ethernet_prefix(ethertype: u16) -> Vec<u8> {
        let mut packet = Vec::from([0x02, 0, 0, 0, 0, 1]);
        packet.extend_from_slice(&SOURCE_MAC);
        packet.extend_from_slice(&ethertype.to_be_bytes());
        packet
    }

    fn tcp_segment(payload: &[u8]) -> Vec<u8> {
        let mut tcp = vec![0; TCP_MIN_HEADER_LEN];
        write_u16_be(&mut tcp, 0, 12_345).expect("TCP source port should fit");
        write_u16_be(&mut tcp, 2, 443).expect("TCP destination port should fit");
        write_u32_be(&mut tcp, TCP_SEQUENCE_OFFSET, 0x1020_3040).expect("TCP sequence should fit");
        write_byte(&mut tcp, TCP_DATA_OFFSET_FIELD, 5 << 4).expect("TCP data offset should fit");
        write_byte(
            &mut tcp,
            TCP_FLAGS_OFFSET,
            TCP_ACK | TCP_FLAG_PSH | TCP_FLAG_FIN,
        )
        .expect("TCP flags should fit");
        write_u16_be(&mut tcp, 14, 65_535).expect("TCP window should fit");
        tcp.extend_from_slice(payload);
        tcp
    }

    fn udp_datagram(payload: &[u8]) -> Vec<u8> {
        let mut udp = vec![0; UDP_HEADER_LEN];
        write_u16_be(&mut udp, 0, 12_345).expect("UDP source port should fit");
        write_u16_be(&mut udp, 2, 53).expect("UDP destination port should fit");
        let len = u16::try_from(UDP_HEADER_LEN + payload.len())
            .expect("test UDP datagram should fit its length field");
        write_u16_be(&mut udp, UDP_LENGTH_OFFSET, len).expect("UDP length should fit");
        udp.extend_from_slice(payload);
        udp
    }

    fn ipv4_packet(protocol: u8, transport: Vec<u8>) -> Vec<u8> {
        ipv4_packet_with_options(protocol, &[], transport)
    }

    fn ipv4_packet_with_options(protocol: u8, options: &[u8], transport: Vec<u8>) -> Vec<u8> {
        assert!(options.len() <= 40);
        assert!(options.len().is_multiple_of(4));
        let mut packet = ethernet_prefix(ETHERNET_ETHERTYPE_IPV4);
        let ip_offset = packet.len();
        let header_len = IPV4_MIN_HEADER_LEN + options.len();
        packet.resize(ip_offset + header_len, 0);
        let header_words = u8::try_from(header_len / 4).expect("IPv4 IHL should fit");
        write_byte(&mut packet, ip_offset, 0x40 | header_words).expect("IPv4 version should fit");
        let total_len = u16::try_from(header_len + transport.len())
            .expect("test IPv4 packet should fit its length field");
        write_u16_be(&mut packet, ip_offset + IPV4_TOTAL_LENGTH_OFFSET, total_len)
            .expect("IPv4 total length should fit");
        write_u16_be(&mut packet, ip_offset + IPV4_IDENTIFICATION_OFFSET, 0x2345)
            .expect("IPv4 identification should fit");
        write_byte(&mut packet, ip_offset + 8, 64).expect("IPv4 TTL should fit");
        write_byte(&mut packet, ip_offset + IPV4_PROTOCOL_OFFSET, protocol)
            .expect("IPv4 protocol should fit");
        packet
            .get_mut(ip_offset + IPV4_SOURCE_OFFSET..ip_offset + IPV4_SOURCE_OFFSET + 4)
            .expect("IPv4 source address should fit")
            .copy_from_slice(&[192, 0, 2, 1]);
        packet
            .get_mut(ip_offset + IPV4_DESTINATION_OFFSET..ip_offset + IPV4_DESTINATION_OFFSET + 4)
            .expect("IPv4 destination address should fit")
            .copy_from_slice(&[198, 51, 100, 2]);
        packet
            .get_mut(ip_offset + IPV4_MIN_HEADER_LEN..ip_offset + header_len)
            .expect("IPv4 options should fit")
            .copy_from_slice(options);
        packet.extend_from_slice(&transport);
        write_ipv4_header_checksum(
            &mut packet,
            IpPacketLayout {
                version: IpVersion::V4 { header_len },
                ip_offset,
                transport_offset: ip_offset + header_len,
                protocol,
            },
        )
        .expect("IPv4 header checksum should fit");
        packet
    }

    fn ipv6_packet(protocol: u8, transport: Vec<u8>) -> Vec<u8> {
        let mut packet = ethernet_prefix(ETHERNET_ETHERTYPE_IPV6);
        let ip_offset = packet.len();
        packet.resize(ip_offset + IPV6_HEADER_LEN, 0);
        write_byte(&mut packet, ip_offset, 0x60).expect("IPv6 version should fit");
        let payload_len =
            u16::try_from(transport.len()).expect("test IPv6 packet should fit its length field");
        write_u16_be(
            &mut packet,
            ip_offset + IPV6_PAYLOAD_LENGTH_OFFSET,
            payload_len,
        )
        .expect("IPv6 payload length should fit");
        write_byte(&mut packet, ip_offset + IPV6_NEXT_HEADER_OFFSET, protocol)
            .expect("IPv6 next header should fit");
        write_byte(&mut packet, ip_offset + 7, 64).expect("IPv6 hop limit should fit");
        packet
            .get_mut(ip_offset + IPV6_SOURCE_OFFSET..ip_offset + IPV6_SOURCE_OFFSET + 16)
            .expect("IPv6 source address should fit")
            .copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        packet
            .get_mut(ip_offset + IPV6_DESTINATION_OFFSET..ip_offset + IPV6_DESTINATION_OFFSET + 16)
            .expect("IPv6 destination address should fit")
            .copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        packet.extend_from_slice(&transport);
        packet
    }

    #[test]
    fn packet_plan_and_emitted_packet_debug_redact_network_bytes() {
        let protected_value = "private-network-token-value-that-must-not-appear";
        let packet = ipv4_packet(IP_PROTOCOL_TCP, tcp_segment(protected_value.as_bytes()));
        let plan = VirtioNetworkPacketPlan::prepare(VirtioNetworkTxHeader::new(), 0, packet)
            .expect("test network packet should prepare");
        let emitted = plan
            .emit(VirtioNetworkPacketEnvelope::RawEthernet)
            .expect("test network packet should emit");

        assert_debug_redacts(&format!("{plan:?}"), protected_value);
        assert_debug_redacts(&format!("{emitted:?}"), protected_value);
    }

    fn offload_header(
        gso_type: u8,
        gso_size: u16,
        checksum_start: u16,
        offset: u16,
    ) -> VirtioNetworkTxHeader {
        VirtioNetworkTxHeader::new()
            .with_flags(VIRTIO_NET_HDR_F_NEEDS_CSUM)
            .with_gso_type(gso_type)
            .with_header_len(1)
            .with_gso_size(gso_size)
            .with_checksum_start(checksum_start)
            .with_checksum_offset(offset)
    }

    fn emitted_packet(emitted: &VirtioNetworkEmittedPackets, index: usize) -> &[u8] {
        let range = emitted
            .ranges()
            .get(index)
            .expect("emitted packet range should exist");
        emitted
            .bytes()
            .get(range.clone())
            .expect("emitted packet bytes should exist")
    }

    fn assert_transport_checksum(packet: &[u8]) {
        let ip = parse_ip_packet(packet).expect("normalized IP packet should parse");
        let transport = packet
            .get(ip.transport_offset..)
            .expect("normalized transport should exist");
        let sum =
            pseudo_header_sum(packet, ip, transport.len()).expect("pseudo header should be valid");
        assert_eq!(checksum_finish(checksum_add(sum, transport)), 0);
    }

    fn seed_partial_transport_checksum(packet: &mut [u8]) {
        let ip = parse_ip_packet(packet).expect("partial-checksum IP packet should parse");
        let checksum_offset = match ip.protocol {
            IP_PROTOCOL_TCP => TCP_CHECKSUM_OFFSET,
            IP_PROTOCOL_UDP => UDP_CHECKSUM_OFFSET,
            protocol => panic!("unsupported test transport protocol {protocol}"),
        };
        let transport_len = packet.len() - ip.transport_offset;
        let seed = !checksum_finish(
            pseudo_header_sum(packet, ip, transport_len)
                .expect("partial-checksum pseudo header should be valid"),
        );
        write_u16_be(packet, ip.transport_offset + checksum_offset, seed)
            .expect("partial-checksum seed should fit");
    }

    fn assert_ipv4_header_checksum(packet: &[u8]) {
        let (ethertype, ip_offset) = parse_ethernet(packet).expect("Ethernet packet should parse");
        assert_eq!(ethertype, ETHERNET_ETHERTYPE_IPV4);
        let version_ihl = packet_byte(packet, ip_offset).expect("IPv4 header should exist");
        assert_eq!(version_ihl >> 4, 4);
        let header_len = usize::from(version_ihl & 0x0f) * 4;
        let header = packet
            .get(ip_offset..ip_offset + header_len)
            .expect("IPv4 header should exist");
        assert_eq!(checksum_finish(checksum_add(0, header)), 0);
    }

    #[test]
    fn plain_and_direct_packets_preserve_ethernet_bytes_and_source_mac() {
        let packet = ipv4_packet(IP_PROTOCOL_UDP, udp_datagram(&[1, 2, 3, 4]));
        let plan = VirtioNetworkPacketPlan::prepare(
            VirtioNetworkTxHeader::new().with_flags(0x80),
            0,
            packet.clone(),
        )
        .expect("unknown flag bits should be ignored");

        let raw = plan
            .emit(VirtioNetworkPacketEnvelope::RawEthernet)
            .expect("raw emission should succeed");
        assert_eq!(raw.packet_count(), 1);
        assert_eq!(raw.source_mac(), Some(SOURCE_MAC));
        assert_eq!(emitted_packet(&raw, 0), packet);

        let direct = plan
            .emit(VirtioNetworkPacketEnvelope::DirectVirtioHeader)
            .expect("direct emission should succeed");
        assert_eq!(direct.packet_count(), 1);
        let direct_packet = emitted_packet(&direct, 0);
        assert_eq!(
            direct_packet
                .get(..VIRTIO_NET_TX_HEADER_SIZE as usize)
                .expect("direct header should exist"),
            [0; VIRTIO_NET_TX_HEADER_SIZE as usize]
        );
        assert_eq!(
            direct_packet
                .get(VIRTIO_NET_TX_HEADER_SIZE as usize..)
                .expect("direct Ethernet packet should exist"),
            packet
        );
    }

    #[test]
    fn checksum_completion_supports_ipv4_udp_and_ipv6_tcp() {
        let mut ipv4 = ipv4_packet(IP_PROTOCOL_UDP, udp_datagram(&[1, 2, 3, 4, 5]));
        seed_partial_transport_checksum(&mut ipv4);
        let mut ipv6 = ipv6_packet(IP_PROTOCOL_TCP, tcp_segment(&[6, 7, 8, 9, 10]));
        seed_partial_transport_checksum(&mut ipv6);
        let packets = [
            (ipv4, 34_u16, UDP_CHECKSUM_OFFSET as u16),
            (ipv6, 54_u16, TCP_CHECKSUM_OFFSET as u16),
        ];
        for (packet, checksum_start, checksum_offset) in packets {
            let plan = VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_NONE, 0, checksum_start, checksum_offset),
                feature_bits(&[VIRTIO_NET_F_CSUM]),
                packet,
            )
            .expect("checksum plan should validate");
            let emitted = plan
                .emit(VirtioNetworkPacketEnvelope::RawEthernet)
                .expect("checksum completion should succeed");
            assert_transport_checksum(emitted_packet(&emitted, 0));
        }
    }

    #[test]
    fn checksum_completion_honors_generic_start_offset_and_seed() {
        let mut packet = vec![0x90, 0x91, 0x10, 0x11, 0x12, 0x13, 0x24, 0x68, 0x14];
        let expected = checksum_finish(checksum_add(0, &packet[2..]));
        let expected = if expected == 0 { u16::MAX } else { expected };
        let plan = VirtioNetworkPacketPlan::prepare(
            offload_header(VIRTIO_NET_HDR_GSO_NONE, 0, 2, 4),
            feature_bits(&[VIRTIO_NET_F_CSUM]),
            packet.clone(),
        )
        .expect("generic checksum plan should validate");
        let emitted = plan
            .emit(VirtioNetworkPacketEnvelope::RawEthernet)
            .expect("generic checksum completion should succeed");
        packet[6..8].copy_from_slice(&expected.to_be_bytes());
        assert_eq!(emitted_packet(&emitted, 0), packet);
    }

    #[test]
    fn tcpv4_segmentation_updates_lengths_sequences_flags_and_checksums() {
        let payload = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let packet = ipv4_packet(IP_PROTOCOL_TCP, tcp_segment(&payload));
        let plan = VirtioNetworkPacketPlan::prepare(
            offload_header(VIRTIO_NET_HDR_GSO_TCPV4, 4, 34, TCP_CHECKSUM_OFFSET as u16),
            feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_TSO4]),
            packet,
        )
        .expect("TCPv4 segmentation plan should validate");
        let emitted = plan
            .emit(VirtioNetworkPacketEnvelope::RawEthernet)
            .expect("TCPv4 segmentation should succeed");
        assert_eq!(emitted.packet_count(), 3);

        for index in 0..emitted.packet_count() {
            let segment = emitted_packet(&emitted, index);
            let ip = parse_ip_packet(segment).expect("TCPv4 segment should parse");
            let sequence = read_u32_be(segment, ip.transport_offset + TCP_SEQUENCE_OFFSET)
                .expect("TCP sequence should exist");
            assert_eq!(
                sequence,
                0x1020_3040 + u32::try_from(index * 4).expect("test sequence delta should fit")
            );
            let flags = packet_byte(segment, ip.transport_offset + TCP_FLAGS_OFFSET)
                .expect("TCP flags should exist");
            if index + 1 == emitted.packet_count() {
                assert_eq!(flags, TCP_ACK | TCP_FLAG_PSH | TCP_FLAG_FIN);
                assert_eq!(segment.len(), ETHERNET_HEADER_LEN + 20 + 20 + 2);
            } else {
                assert_eq!(flags, TCP_ACK);
                assert_eq!(segment.len(), ETHERNET_HEADER_LEN + 20 + 20 + 4);
            }
            assert_eq!(
                read_u16_be(segment, ip.ip_offset + IPV4_IDENTIFICATION_OFFSET),
                Some(0x2345 + u16::try_from(index).expect("test identification delta should fit"))
            );
            assert_ipv4_header_checksum(segment);
            assert_transport_checksum(segment);
        }
    }

    #[test]
    fn tcpv6_segmentation_updates_lengths_sequences_and_checksums() {
        let packet = ipv6_packet(IP_PROTOCOL_TCP, tcp_segment(&[1, 2, 3, 4, 5, 6, 7, 8, 9]));
        let plan = VirtioNetworkPacketPlan::prepare(
            offload_header(VIRTIO_NET_HDR_GSO_TCPV6, 4, 54, TCP_CHECKSUM_OFFSET as u16),
            feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_TSO6]),
            packet,
        )
        .expect("TCPv6 segmentation plan should validate");
        let emitted = plan
            .emit(VirtioNetworkPacketEnvelope::DirectVirtioHeader)
            .expect("TCPv6 segmentation should succeed");
        assert_eq!(emitted.packet_count(), 3);

        for index in 0..emitted.packet_count() {
            let direct = emitted_packet(&emitted, index);
            assert_eq!(
                direct
                    .get(..VIRTIO_NET_TX_HEADER_SIZE as usize)
                    .expect("direct header should exist"),
                [0; VIRTIO_NET_TX_HEADER_SIZE as usize]
            );
            let segment = direct
                .get(VIRTIO_NET_TX_HEADER_SIZE as usize..)
                .expect("TCPv6 segment should exist");
            let ip = parse_ip_packet(segment).expect("TCPv6 segment should parse");
            assert_eq!(
                read_u32_be(segment, ip.transport_offset + TCP_SEQUENCE_OFFSET),
                Some(
                    0x1020_3040 + u32::try_from(index * 4).expect("test sequence delta should fit")
                )
            );
            assert_transport_checksum(segment);
        }
    }

    #[test]
    fn ipv4_ufo_emits_valid_ordered_fragments() {
        let packet = ipv4_packet(IP_PROTOCOL_UDP, udp_datagram(&[0xa5; 25]));
        let original_ip = parse_ip_packet(&packet).expect("original IPv4 UDP packet should parse");
        let plan = VirtioNetworkPacketPlan::prepare(
            offload_header(VIRTIO_NET_HDR_GSO_UDP, 16, 34, UDP_CHECKSUM_OFFSET as u16),
            feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
            packet.clone(),
        )
        .expect("IPv4 UFO plan should validate");
        let emitted = plan
            .emit(VirtioNetworkPacketEnvelope::RawEthernet)
            .expect("IPv4 UFO should succeed");
        assert_eq!(emitted.packet_count(), 3);

        let mut datagram = Vec::new();
        for (index, expected_fragment_field) in [0x2000, 0x2002, 0x0004].into_iter().enumerate() {
            let fragment = emitted_packet(&emitted, index);
            assert_eq!(
                read_u16_be(fragment, original_ip.ip_offset + IPV4_FRAGMENT_OFFSET),
                Some(expected_fragment_field)
            );
            assert_ipv4_header_checksum(fragment);
            datagram.extend_from_slice(
                fragment
                    .get(original_ip.transport_offset..)
                    .expect("IPv4 fragment payload should exist"),
            );
        }
        let sum = pseudo_header_sum(&packet, original_ip, datagram.len())
            .expect("IPv4 UDP pseudo header should be valid");
        assert_eq!(checksum_finish(checksum_add(sum, &datagram)), 0);
    }

    #[test]
    fn ipv4_ufo_neutralizes_noncopied_options_after_first_fragment() {
        let options = [0x82, 4, 0xaa, 0xbb, 0x02, 4, 0xcc, 0xdd];
        let packet = ipv4_packet_with_options(IP_PROTOCOL_UDP, &options, udp_datagram(&[0xa5; 25]));
        let original_ip = parse_ip_packet(&packet).expect("IPv4 options should parse");
        let checksum_start =
            u16::try_from(original_ip.transport_offset).expect("test checksum start should fit");
        let plan = VirtioNetworkPacketPlan::prepare(
            offload_header(
                VIRTIO_NET_HDR_GSO_UDP,
                16,
                checksum_start,
                UDP_CHECKSUM_OFFSET as u16,
            ),
            feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
            packet.clone(),
        )
        .expect("IPv4 UFO options should validate");
        let emitted = plan
            .emit(VirtioNetworkPacketEnvelope::RawEthernet)
            .expect("IPv4 UFO options should emit");

        let mut datagram = Vec::new();
        for index in 0..emitted.packet_count() {
            let fragment = emitted_packet(&emitted, index);
            let emitted_options = fragment
                .get(original_ip.ip_offset + IPV4_MIN_HEADER_LEN..original_ip.transport_offset)
                .expect("fragment options should exist");
            if index == 0 {
                assert_eq!(emitted_options, options);
            } else {
                assert_eq!(emitted_options, [0x82, 4, 0xaa, 0xbb, 1, 1, 1, 1]);
            }
            assert_ipv4_header_checksum(fragment);
            datagram.extend_from_slice(
                fragment
                    .get(original_ip.transport_offset..)
                    .expect("fragment payload should exist"),
            );
        }
        let sum = pseudo_header_sum(&packet, original_ip, datagram.len())
            .expect("IPv4 UDP pseudo header should be valid");
        assert_eq!(checksum_finish(checksum_add(sum, &datagram)), 0);
    }

    #[test]
    fn ipv4_ufo_rejects_malformed_options_before_emission() {
        let packet =
            ipv4_packet_with_options(IP_PROTOCOL_UDP, &[0x82, 1, 0, 0], udp_datagram(&[0xa5; 25]));
        let checksum_start = u16::try_from(ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN + 4)
            .expect("test checksum start should fit");

        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                offload_header(
                    VIRTIO_NET_HDR_GSO_UDP,
                    16,
                    checksum_start,
                    UDP_CHECKSUM_OFFSET as u16,
                ),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
                packet,
            ),
            Err(VirtioNetworkPacketPlanError::MalformedIpv4)
        ));

        let invalid_padding = ipv4_packet_with_options(
            IP_PROTOCOL_UDP,
            &[IPV4_OPTION_END, 0xff, 0, 0],
            udp_datagram(&[0xa5; 25]),
        );
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                offload_header(
                    VIRTIO_NET_HDR_GSO_UDP,
                    16,
                    checksum_start,
                    UDP_CHECKSUM_OFFSET as u16,
                ),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
                invalid_padding,
            ),
            Err(VirtioNetworkPacketPlanError::MalformedIpv4)
        ));
    }

    #[test]
    fn ipv6_ufo_emits_valid_ordered_fragments() {
        let packet = ipv6_packet(IP_PROTOCOL_UDP, udp_datagram(&[0x5a; 25]));
        let original_ip = parse_ip_packet(&packet).expect("original IPv6 UDP packet should parse");
        let plan = VirtioNetworkPacketPlan::prepare(
            offload_header(VIRTIO_NET_HDR_GSO_UDP, 16, 54, UDP_CHECKSUM_OFFSET as u16),
            feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
            packet.clone(),
        )
        .expect("IPv6 UFO plan should validate");
        let emitted = plan
            .emit(VirtioNetworkPacketEnvelope::RawEthernet)
            .expect("IPv6 UFO should succeed");
        assert_eq!(emitted.packet_count(), 3);

        let fragment_header = original_ip.ip_offset + IPV6_HEADER_LEN;
        let mut datagram = Vec::new();
        let mut identification = None;
        for (index, expected_fragment_field) in [0x0001, 0x0011, 0x0020].into_iter().enumerate() {
            let fragment = emitted_packet(&emitted, index);
            assert_eq!(
                packet_byte(fragment, original_ip.ip_offset + IPV6_NEXT_HEADER_OFFSET),
                Some(IPV6_FRAGMENT_NEXT_HEADER)
            );
            assert_eq!(
                read_u16_be(fragment, fragment_header + 2),
                Some(expected_fragment_field)
            );
            let current_identification = read_u32_be(fragment, fragment_header + 4)
                .expect("IPv6 fragment identification should exist");
            assert!(identification.is_none_or(|value| value == current_identification));
            identification = Some(current_identification);
            datagram.extend_from_slice(
                fragment
                    .get(fragment_header + IPV6_FRAGMENT_HEADER_LEN..)
                    .expect("IPv6 fragment payload should exist"),
            );
        }
        let sum = pseudo_header_sum(&packet, original_ip, datagram.len())
            .expect("IPv6 UDP pseudo header should be valid");
        assert_eq!(checksum_finish(checksum_add(sum, &datagram)), 0);

        let next = VirtioNetworkPacketPlan::prepare(
            offload_header(VIRTIO_NET_HDR_GSO_UDP, 16, 54, UDP_CHECKSUM_OFFSET as u16),
            feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
            packet,
        )
        .expect("next IPv6 UFO plan should validate")
        .emit(VirtioNetworkPacketEnvelope::RawEthernet)
        .expect("next IPv6 UFO emission should succeed");
        let next_identification = read_u32_be(emitted_packet(&next, 0), fragment_header + 4)
            .expect("next IPv6 fragment identification should exist");
        assert_ne!(identification, Some(next_identification));
    }

    #[test]
    fn ipv6_ufo_does_not_add_an_atomic_fragment_when_datagram_already_fits() {
        let packet = ipv6_packet(IP_PROTOCOL_UDP, udp_datagram(&[0x5a; 4]));
        let plan = VirtioNetworkPacketPlan::prepare(
            offload_header(VIRTIO_NET_HDR_GSO_UDP, 16, 54, UDP_CHECKSUM_OFFSET as u16),
            feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
            packet.clone(),
        )
        .expect("small IPv6 UFO plan should validate");
        assert_eq!(
            plan.emitted_len(VirtioNetworkPacketEnvelope::RawEthernet)
                .expect("small IPv6 UFO length should fit"),
            packet.len()
        );
        let emitted = plan
            .emit(VirtioNetworkPacketEnvelope::RawEthernet)
            .expect("small IPv6 UFO plan should emit");

        assert_eq!(emitted.packet_count(), 1);
        assert_eq!(emitted.bytes().len(), packet.len());
        let normalized = emitted_packet(&emitted, 0);
        assert_eq!(
            packet_byte(normalized, 14 + IPV6_NEXT_HEADER_OFFSET),
            Some(IP_PROTOCOL_UDP)
        );
        assert_transport_checksum(normalized);
    }

    #[test]
    fn emitted_lengths_are_exact_for_every_plan_and_envelope() {
        let plans = [
            VirtioNetworkPacketPlan::prepare(
                VirtioNetworkTxHeader::new(),
                0,
                ipv4_packet(IP_PROTOCOL_TCP, tcp_segment(&[1, 2, 3])),
            )
            .expect("single-packet plan should validate"),
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_TCPV4, 4, 34, TCP_CHECKSUM_OFFSET as u16),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_TSO4]),
                ipv4_packet(IP_PROTOCOL_TCP, tcp_segment(&[1, 2, 3, 4, 5, 6, 7, 8, 9])),
            )
            .expect("TCP segmentation plan should validate"),
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_UDP, 16, 34, UDP_CHECKSUM_OFFSET as u16),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
                ipv4_packet(IP_PROTOCOL_UDP, udp_datagram(&[0xa5; 25])),
            )
            .expect("IPv4 UFO plan should validate"),
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_UDP, 16, 54, UDP_CHECKSUM_OFFSET as u16),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
                ipv6_packet(IP_PROTOCOL_UDP, udp_datagram(&[0x5a; 25])),
            )
            .expect("IPv6 UFO plan should validate"),
        ];

        for plan in plans {
            for envelope in [
                VirtioNetworkPacketEnvelope::RawEthernet,
                VirtioNetworkPacketEnvelope::DirectVirtioHeader,
            ] {
                let expected = plan
                    .emitted_len(envelope)
                    .expect("emitted length should fit");
                let emitted = plan.emit(envelope).expect("plan emission should succeed");
                assert_eq!(expected, emitted.bytes().len());
            }
        }
    }

    #[test]
    fn invalid_or_unnegotiated_header_semantics_are_rejected() {
        let packet = ipv4_packet(IP_PROTOCOL_TCP, tcp_segment(&[1, 2, 3, 4]));
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                VirtioNetworkTxHeader::new().with_num_buffers(1),
                0,
                packet.clone(),
            ),
            Err(VirtioNetworkPacketPlanError::TransmitNumBuffers { num_buffers: 1 })
        ));
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                VirtioNetworkTxHeader::new().with_flags(VIRTIO_NET_HDR_F_DATA_VALID),
                0,
                packet.clone(),
            ),
            Err(VirtioNetworkPacketPlanError::GuestForbiddenFlags { .. })
        ));
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_NONE, 0, 34, TCP_CHECKSUM_OFFSET as u16,),
                0,
                packet.clone(),
            ),
            Err(VirtioNetworkPacketPlanError::FeatureNotNegotiated {
                feature: VIRTIO_NET_F_CSUM
            })
        ));
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_TCPV4, 4, 34, TCP_CHECKSUM_OFFSET as u16,),
                feature_bits(&[VIRTIO_NET_F_CSUM]),
                packet.clone(),
            ),
            Err(VirtioNetworkPacketPlanError::FeatureNotNegotiated {
                feature: VIRTIO_NET_F_HOST_TSO4
            })
        ));
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                VirtioNetworkTxHeader::new().with_gso_type(0x7f),
                u64::MAX,
                packet.clone(),
            ),
            Err(VirtioNetworkPacketPlanError::UnsupportedGsoType { .. })
        ));
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                VirtioNetworkTxHeader::new().with_gso_size(1),
                0,
                packet,
            ),
            Err(VirtioNetworkPacketPlanError::UnexpectedGsoSize { gso_size: 1 })
        ));

        let udp = ipv4_packet(IP_PROTOCOL_UDP, udp_datagram(&[1, 2, 3, 4]));
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_UDP, 10, 34, UDP_CHECKSUM_OFFSET as u16,),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
                udp,
            ),
            Err(VirtioNetworkPacketPlanError::UfoGsoSizeUnaligned { gso_size: 10 })
        ));
    }

    #[test]
    fn packet_bounds_and_malformed_transport_layouts_are_rejected_without_panics() {
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(VirtioNetworkTxHeader::new(), 0, Vec::new()),
            Err(VirtioNetworkPacketPlanError::EmptyPacket)
        ));
        let oversized_len =
            usize::try_from(VIRTIO_NET_MAX_BUFFER_SIZE - u64::from(VIRTIO_NET_TX_HEADER_SIZE) + 1)
                .expect("maximum packet bound should fit usize");
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                VirtioNetworkTxHeader::new(),
                0,
                vec![0; oversized_len],
            ),
            Err(VirtioNetworkPacketPlanError::FrameTooLarge { .. })
        ));

        let mut malformed_tcp = ipv4_packet(IP_PROTOCOL_TCP, tcp_segment(&[1, 2, 3, 4]));
        write_byte(&mut malformed_tcp, 34 + TCP_DATA_OFFSET_FIELD, 0x10)
            .expect("test TCP field should exist");
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_TCPV4, 4, 34, TCP_CHECKSUM_OFFSET as u16,),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_TSO4]),
                malformed_tcp,
            ),
            Err(VirtioNetworkPacketPlanError::MalformedTcp)
        ));

        let mut unsupported_tcp = ipv4_packet(IP_PROTOCOL_TCP, tcp_segment(&[1, 2, 3, 4]));
        write_byte(
            &mut unsupported_tcp,
            34 + TCP_FLAGS_OFFSET,
            TCP_ACK | TCP_FLAG_URG,
        )
        .expect("test TCP flags should exist");
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_TCPV4, 4, 34, TCP_CHECKSUM_OFFSET as u16,),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_TSO4]),
                unsupported_tcp,
            ),
            Err(VirtioNetworkPacketPlanError::TcpControlSegment)
        ));

        let fragmented = {
            let mut packet = ipv4_packet(IP_PROTOCOL_UDP, udp_datagram(&[1, 2, 3, 4]));
            write_u16_be(&mut packet, 14 + IPV4_FRAGMENT_OFFSET, IPV4_MORE_FRAGMENTS)
                .expect("test IPv4 fragment field should exist");
            packet
        };
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_UDP, 16, 34, UDP_CHECKSUM_OFFSET as u16,),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
                fragmented,
            ),
            Err(VirtioNetworkPacketPlanError::FragmentedInput)
        ));

        let reserved_fragment_flag = {
            let mut packet = ipv4_packet(IP_PROTOCOL_UDP, udp_datagram(&[1, 2, 3, 4]));
            write_u16_be(&mut packet, 14 + IPV4_FRAGMENT_OFFSET, 0x8000)
                .expect("test IPv4 fragment field should exist");
            packet
        };
        assert!(matches!(
            VirtioNetworkPacketPlan::prepare(
                offload_header(VIRTIO_NET_HDR_GSO_UDP, 16, 34, UDP_CHECKSUM_OFFSET as u16,),
                feature_bits(&[VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_UFO]),
                reserved_fragment_flag,
            ),
            Err(VirtioNetworkPacketPlanError::FragmentedInput)
        ));
    }
}
