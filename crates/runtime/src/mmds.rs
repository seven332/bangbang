//! Backend-neutral MMDS control-plane input and metadata query model.

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;
use std::str;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Map, Value};

use crate::network::NetworkInterfaceConfig;

pub const MMDS_DATA_STORE_LIMIT_BYTES: usize = 51_200;
pub const MMDS_GUEST_TCP_PORT: u16 = 80;
pub const MMDS_TOKEN_MIN_TTL_SECONDS: u32 = 1;
pub const MMDS_TOKEN_MAX_TTL_SECONDS: u32 = 21_600;
pub const MMDS_TOKEN_MAX_ACTIVE_TOKENS: usize = 1_024;
pub const DEFAULT_MMDS_IPV4_ADDRESS: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
pub const DEFAULT_MMDS_MAC_ADDRESS: EthernetMacAddress =
    EthernetMacAddress::from_octets([0x06, 0x01, 0x23, 0x45, 0x67, 0x01]);

const ETHERNET_ETHERTYPE_ARP: u16 = 0x0806;
const ETHERNET_ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERNET_DESTINATION_ADDRESS_OFFSET: usize = 0;
const ETHERNET_ETHERTYPE_OFFSET: usize = 12;
const ETHERNET_HEADER_LEN: usize = 14;
const ETHERNET_MAC_ADDRESS_LEN: usize = 6;
const ETHERNET_SOURCE_ADDRESS_OFFSET: usize = 6;
const IPV4_HEADER_CHECKSUM_OFFSET: usize = 10;
const IPV4_DESTINATION_ADDRESS_OFFSET: usize = 16;
const IPV4_FLAGS_FRAGMENT_OFFSET_OFFSET: usize = 6;
const IPV4_MAX_TOTAL_LENGTH: usize = u16::MAX as usize;
const IPV4_MIN_HEADER_WORDS: u8 = 5;
const IPV4_REJECT_FLAGS_FRAGMENT_MASK: u16 = 0xbfff;
const IPV4_MIN_HEADER_LEN: usize = 20;
const IPV4_PROTOCOL_OFFSET: usize = 9;
const IPV4_PROTOCOL_TCP: u8 = 6;
const IPV4_SOURCE_ADDRESS_OFFSET: usize = 12;
const IPV4_TOTAL_LENGTH_OFFSET: usize = 2;
const IPV4_VERSION: u8 = 4;
const IPV4_VERSION_IHL_OFFSET: usize = 0;
const TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET: usize = 8;
const TCP_CHECKSUM_OFFSET: usize = 16;
const TCP_DATA_OFFSET_OFFSET: usize = 12;
const TCP_DESTINATION_PORT_OFFSET: usize = 2;
const TCP_FLAG_ACK: u8 = 0x10;
const TCP_FLAG_FIN: u8 = 0x01;
const TCP_FLAG_PSH: u8 = 0x08;
const TCP_FLAG_RST: u8 = 0x04;
const TCP_FLAG_SYN: u8 = 0x02;
const TCP_FLAGS_OFFSET: usize = 13;
const TCP_MIN_HEADER_WORDS: u8 = 5;
const TCP_MIN_HEADER_LEN: usize = 20;
const TCP_SEQUENCE_NUMBER_OFFSET: usize = 4;
const TCP_SOURCE_PORT_OFFSET: usize = 0;
const ARP_ETHERNET_IPV4_LEN: usize = 28;
const ARP_HARDWARE_ADDRESS_LEN_OFFSET: usize = 4;
const ARP_HARDWARE_ADDRESS_LEN_ETHERNET: u8 = ETHERNET_MAC_ADDRESS_LEN as u8;
const ARP_HARDWARE_TYPE_ETHERNET: u16 = 1;
const ARP_HARDWARE_TYPE_OFFSET: usize = 0;
const ARP_OPERATION_OFFSET: usize = 6;
const ARP_OPERATION_REPLY: u16 = 2;
const ARP_OPERATION_REQUEST: u16 = 1;
const ARP_PROTOCOL_ADDRESS_LEN_IPV4: u8 = 4;
const ARP_PROTOCOL_ADDRESS_LEN_OFFSET: usize = 5;
const ARP_PROTOCOL_TYPE_IPV4: u16 = ETHERNET_ETHERTYPE_IPV4;
const ARP_PROTOCOL_TYPE_OFFSET: usize = 2;
const ARP_SENDER_HARDWARE_ADDRESS_OFFSET: usize = 8;
const ARP_SENDER_PROTOCOL_ADDRESS_OFFSET: usize = 14;
const ARP_TARGET_PROTOCOL_ADDRESS_OFFSET: usize = 24;
const MMDS_TOKEN_BYTES: usize = 32;
const MMDS_GUEST_TCP_SYN_ACK_SEQUENCE_NUMBER: u32 = 0;
const MMDS_GUEST_ALLOW_METHODS: &str = "GET, PUT";
const MMDS_GUEST_INVALID_TOKEN: &str = "MMDS token not valid.";
const MMDS_GUEST_MISSING_TOKEN: &str = "No MMDS token provided. Use `X-metadata-token` or `X-aws-ec2-metadata-token` header to specify the session token.";
const MMDS_GUEST_TOKEN_PATH: &str = "/latest/api/token";
const MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN: &str = "X-aws-ec2-metadata-token";
const MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN_TTL_SECONDS: &str =
    "X-aws-ec2-metadata-token-ttl-seconds";
const MMDS_GUEST_X_FORWARDED_FOR: &str = "X-Forwarded-For";
const MMDS_GUEST_X_METADATA_TOKEN: &str = "X-metadata-token";
const MMDS_GUEST_X_METADATA_TOKEN_TTL_SECONDS: &str = "X-metadata-token-ttl-seconds";
const MMDS_MILLISECONDS_PER_SECOND: u64 = 1_000;
const MMDS_TOKEN_GENERATION_ATTEMPTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthernetMacAddress {
    octets: [u8; ETHERNET_MAC_ADDRESS_LEN],
}

impl EthernetMacAddress {
    pub const fn from_octets(octets: [u8; ETHERNET_MAC_ADDRESS_LEN]) -> Self {
        Self { octets }
    }

    pub const fn octets(self) -> [u8; ETHERNET_MAC_ADDRESS_LEN] {
        self.octets
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmdsGuestTcpPacket<'a> {
    source_ethernet_address: EthernetMacAddress,
    destination_ethernet_address: EthernetMacAddress,
    source_ipv4_address: Ipv4Addr,
    destination_ipv4_address: Ipv4Addr,
    source_port: u16,
    destination_port: u16,
    sequence_number: u32,
    acknowledgement_number: u32,
    tcp_flags: u8,
    payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmdsGuestArpRequest {
    source_ethernet_address: EthernetMacAddress,
    destination_ethernet_address: EthernetMacAddress,
    sender_hardware_address: EthernetMacAddress,
    sender_protocol_address: Ipv4Addr,
    target_protocol_address: Ipv4Addr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmdsGuestTcpResponseContext {
    source_ethernet_address: EthernetMacAddress,
    destination_ethernet_address: EthernetMacAddress,
    source_ipv4_address: Ipv4Addr,
    destination_ipv4_address: Ipv4Addr,
    source_port: u16,
    destination_port: u16,
    sequence_number: u32,
    acknowledgement_number: u32,
    tcp_flags: u8,
}

impl MmdsGuestArpRequest {
    pub const fn source_ethernet_address(self) -> EthernetMacAddress {
        self.source_ethernet_address
    }

    pub const fn destination_ethernet_address(self) -> EthernetMacAddress {
        self.destination_ethernet_address
    }

    pub const fn sender_hardware_address(self) -> EthernetMacAddress {
        self.sender_hardware_address
    }

    pub const fn sender_protocol_address(self) -> Ipv4Addr {
        self.sender_protocol_address
    }

    pub const fn target_protocol_address(self) -> Ipv4Addr {
        self.target_protocol_address
    }

    pub fn response_frame(self) -> Result<Vec<u8>, MmdsGuestArpResponseFrameError> {
        mmds_guest_arp_response_frame(self, DEFAULT_MMDS_MAC_ADDRESS)
    }
}

impl<'a> MmdsGuestTcpPacket<'a> {
    pub const fn source_ethernet_address(self) -> EthernetMacAddress {
        self.source_ethernet_address
    }

    pub const fn destination_ethernet_address(self) -> EthernetMacAddress {
        self.destination_ethernet_address
    }

    pub const fn source_ipv4_address(self) -> Ipv4Addr {
        self.source_ipv4_address
    }

    pub const fn destination_ipv4_address(self) -> Ipv4Addr {
        self.destination_ipv4_address
    }

    pub const fn source_port(self) -> u16 {
        self.source_port
    }

    pub const fn destination_port(self) -> u16 {
        self.destination_port
    }

    pub const fn sequence_number(self) -> u32 {
        self.sequence_number
    }

    pub const fn acknowledgement_number(self) -> u32 {
        self.acknowledgement_number
    }

    pub const fn tcp_flags(self) -> u8 {
        self.tcp_flags
    }

    pub const fn payload(self) -> &'a [u8] {
        self.payload
    }

    pub fn is_initial_synchronization_request(self) -> bool {
        self.tcp_flags == TCP_FLAG_SYN && self.payload.is_empty()
    }

    pub fn is_acknowledgement_only(self) -> bool {
        self.tcp_flags == TCP_FLAG_ACK && self.payload.is_empty()
    }

    pub fn is_empty_fin_close_request(self) -> bool {
        (self.tcp_flags == TCP_FLAG_FIN || self.tcp_flags == (TCP_FLAG_FIN | TCP_FLAG_ACK))
            && self.payload.is_empty()
    }

    pub fn is_empty_reset_control(self) -> bool {
        self.tcp_flags & TCP_FLAG_RST != 0 && self.payload.is_empty()
    }

    pub fn is_unsupported_empty_control_reset_request(self) -> bool {
        self.payload.is_empty()
            && !self.is_initial_synchronization_request()
            && !self.is_acknowledgement_only()
            && !self.is_empty_fin_close_request()
            && !self.is_empty_reset_control()
    }

    pub const fn response_context(self) -> MmdsGuestTcpResponseContext {
        MmdsGuestTcpResponseContext {
            source_ethernet_address: self.source_ethernet_address,
            destination_ethernet_address: self.destination_ethernet_address,
            source_ipv4_address: self.source_ipv4_address,
            destination_ipv4_address: self.destination_ipv4_address,
            source_port: self.source_port,
            destination_port: self.destination_port,
            sequence_number: self.sequence_number,
            acknowledgement_number: self.acknowledgement_number,
            tcp_flags: self.tcp_flags,
        }
    }

    pub fn response_frame(
        self,
        tcp_payload: &[u8],
    ) -> Result<Vec<u8>, MmdsGuestTcpResponseFrameError> {
        self.response_context()
            .response_frame(tcp_payload, self.payload.len())
    }

    pub fn syn_ack_response_frame(self) -> Result<Vec<u8>, MmdsGuestTcpResponseFrameError> {
        if !self.is_initial_synchronization_request() {
            return Err(MmdsGuestTcpResponseFrameError::NotInitialSynchronizationRequest);
        }
        self.response_context().syn_ack_response_frame()
    }

    pub fn fin_close_response_frames(self) -> Result<[Vec<u8>; 2], MmdsGuestTcpResponseFrameError> {
        if !self.is_empty_fin_close_request() {
            return Err(MmdsGuestTcpResponseFrameError::NotConnectionCloseRequest);
        }
        self.response_context().fin_close_response_frames()
    }

    pub fn reset_response_frame(self) -> Result<Vec<u8>, MmdsGuestTcpResponseFrameError> {
        if !self.is_unsupported_empty_control_reset_request() {
            return Err(MmdsGuestTcpResponseFrameError::NotUnsupportedEmptyControlRequest);
        }
        self.response_context().reset_response_frame()
    }
}

impl MmdsGuestTcpResponseContext {
    pub fn response_frame(
        self,
        tcp_payload: &[u8],
        request_payload_len: usize,
    ) -> Result<Vec<u8>, MmdsGuestTcpResponseFrameError> {
        let request_payload_len = u32::try_from(request_payload_len).map_err(|_| {
            MmdsGuestTcpResponseFrameError::RequestPayloadTooLarge {
                request_payload_len,
            }
        })?;

        mmds_guest_tcp_response_frame(self, tcp_payload, request_payload_len)
    }

    fn syn_ack_response_frame(self) -> Result<Vec<u8>, MmdsGuestTcpResponseFrameError> {
        mmds_guest_tcp_response_frame_with_parts(
            self,
            MmdsGuestTcpResponseFrameParts {
                sequence_number: MMDS_GUEST_TCP_SYN_ACK_SEQUENCE_NUMBER,
                acknowledgement_number: self.sequence_number.wrapping_add(1),
                tcp_flags: TCP_FLAG_SYN | TCP_FLAG_ACK,
                tcp_payload: &[],
            },
        )
    }

    fn fin_close_response_frames(self) -> Result<[Vec<u8>; 2], MmdsGuestTcpResponseFrameError> {
        Ok([
            self.control_response_frame(TCP_FLAG_ACK)?,
            self.control_response_frame(TCP_FLAG_FIN | TCP_FLAG_ACK)?,
        ])
    }

    fn reset_response_frame(self) -> Result<Vec<u8>, MmdsGuestTcpResponseFrameError> {
        let parts = if self.tcp_flags & TCP_FLAG_ACK != 0 {
            MmdsGuestTcpResponseFrameParts {
                sequence_number: self.acknowledgement_number,
                acknowledgement_number: 0,
                tcp_flags: TCP_FLAG_RST,
                tcp_payload: &[],
            }
        } else {
            MmdsGuestTcpResponseFrameParts {
                sequence_number: 0,
                acknowledgement_number: self.sequence_number,
                tcp_flags: TCP_FLAG_RST | TCP_FLAG_ACK,
                tcp_payload: &[],
            }
        };

        mmds_guest_tcp_response_frame_with_parts(self, parts)
    }

    fn control_response_frame(
        self,
        tcp_flags: u8,
    ) -> Result<Vec<u8>, MmdsGuestTcpResponseFrameError> {
        mmds_guest_tcp_response_frame_with_parts(
            self,
            MmdsGuestTcpResponseFrameParts {
                sequence_number: self.acknowledgement_number,
                acknowledgement_number: self.response_acknowledgement_number(0),
                tcp_flags,
                tcp_payload: &[],
            },
        )
    }
}

pub fn classify_mmds_guest_arp_request(
    packet: &[u8],
    mmds_ipv4_address: Ipv4Addr,
) -> Option<MmdsGuestArpRequest> {
    let destination_ethernet_address =
        EthernetMacAddress::from_octets(packet_array::<ETHERNET_MAC_ADDRESS_LEN>(
            packet,
            ETHERNET_DESTINATION_ADDRESS_OFFSET,
        )?);
    let source_ethernet_address =
        EthernetMacAddress::from_octets(packet_array::<ETHERNET_MAC_ADDRESS_LEN>(
            packet,
            ETHERNET_SOURCE_ADDRESS_OFFSET,
        )?);
    let ethertype = packet_u16(packet, ETHERNET_ETHERTYPE_OFFSET)?;
    if ethertype != ETHERNET_ETHERTYPE_ARP {
        return None;
    }

    let arp_packet = packet
        .get(ETHERNET_HEADER_LEN..)?
        .get(..ARP_ETHERNET_IPV4_LEN)?;
    if packet_u16(arp_packet, ARP_HARDWARE_TYPE_OFFSET)? != ARP_HARDWARE_TYPE_ETHERNET
        || packet_u16(arp_packet, ARP_PROTOCOL_TYPE_OFFSET)? != ARP_PROTOCOL_TYPE_IPV4
        || *arp_packet.get(ARP_HARDWARE_ADDRESS_LEN_OFFSET)? != ARP_HARDWARE_ADDRESS_LEN_ETHERNET
        || *arp_packet.get(ARP_PROTOCOL_ADDRESS_LEN_OFFSET)? != ARP_PROTOCOL_ADDRESS_LEN_IPV4
        || packet_u16(arp_packet, ARP_OPERATION_OFFSET)? != ARP_OPERATION_REQUEST
    {
        return None;
    }

    let target_protocol_address =
        packet_ipv4_address(arp_packet, ARP_TARGET_PROTOCOL_ADDRESS_OFFSET)?;
    if target_protocol_address != mmds_ipv4_address {
        return None;
    }

    Some(MmdsGuestArpRequest {
        source_ethernet_address,
        destination_ethernet_address,
        sender_hardware_address: EthernetMacAddress::from_octets(packet_array::<
            ETHERNET_MAC_ADDRESS_LEN,
        >(
            arp_packet,
            ARP_SENDER_HARDWARE_ADDRESS_OFFSET,
        )?),
        sender_protocol_address: packet_ipv4_address(
            arp_packet,
            ARP_SENDER_PROTOCOL_ADDRESS_OFFSET,
        )?,
        target_protocol_address,
    })
}

pub fn classify_mmds_guest_tcp_packet(
    packet: &[u8],
    mmds_ipv4_address: Ipv4Addr,
) -> Option<MmdsGuestTcpPacket<'_>> {
    let destination_ethernet_address =
        EthernetMacAddress::from_octets(packet_array::<ETHERNET_MAC_ADDRESS_LEN>(
            packet,
            ETHERNET_DESTINATION_ADDRESS_OFFSET,
        )?);
    let source_ethernet_address =
        EthernetMacAddress::from_octets(packet_array::<ETHERNET_MAC_ADDRESS_LEN>(
            packet,
            ETHERNET_SOURCE_ADDRESS_OFFSET,
        )?);
    let ethertype = packet_u16(packet, ETHERNET_ETHERTYPE_OFFSET)?;
    if ethertype != ETHERNET_ETHERTYPE_IPV4 {
        return None;
    }

    let ipv4_packet = packet.get(ETHERNET_HEADER_LEN..)?;
    let version_ihl = *ipv4_packet.get(IPV4_VERSION_IHL_OFFSET)?;
    if version_ihl >> 4 != IPV4_VERSION {
        return None;
    }

    let ipv4_header_len = usize::from(version_ihl & 0x0f) * 4;
    if ipv4_header_len < IPV4_MIN_HEADER_LEN {
        return None;
    }

    let total_len = usize::from(packet_u16(ipv4_packet, IPV4_TOTAL_LENGTH_OFFSET)?);
    if total_len < ipv4_header_len.saturating_add(TCP_MIN_HEADER_LEN) {
        return None;
    }

    let ipv4_packet = ipv4_packet.get(..total_len)?;
    if *ipv4_packet.get(IPV4_PROTOCOL_OFFSET)? != IPV4_PROTOCOL_TCP {
        return None;
    }

    if packet_u16(ipv4_packet, IPV4_FLAGS_FRAGMENT_OFFSET_OFFSET)? & IPV4_REJECT_FLAGS_FRAGMENT_MASK
        != 0
    {
        return None;
    }

    let destination_ipv4_address =
        packet_ipv4_address(ipv4_packet, IPV4_DESTINATION_ADDRESS_OFFSET)?;
    if destination_ipv4_address != mmds_ipv4_address {
        return None;
    }

    let source_ipv4_address = packet_ipv4_address(ipv4_packet, IPV4_SOURCE_ADDRESS_OFFSET)?;
    let tcp_segment = ipv4_packet.get(ipv4_header_len..)?;
    let destination_port = packet_u16(tcp_segment, TCP_DESTINATION_PORT_OFFSET)?;
    if destination_port != MMDS_GUEST_TCP_PORT {
        return None;
    }

    let source_port = packet_u16(tcp_segment, TCP_SOURCE_PORT_OFFSET)?;
    let sequence_number = packet_u32(tcp_segment, TCP_SEQUENCE_NUMBER_OFFSET)?;
    let acknowledgement_number = packet_u32(tcp_segment, TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET)?;
    let tcp_data_offset = usize::from(*tcp_segment.get(TCP_DATA_OFFSET_OFFSET)? >> 4) * 4;
    if tcp_data_offset < TCP_MIN_HEADER_LEN {
        return None;
    }

    let tcp_flags = *tcp_segment.get(TCP_FLAGS_OFFSET)?;
    let payload = tcp_segment.get(tcp_data_offset..)?;
    Some(MmdsGuestTcpPacket {
        source_ethernet_address,
        destination_ethernet_address,
        source_ipv4_address,
        destination_ipv4_address,
        source_port,
        destination_port,
        sequence_number,
        acknowledgement_number,
        tcp_flags,
        payload,
    })
}

fn mmds_guest_arp_response_frame(
    request: MmdsGuestArpRequest,
    mmds_mac_address: EthernetMacAddress,
) -> Result<Vec<u8>, MmdsGuestArpResponseFrameError> {
    let mut frame = Vec::with_capacity(ETHERNET_HEADER_LEN + ARP_ETHERNET_IPV4_LEN);
    frame.extend_from_slice(&request.sender_hardware_address.octets());
    frame.extend_from_slice(&mmds_mac_address.octets());
    frame.extend_from_slice(&ETHERNET_ETHERTYPE_ARP.to_be_bytes());
    frame.extend_from_slice(&ARP_HARDWARE_TYPE_ETHERNET.to_be_bytes());
    frame.extend_from_slice(&ARP_PROTOCOL_TYPE_IPV4.to_be_bytes());
    frame.push(ARP_HARDWARE_ADDRESS_LEN_ETHERNET);
    frame.push(ARP_PROTOCOL_ADDRESS_LEN_IPV4);
    frame.extend_from_slice(&ARP_OPERATION_REPLY.to_be_bytes());
    frame.extend_from_slice(&mmds_mac_address.octets());
    frame.extend_from_slice(&request.target_protocol_address.octets());
    frame.extend_from_slice(&request.sender_hardware_address.octets());
    frame.extend_from_slice(&request.sender_protocol_address.octets());

    if frame.len() == ETHERNET_HEADER_LEN + ARP_ETHERNET_IPV4_LEN {
        return Ok(frame);
    }
    Err(MmdsGuestArpResponseFrameError::InternalFrameLayout)
}

fn mmds_guest_tcp_response_frame(
    request: MmdsGuestTcpResponseContext,
    tcp_payload: &[u8],
    request_payload_len: u32,
) -> Result<Vec<u8>, MmdsGuestTcpResponseFrameError> {
    mmds_guest_tcp_response_frame_with_parts(
        request,
        MmdsGuestTcpResponseFrameParts {
            sequence_number: request.acknowledgement_number,
            acknowledgement_number: request.response_acknowledgement_number(request_payload_len),
            tcp_flags: TCP_FLAG_PSH | TCP_FLAG_ACK,
            tcp_payload,
        },
    )
}

struct MmdsGuestTcpResponseFrameParts<'a> {
    sequence_number: u32,
    acknowledgement_number: u32,
    tcp_flags: u8,
    tcp_payload: &'a [u8],
}

fn mmds_guest_tcp_response_frame_with_parts(
    request: MmdsGuestTcpResponseContext,
    parts: MmdsGuestTcpResponseFrameParts<'_>,
) -> Result<Vec<u8>, MmdsGuestTcpResponseFrameError> {
    let ipv4_total_len = IPV4_MIN_HEADER_LEN
        .checked_add(TCP_MIN_HEADER_LEN)
        .and_then(|len| len.checked_add(parts.tcp_payload.len()))
        .filter(|len| *len <= IPV4_MAX_TOTAL_LENGTH)
        .ok_or(MmdsGuestTcpResponseFrameError::PayloadTooLarge {
            payload_len: parts.tcp_payload.len(),
        })?;
    let ipv4_total_len = u16::try_from(ipv4_total_len).map_err(|_| {
        MmdsGuestTcpResponseFrameError::PayloadTooLarge {
            payload_len: parts.tcp_payload.len(),
        }
    })?;
    let tcp_segment_len = TCP_MIN_HEADER_LEN
        .checked_add(parts.tcp_payload.len())
        .and_then(|len| u16::try_from(len).ok())
        .ok_or(MmdsGuestTcpResponseFrameError::PayloadTooLarge {
            payload_len: parts.tcp_payload.len(),
        })?;
    let mut frame = Vec::with_capacity(ETHERNET_HEADER_LEN + usize::from(ipv4_total_len));
    frame.extend_from_slice(&request.source_ethernet_address.octets());
    frame.extend_from_slice(&request.destination_ethernet_address.octets());
    frame.extend_from_slice(&ETHERNET_ETHERTYPE_IPV4.to_be_bytes());

    let mut ipv4_header = Vec::with_capacity(IPV4_MIN_HEADER_LEN);
    ipv4_header.push((IPV4_VERSION << 4) | IPV4_MIN_HEADER_WORDS);
    ipv4_header.push(0);
    ipv4_header.extend_from_slice(&ipv4_total_len.to_be_bytes());
    ipv4_header.extend_from_slice(&0_u16.to_be_bytes());
    ipv4_header.extend_from_slice(&0_u16.to_be_bytes());
    ipv4_header.push(64);
    ipv4_header.push(IPV4_PROTOCOL_TCP);
    ipv4_header.extend_from_slice(&0_u16.to_be_bytes());
    ipv4_header.extend_from_slice(&request.destination_ipv4_address.octets());
    ipv4_header.extend_from_slice(&request.source_ipv4_address.octets());
    let ipv4_checksum = internet_checksum(&ipv4_header);
    if !packet_write_u16(&mut ipv4_header, IPV4_HEADER_CHECKSUM_OFFSET, ipv4_checksum) {
        return Err(MmdsGuestTcpResponseFrameError::InternalFrameLayout);
    }
    frame.extend_from_slice(&ipv4_header);

    let mut tcp_segment = Vec::with_capacity(TCP_MIN_HEADER_LEN + parts.tcp_payload.len());
    tcp_segment.extend_from_slice(&request.destination_port.to_be_bytes());
    tcp_segment.extend_from_slice(&request.source_port.to_be_bytes());
    tcp_segment.extend_from_slice(&parts.sequence_number.to_be_bytes());
    tcp_segment.extend_from_slice(&parts.acknowledgement_number.to_be_bytes());
    tcp_segment.push(TCP_MIN_HEADER_WORDS << 4);
    tcp_segment.push(parts.tcp_flags);
    tcp_segment.extend_from_slice(&4096_u16.to_be_bytes());
    tcp_segment.extend_from_slice(&0_u16.to_be_bytes());
    tcp_segment.extend_from_slice(&0_u16.to_be_bytes());
    tcp_segment.extend_from_slice(parts.tcp_payload);
    let tcp_checksum = tcp_ipv4_checksum(
        request.destination_ipv4_address,
        request.source_ipv4_address,
        tcp_segment_len,
        &tcp_segment,
    );
    if !packet_write_u16(&mut tcp_segment, TCP_CHECKSUM_OFFSET, tcp_checksum) {
        return Err(MmdsGuestTcpResponseFrameError::InternalFrameLayout);
    }
    frame.extend_from_slice(&tcp_segment);

    Ok(frame)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsGuestArpResponseFrameError {
    InternalFrameLayout,
}

impl fmt::Display for MmdsGuestArpResponseFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InternalFrameLayout => {
                f.write_str("MMDS guest ARP response frame internal layout is invalid")
            }
        }
    }
}

impl std::error::Error for MmdsGuestArpResponseFrameError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsGuestTcpResponseFrameError {
    InternalFrameLayout,
    NotConnectionCloseRequest,
    NotInitialSynchronizationRequest,
    NotUnsupportedEmptyControlRequest,
    PayloadTooLarge { payload_len: usize },
    RequestPayloadTooLarge { request_payload_len: usize },
}

impl fmt::Display for MmdsGuestTcpResponseFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InternalFrameLayout => {
                f.write_str("MMDS guest TCP response frame internal layout is invalid")
            }
            Self::NotConnectionCloseRequest => f.write_str(
                "MMDS guest TCP FIN close response requires an empty FIN request packet",
            ),
            Self::NotInitialSynchronizationRequest => f.write_str(
                "MMDS guest TCP SYN-ACK response requires an initial SYN request packet",
            ),
            Self::NotUnsupportedEmptyControlRequest => f.write_str(
                "MMDS guest TCP RST response requires an unsupported empty control packet",
            ),
            Self::PayloadTooLarge { payload_len } => write!(
                f,
                "MMDS guest TCP response payload length {payload_len} exceeds IPv4 frame capacity"
            ),
            Self::RequestPayloadTooLarge {
                request_payload_len,
            } => write!(
                f,
                "MMDS guest TCP request payload length {request_payload_len} exceeds TCP acknowledgement capacity"
            ),
        }
    }
}

impl std::error::Error for MmdsGuestTcpResponseFrameError {}

impl MmdsGuestTcpResponseContext {
    fn response_acknowledgement_number(self, request_payload_len: u32) -> u32 {
        let mut acknowledgement = self.sequence_number.wrapping_add(request_payload_len);
        if self.tcp_flags & TCP_FLAG_SYN != 0 {
            acknowledgement = acknowledgement.wrapping_add(1);
        }
        if self.tcp_flags & TCP_FLAG_FIN != 0 {
            acknowledgement = acknowledgement.wrapping_add(1);
        }
        acknowledgement
    }
}

fn packet_ipv4_address(packet: &[u8], offset: usize) -> Option<Ipv4Addr> {
    Some(Ipv4Addr::from(packet_array::<4>(packet, offset)?))
}

fn packet_u16(packet: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(packet_array::<2>(packet, offset)?))
}

fn packet_u32(packet: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(packet_array::<4>(packet, offset)?))
}

fn packet_array<const N: usize>(packet: &[u8], offset: usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    packet.get(offset..end)?.try_into().ok()
}

fn packet_write_u16(packet: &mut [u8], offset: usize, value: u16) -> bool {
    let end = offset.saturating_add(2);
    if let Some(destination) = packet.get_mut(offset..end) {
        destination.copy_from_slice(&value.to_be_bytes());
        return true;
    }
    false
}

fn tcp_ipv4_checksum(
    source_ipv4_address: Ipv4Addr,
    destination_ipv4_address: Ipv4Addr,
    tcp_segment_len: u16,
    tcp_segment: &[u8],
) -> u16 {
    let mut sum = 0_u32;
    sum = checksum_add_bytes(sum, &source_ipv4_address.octets());
    sum = checksum_add_bytes(sum, &destination_ipv4_address.octets());
    sum = checksum_add_bytes(sum, &[0, IPV4_PROTOCOL_TCP]);
    sum = checksum_add_bytes(sum, &tcp_segment_len.to_be_bytes());
    sum = checksum_add_bytes(sum, tcp_segment);
    checksum_finish(sum)
}

fn internet_checksum(bytes: &[u8]) -> u16 {
    checksum_finish(checksum_add_bytes(0, bytes))
}

fn checksum_add_bytes(mut sum: u32, bytes: &[u8]) -> u32 {
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        if let Ok(pair) = <[u8; 2]>::try_from(chunk) {
            sum = sum.wrapping_add(u32::from(u16::from_be_bytes(pair)));
        }
    }
    if let Some(byte) = chunks.remainder().first() {
        sum = sum.wrapping_add(u32::from(*byte) << 8);
    }
    sum
}

fn checksum_finish(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff).wrapping_add(sum >> 16);
    }
    !(sum as u16)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsContentInput {
    value: Value,
}

impl MmdsContentInput {
    pub fn new(value: Value) -> Self {
        Self { value }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsConfigInput {
    network_interfaces: Vec<String>,
    version: MmdsVersion,
    ipv4_address: Option<Ipv4Addr>,
    imds_compat: bool,
}

impl MmdsConfigInput {
    pub fn new(network_interfaces: impl Into<Vec<String>>) -> Self {
        Self {
            network_interfaces: network_interfaces.into(),
            version: MmdsVersion::V1,
            ipv4_address: None,
            imds_compat: false,
        }
    }

    pub fn network_interfaces(&self) -> &[String] {
        &self.network_interfaces
    }

    pub const fn version(&self) -> MmdsVersion {
        self.version
    }

    pub const fn ipv4_address(&self) -> Option<Ipv4Addr> {
        self.ipv4_address
    }

    pub const fn imds_compat(&self) -> bool {
        self.imds_compat
    }

    pub const fn with_version(mut self, version: MmdsVersion) -> Self {
        self.version = version;
        self
    }

    pub const fn with_ipv4_address(mut self, ipv4_address: Ipv4Addr) -> Self {
        self.ipv4_address = Some(ipv4_address);
        self
    }

    pub const fn with_imds_compat(mut self, imds_compat: bool) -> Self {
        self.imds_compat = imds_compat;
        self
    }

    pub fn validate(
        self,
        configured_network_interfaces: &[NetworkInterfaceConfig],
    ) -> Result<MmdsConfig, MmdsConfigError> {
        if self.network_interfaces.is_empty() {
            return Err(MmdsConfigError::EmptyNetworkInterfaceList);
        }

        if let Some(ipv4_address) = self.ipv4_address
            && !is_valid_link_local_ipv4(ipv4_address)
        {
            return Err(MmdsConfigError::InvalidIpv4Address(ipv4_address));
        }

        for iface_id in &self.network_interfaces {
            if !configured_network_interfaces
                .iter()
                .any(|config| config.iface_id() == iface_id)
            {
                return Err(MmdsConfigError::UnknownNetworkInterfaceId {
                    iface_id: iface_id.clone(),
                });
            }
        }

        Ok(MmdsConfig {
            network_interfaces: self.network_interfaces,
            version: self.version,
            ipv4_address: self.ipv4_address,
            imds_compat: self.imds_compat,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsVersion {
    V1,
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsConfig {
    network_interfaces: Vec<String>,
    version: MmdsVersion,
    ipv4_address: Option<Ipv4Addr>,
    imds_compat: bool,
}

impl MmdsConfig {
    pub fn network_interfaces(&self) -> &[String] {
        &self.network_interfaces
    }

    pub const fn version(&self) -> MmdsVersion {
        self.version
    }

    pub const fn ipv4_address(&self) -> Option<Ipv4Addr> {
        self.ipv4_address
    }

    pub fn effective_ipv4_address(&self) -> Ipv4Addr {
        self.ipv4_address.unwrap_or(DEFAULT_MMDS_IPV4_ADDRESS)
    }

    pub const fn imds_compat(&self) -> bool {
        self.imds_compat
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsConfigError {
    EmptyNetworkInterfaceList,
    InvalidIpv4Address(Ipv4Addr),
    UnknownNetworkInterfaceId { iface_id: String },
}

impl fmt::Display for MmdsConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNetworkInterfaceList => {
                f.write_str("MMDS network_interfaces must not be empty")
            }
            Self::InvalidIpv4Address(ipv4_address) => {
                write!(
                    f,
                    "MMDS ipv4_address must be a usable RFC 3927 link-local address: {ipv4_address}"
                )
            }
            Self::UnknownNetworkInterfaceId { iface_id } => {
                write!(f, "MMDS network interface id is not configured: {iface_id}")
            }
        }
    }
}

impl std::error::Error for MmdsConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsOutputFormat {
    Json,
    Imds,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MmdsGuestHttpVersion {
    Http10,
    #[default]
    Http11,
}

impl MmdsGuestHttpVersion {
    fn parse(version: &str) -> Result<Self, MmdsGuestRequestParseError> {
        match version {
            "HTTP/1.0" => Ok(Self::Http10),
            "HTTP/1.1" => Ok(Self::Http11),
            _ => Err(MmdsGuestRequestParseError::UnsupportedHttpVersion),
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsGuestRequest {
    Get(MmdsGuestGetRequest),
    TokenPut(MmdsGuestTokenPutRequest),
}

impl MmdsGuestRequest {
    pub fn uri(&self) -> &str {
        match self {
            Self::Get(request) => request.uri(),
            Self::TokenPut(request) => request.uri(),
        }
    }

    pub const fn http_version(&self) -> MmdsGuestHttpVersion {
        match self {
            Self::Get(request) => request.http_version(),
            Self::TokenPut(request) => request.http_version(),
        }
    }

    pub fn parse_http(bytes: &[u8]) -> Result<Self, MmdsGuestRequestParseError> {
        Self::parse_http_with_version(bytes).map_err(MmdsGuestRequestParseFailure::into_error)
    }

    fn parse_http_with_version(bytes: &[u8]) -> Result<Self, MmdsGuestRequestParseFailure> {
        let request = str::from_utf8(bytes).map_err(|_| {
            MmdsGuestRequestParseFailure::without_version(MmdsGuestRequestParseError::InvalidUtf8)
        })?;
        let (head, body) = request
            .split_once("\r\n\r\n")
            .ok_or(MmdsGuestRequestParseError::MalformedRequest)
            .map_err(MmdsGuestRequestParseFailure::without_version)?;
        let mut lines = head.split("\r\n");
        let request_line = lines
            .next()
            .ok_or(MmdsGuestRequestParseError::MalformedRequest)
            .map_err(MmdsGuestRequestParseFailure::without_version)?;
        let (method, uri, version) = parse_guest_request_line(request_line)
            .map_err(MmdsGuestRequestParseFailure::without_version)?;
        let http_version = MmdsGuestHttpVersion::parse(version);
        let method = MmdsGuestRequestMethod::parse(method).map_err(|err| {
            if let Ok(http_version) = http_version {
                MmdsGuestRequestParseFailure::with_version(http_version, err)
            } else {
                MmdsGuestRequestParseFailure::without_version(err)
            }
        })?;
        let http_version = http_version.map_err(MmdsGuestRequestParseFailure::without_version)?;

        let uri = guest_request_uri_path(uri)
            .map_err(|err| MmdsGuestRequestParseFailure::with_version(http_version, err))?;
        let mut content_length = None;
        let mut output_format = MmdsOutputFormat::Imds;
        let mut token = MmdsGuestToken::Missing;
        let mut token_ttl = MmdsGuestTokenTtl::Missing;
        let mut forwarded_for = false;

        for line in lines {
            let (name, value) = parse_guest_request_header(line)
                .map_err(|err| MmdsGuestRequestParseFailure::with_version(http_version, err))?;
            if name.eq_ignore_ascii_case("Content-Length") {
                if content_length.is_some() {
                    return Err(MmdsGuestRequestParseFailure::with_version(
                        http_version,
                        MmdsGuestRequestParseError::DuplicateContentLength,
                    ));
                }
                content_length = Some(parse_guest_content_length(value).map_err(|err| {
                    MmdsGuestRequestParseFailure::with_version(http_version, err)
                })?);
            } else if name.eq_ignore_ascii_case("Transfer-Encoding") {
                return Err(MmdsGuestRequestParseFailure::with_version(
                    http_version,
                    MmdsGuestRequestParseError::UnsupportedTransferEncoding,
                ));
            } else if method == MmdsGuestRequestMethod::Get && name.eq_ignore_ascii_case("Accept") {
                output_format = parse_guest_accept_header(value)
                    .map_err(|err| MmdsGuestRequestParseFailure::with_version(http_version, err))?;
            } else if method == MmdsGuestRequestMethod::Get {
                if let Some(header) = MmdsGuestTokenHeader::parse_name(name) {
                    token = match token {
                        MmdsGuestToken::Missing => MmdsGuestToken::Header {
                            token_header: header,
                            token_value: value.to_string(),
                        },
                        MmdsGuestToken::Header { .. } | MmdsGuestToken::Duplicate => {
                            MmdsGuestToken::Duplicate
                        }
                    };
                }
            } else if method == MmdsGuestRequestMethod::Put {
                if name.eq_ignore_ascii_case(MMDS_GUEST_X_FORWARDED_FOR) {
                    forwarded_for = true;
                } else if let Some(header) = MmdsGuestTokenTtlHeader::parse_name(name) {
                    token_ttl = match token_ttl {
                        MmdsGuestTokenTtl::Missing => MmdsGuestTokenTtl::Header {
                            ttl_header: header,
                            ttl_value: value.to_string(),
                        },
                        MmdsGuestTokenTtl::Header { .. } | MmdsGuestTokenTtl::Duplicate => {
                            MmdsGuestTokenTtl::Duplicate
                        }
                    };
                }
            }
        }

        let content_length = content_length.unwrap_or(0);
        if content_length != 0 || !body.is_empty() {
            return Err(MmdsGuestRequestParseFailure::with_version(
                http_version,
                MmdsGuestRequestParseError::UnsupportedBody,
            ));
        }

        match method {
            MmdsGuestRequestMethod::Get => Ok(Self::Get(MmdsGuestGetRequest {
                http_version,
                uri: uri.to_string(),
                output_format,
                token,
            })),
            MmdsGuestRequestMethod::Put => {
                if forwarded_for {
                    return Err(MmdsGuestRequestParseFailure::with_version(
                        http_version,
                        MmdsGuestRequestParseError::UnsupportedForwardedFor,
                    ));
                }

                Ok(Self::TokenPut(MmdsGuestTokenPutRequest {
                    http_version,
                    uri: uri.to_string(),
                    token_ttl,
                }))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MmdsGuestRequestParseFailure {
    error: MmdsGuestRequestParseError,
    http_version: Option<MmdsGuestHttpVersion>,
}

impl MmdsGuestRequestParseFailure {
    const fn without_version(error: MmdsGuestRequestParseError) -> Self {
        Self {
            error,
            http_version: None,
        }
    }

    const fn with_version(
        http_version: MmdsGuestHttpVersion,
        error: MmdsGuestRequestParseError,
    ) -> Self {
        Self {
            error,
            http_version: Some(http_version),
        }
    }

    const fn into_error(self) -> MmdsGuestRequestParseError {
        self.error
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsGuestGetRequest {
    http_version: MmdsGuestHttpVersion,
    uri: String,
    output_format: MmdsOutputFormat,
    token: MmdsGuestToken,
}

impl MmdsGuestGetRequest {
    pub const fn http_version(&self) -> MmdsGuestHttpVersion {
        self.http_version
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub const fn output_format(&self) -> MmdsOutputFormat {
        self.output_format
    }

    pub fn token(&self) -> &MmdsGuestToken {
        &self.token
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsGuestTokenPutRequest {
    http_version: MmdsGuestHttpVersion,
    uri: String,
    token_ttl: MmdsGuestTokenTtl,
}

impl MmdsGuestTokenPutRequest {
    pub const fn http_version(&self) -> MmdsGuestHttpVersion {
        self.http_version
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn token_ttl(&self) -> &MmdsGuestTokenTtl {
        &self.token_ttl
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsGuestTokenTtl {
    Missing,
    Header {
        ttl_header: MmdsGuestTokenTtlHeader,
        ttl_value: String,
    },
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsGuestToken {
    Missing,
    Header {
        token_header: MmdsGuestTokenHeader,
        token_value: String,
    },
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestTokenHeader {
    Metadata,
    AwsEc2Metadata,
}

impl MmdsGuestTokenHeader {
    fn parse_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case(MMDS_GUEST_X_METADATA_TOKEN) {
            return Some(Self::Metadata);
        }
        if name.eq_ignore_ascii_case(MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN) {
            return Some(Self::AwsEc2Metadata);
        }

        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestTokenTtlHeader {
    Metadata,
    AwsEc2Metadata,
}

impl MmdsGuestTokenTtlHeader {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Metadata => MMDS_GUEST_X_METADATA_TOKEN_TTL_SECONDS,
            Self::AwsEc2Metadata => MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN_TTL_SECONDS,
        }
    }

    fn parse_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case(MMDS_GUEST_X_METADATA_TOKEN_TTL_SECONDS) {
            return Some(Self::Metadata);
        }
        if name.eq_ignore_ascii_case(MMDS_GUEST_X_AWS_EC2_METADATA_TOKEN_TTL_SECONDS) {
            return Some(Self::AwsEc2Metadata);
        }

        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MmdsGuestRequestMethod {
    Get,
    Put,
}

impl MmdsGuestRequestMethod {
    fn parse(method: &str) -> Result<Self, MmdsGuestRequestParseError> {
        match method {
            "GET" => Ok(Self::Get),
            "PUT" => Ok(Self::Put),
            _ => Err(MmdsGuestRequestParseError::UnsupportedMethod),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestRequestParseError {
    InvalidUtf8,
    MalformedRequest,
    UnsupportedMethod,
    UnsupportedHttpVersion,
    InvalidUri,
    MalformedHeader,
    DuplicateContentLength,
    InvalidContentLength,
    UnsupportedTransferEncoding,
    UnsupportedBody,
    UnsupportedAccept,
    MissingToken,
    InvalidToken,
    MissingTokenTtl,
    InvalidTokenTtl,
    DuplicateTokenTtl,
    UnsupportedForwardedFor,
}

impl fmt::Display for MmdsGuestRequestParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 => f.write_str("MMDS guest HTTP request is not valid UTF-8."),
            Self::MalformedRequest => f.write_str("MMDS guest HTTP request is malformed."),
            Self::UnsupportedMethod => {
                f.write_str("MMDS guest HTTP request method is not supported.")
            }
            Self::UnsupportedHttpVersion => {
                f.write_str("MMDS guest HTTP request version is not supported.")
            }
            Self::InvalidUri => f.write_str("Invalid URI."),
            Self::MalformedHeader => f.write_str("MMDS guest HTTP request header is malformed."),
            Self::DuplicateContentLength => {
                f.write_str("MMDS guest HTTP request has duplicate Content-Length headers.")
            }
            Self::InvalidContentLength => {
                f.write_str("MMDS guest HTTP request Content-Length is invalid.")
            }
            Self::UnsupportedTransferEncoding => {
                f.write_str("MMDS guest HTTP request Transfer-Encoding is not supported.")
            }
            Self::UnsupportedBody => f.write_str("MMDS guest HTTP request body is not supported."),
            Self::UnsupportedAccept => {
                f.write_str("MMDS guest HTTP request Accept header is not supported.")
            }
            Self::MissingToken => f.write_str(MMDS_GUEST_MISSING_TOKEN),
            Self::InvalidToken => f.write_str(MMDS_GUEST_INVALID_TOKEN),
            Self::MissingTokenTtl => f.write_str(
                "Token time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime.",
            ),
            Self::InvalidTokenTtl => {
                f.write_str("MMDS guest token TTL header value is invalid.")
            }
            Self::DuplicateTokenTtl => {
                f.write_str("MMDS guest token TTL header is duplicated.")
            }
            Self::UnsupportedForwardedFor => {
                f.write_str("MMDS guest token PUT request does not support X-Forwarded-For.")
            }
        }
    }
}

impl std::error::Error for MmdsGuestRequestParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestStatus {
    Ok,
    BadRequest,
    Unauthorized,
    NotFound,
    MethodNotAllowed,
    NotImplemented,
}

impl MmdsGuestStatus {
    pub const fn as_u16(&self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::NotImplemented => 501,
        }
    }

    pub const fn reason_phrase(&self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::BadRequest => "Bad Request",
            Self::Unauthorized => "Unauthorized",
            Self::NotFound => "Not Found",
            Self::MethodNotAllowed => "Method Not Allowed",
            Self::NotImplemented => "Not Implemented",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsGuestContentType {
    ApplicationJson,
    PlainText,
}

impl MmdsGuestContentType {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ApplicationJson => "application/json",
            Self::PlainText => "text/plain",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsGuestResponse {
    http_version: MmdsGuestHttpVersion,
    status: MmdsGuestStatus,
    content_type: MmdsGuestContentType,
    allow: Option<&'static str>,
    custom_headers: Vec<(&'static str, String)>,
    body: String,
}

impl MmdsGuestResponse {
    fn new(status: MmdsGuestStatus, content_type: MmdsGuestContentType, body: String) -> Self {
        Self {
            http_version: MmdsGuestHttpVersion::default(),
            status,
            content_type,
            allow: None,
            custom_headers: Vec::new(),
            body,
        }
    }

    fn with_allow_header(mut self, allow: &'static str) -> Self {
        self.allow = Some(allow);
        self
    }

    fn with_http_version(mut self, http_version: MmdsGuestHttpVersion) -> Self {
        self.http_version = http_version;
        self
    }

    fn with_custom_header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.custom_headers.push((name, value.into()));
        self
    }

    pub const fn status(&self) -> MmdsGuestStatus {
        self.status
    }

    pub const fn content_type(&self) -> MmdsGuestContentType {
        self.content_type
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn to_http_bytes(&self) -> Vec<u8> {
        let mut response = format!(
            "{} {} {}\r\nContent-Type: {}\r\n",
            self.http_version.as_str(),
            self.status.as_u16(),
            self.status.reason_phrase(),
            self.content_type.as_str(),
        );
        if let Some(allow) = self.allow {
            response.push_str("Allow: ");
            response.push_str(allow);
            response.push_str("\r\n");
        }
        for (name, value) in &self.custom_headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("Content-Length: ");
        response.push_str(&self.body.len().to_string());
        response.push_str("\r\n\r\n");
        response.push_str(&self.body);
        response.into_bytes()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsTokenError {
    InvalidTtl { ttl_seconds: u32 },
    ActiveTokenLimitExceeded { limit: usize },
    RandomnessUnavailable,
    TokenCollision,
}

impl fmt::Display for MmdsTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTtl { ttl_seconds } => write!(
                f,
                "Invalid MMDS token TTL: {ttl_seconds}. Please provide a value between {MMDS_TOKEN_MIN_TTL_SECONDS} and {MMDS_TOKEN_MAX_TTL_SECONDS}."
            ),
            Self::ActiveTokenLimitExceeded { limit } => {
                write!(f, "The MMDS active token limit was exceeded: {limit}.")
            }
            Self::RandomnessUnavailable => f.write_str("MMDS token randomness is unavailable."),
            Self::TokenCollision => f.write_str("MMDS token generation collided repeatedly."),
        }
    }
}

impl std::error::Error for MmdsTokenError {}

#[derive(Debug, Clone, Copy)]
enum MmdsTokenClock {
    System {
        origin: Instant,
    },
    #[cfg(test)]
    Manual {
        now_millis: u64,
    },
}

impl Default for MmdsTokenClock {
    fn default() -> Self {
        Self::System {
            origin: Instant::now(),
        }
    }
}

impl MmdsTokenClock {
    fn now_millis(&self) -> u64 {
        match self {
            Self::System { origin } => {
                u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
            }
            #[cfg(test)]
            Self::Manual { now_millis } => *now_millis,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MmdsTokenAuthority {
    tokens: HashMap<String, u64>,
    max_active_tokens: usize,
    clock: MmdsTokenClock,
}

impl PartialEq for MmdsTokenAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.tokens == other.tokens && self.max_active_tokens == other.max_active_tokens
    }
}

impl Eq for MmdsTokenAuthority {}

impl Default for MmdsTokenAuthority {
    fn default() -> Self {
        Self::new(MMDS_TOKEN_MAX_ACTIVE_TOKENS)
    }
}

impl MmdsTokenAuthority {
    pub fn new(max_active_tokens: usize) -> Self {
        Self {
            tokens: HashMap::new(),
            max_active_tokens,
            clock: MmdsTokenClock::default(),
        }
    }

    pub fn generate_token(&mut self, ttl_seconds: u32) -> Result<String, MmdsTokenError> {
        self.validate_ttl(ttl_seconds)?;

        let now_millis = self.clock.now_millis();
        self.remove_expired_tokens(now_millis);
        if self.tokens.len() >= self.max_active_tokens {
            return Err(MmdsTokenError::ActiveTokenLimitExceeded {
                limit: self.max_active_tokens,
            });
        }

        let expiry_millis = token_expiry_millis(now_millis, ttl_seconds);
        for _ in 0..MMDS_TOKEN_GENERATION_ATTEMPTS {
            let token = generate_opaque_token()?;
            if self.tokens.contains_key(&token) {
                continue;
            }

            self.tokens.insert(token.clone(), expiry_millis);
            return Ok(token);
        }

        Err(MmdsTokenError::TokenCollision)
    }

    pub fn is_valid(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }

        self.tokens
            .get(token)
            .is_some_and(|expiry_millis| *expiry_millis > self.clock.now_millis())
    }

    fn validate_ttl(&self, ttl_seconds: u32) -> Result<(), MmdsTokenError> {
        if (MMDS_TOKEN_MIN_TTL_SECONDS..=MMDS_TOKEN_MAX_TTL_SECONDS).contains(&ttl_seconds) {
            return Ok(());
        }

        Err(MmdsTokenError::InvalidTtl { ttl_seconds })
    }

    fn remove_expired_tokens(&mut self, now_millis: u64) {
        self.tokens
            .retain(|_, expiry_millis| *expiry_millis > now_millis);
    }

    #[cfg(test)]
    fn with_manual_clock(max_active_tokens: usize, now_millis: u64) -> Self {
        Self {
            tokens: HashMap::new(),
            max_active_tokens,
            clock: MmdsTokenClock::Manual { now_millis },
        }
    }

    #[cfg(test)]
    fn set_now_millis(&mut self, now_millis: u64) {
        self.clock = MmdsTokenClock::Manual { now_millis };
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsDataStoreError {
    InvalidObject,
    NotFound,
    NotInitialized,
    DataStoreLimitExceeded {
        limit_bytes: usize,
        size_bytes: usize,
    },
    Serialization,
    UnsupportedValueType,
}

impl fmt::Display for MmdsDataStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidObject => {
                f.write_str("The MMDS data store request body must be a JSON object.")
            }
            Self::NotFound => f.write_str("The MMDS resource does not exist."),
            Self::NotInitialized => f.write_str("The MMDS data store is not initialized."),
            Self::DataStoreLimitExceeded {
                limit_bytes,
                size_bytes,
            } => write!(
                f,
                "The MMDS data store size limit was exceeded: {size_bytes} bytes > {limit_bytes} bytes"
            ),
            Self::Serialization => f.write_str("The MMDS data store could not be serialized."),
            Self::UnsupportedValueType => {
                f.write_str("Cannot retrieve value. The value has an unsupported type.")
            }
        }
    }
}

impl std::error::Error for MmdsDataStoreError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdsState {
    config: Option<MmdsConfig>,
    value: Option<Value>,
    data_store_limit_bytes: usize,
    token_authority: MmdsTokenAuthority,
}

#[derive(Debug, Clone)]
pub struct MmdsStateHandle {
    state: Arc<Mutex<MmdsState>>,
}

impl Default for MmdsStateHandle {
    fn default() -> Self {
        Self::new(MmdsState::default())
    }
}

impl MmdsStateHandle {
    pub fn new(state: MmdsState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&MmdsState) -> R) -> Result<R, MmdsStateLockError> {
        let state = self.state.lock().map_err(|_| MmdsStateLockError)?;
        Ok(f(&state))
    }

    pub fn with_mut<R>(
        &self,
        f: impl FnOnce(&mut MmdsState) -> R,
    ) -> Result<R, MmdsStateLockError> {
        let mut state = self.state.lock().map_err(|_| MmdsStateLockError)?;
        Ok(f(&mut state))
    }

    pub fn config(&self) -> Result<Option<MmdsConfig>, MmdsStateLockError> {
        self.with(|state| state.config().cloned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmdsStateLockError;

impl fmt::Display for MmdsStateLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MMDS state lock is poisoned")
    }
}

impl std::error::Error for MmdsStateLockError {}

impl Default for MmdsState {
    fn default() -> Self {
        Self::new(MMDS_DATA_STORE_LIMIT_BYTES)
    }
}

impl MmdsState {
    pub fn new(data_store_limit_bytes: usize) -> Self {
        Self {
            config: None,
            value: None,
            data_store_limit_bytes,
            token_authority: MmdsTokenAuthority::default(),
        }
    }

    pub const fn data_store_limit_bytes(&self) -> usize {
        self.data_store_limit_bytes
    }

    pub fn config(&self) -> Option<&MmdsConfig> {
        self.config.as_ref()
    }

    pub fn put_config(
        &mut self,
        input: MmdsConfigInput,
        configured_network_interfaces: &[NetworkInterfaceConfig],
    ) -> Result<(), MmdsConfigError> {
        self.config = Some(input.validate(configured_network_interfaces)?);
        Ok(())
    }

    pub fn get_data(&self) -> Result<Value, MmdsDataStoreError> {
        self.value
            .as_ref()
            .cloned()
            .ok_or(MmdsDataStoreError::NotInitialized)
    }

    pub fn query_data(
        &self,
        path: &str,
        output_format: MmdsOutputFormat,
    ) -> Result<String, MmdsDataStoreError> {
        let value = self
            .value
            .as_ref()
            .ok_or(MmdsDataStoreError::NotInitialized)?;
        let pointer_path = mmds_pointer_path(path);
        let query_value = value
            .pointer(pointer_path)
            .ok_or(MmdsDataStoreError::NotFound)?;

        if self.config.as_ref().is_some_and(MmdsConfig::imds_compat) {
            return format_imds(query_value);
        }

        match output_format {
            MmdsOutputFormat::Json => Ok(query_value.to_string()),
            MmdsOutputFormat::Imds => format_imds(query_value),
        }
    }

    pub fn guest_get_response(
        &self,
        uri: &str,
        output_format: MmdsOutputFormat,
    ) -> MmdsGuestResponse {
        if uri.is_empty() {
            return MmdsGuestResponse::new(
                MmdsGuestStatus::BadRequest,
                MmdsGuestContentType::PlainText,
                "Invalid URI.".to_string(),
            );
        }

        let query_path = sanitize_guest_uri(uri);
        match self.query_data(&query_path, output_format) {
            Ok(body) => MmdsGuestResponse::new(
                MmdsGuestStatus::Ok,
                self.guest_success_content_type(output_format),
                body,
            ),
            Err(err) => guest_error_response(uri, err),
        }
    }

    pub fn guest_http_response(&mut self, request_bytes: &[u8]) -> MmdsGuestResponse {
        match MmdsGuestRequest::parse_http_with_version(request_bytes) {
            Ok(MmdsGuestRequest::Get(request)) => self
                .guest_get_http_response(&request)
                .with_http_version(request.http_version()),
            Ok(MmdsGuestRequest::TokenPut(request)) => self
                .guest_token_put_response(&request)
                .with_http_version(request.http_version()),
            Err(failure) => {
                let response = guest_request_parse_error_response(failure.error);
                if let Some(http_version) = failure.http_version {
                    response.with_http_version(http_version)
                } else {
                    response
                }
            }
        }
    }

    pub fn guest_http_response_bytes(&mut self, request_bytes: &[u8]) -> Vec<u8> {
        self.guest_http_response(request_bytes).to_http_bytes()
    }

    pub fn guest_tcp_packet_response_bytes(
        &mut self,
        packet: &[u8],
        mmds_ipv4_address: Ipv4Addr,
    ) -> Option<Vec<u8>> {
        let packet = classify_mmds_guest_tcp_packet(packet, mmds_ipv4_address)?;
        if packet.payload().is_empty() {
            return None;
        }

        Some(self.guest_http_response_bytes(packet.payload()))
    }

    pub fn generate_guest_token(&mut self, ttl_seconds: u32) -> Result<String, MmdsTokenError> {
        self.token_authority.generate_token(ttl_seconds)
    }

    pub fn is_guest_token_valid(&self, token: &str) -> bool {
        self.token_authority.is_valid(token)
    }

    pub fn put_data(&mut self, input: MmdsContentInput) -> Result<(), MmdsDataStoreError> {
        let value = input.into_value();
        validate_object(&value)?;
        self.ensure_within_limit(&value)?;
        self.value = Some(value);
        Ok(())
    }

    pub fn patch_data(&mut self, input: MmdsContentInput) -> Result<(), MmdsDataStoreError> {
        let value = self
            .value
            .as_ref()
            .ok_or(MmdsDataStoreError::NotInitialized)?;
        validate_object(input.value())?;
        let mut patched = value.clone();
        json_merge_patch(&mut patched, input.value());
        self.ensure_within_limit(&patched)?;
        self.value = Some(patched);
        Ok(())
    }

    fn ensure_within_limit(&self, value: &Value) -> Result<(), MmdsDataStoreError> {
        let size_bytes = serde_json::to_vec(value)
            .map_err(|_| MmdsDataStoreError::Serialization)?
            .len();
        if size_bytes > self.data_store_limit_bytes {
            return Err(MmdsDataStoreError::DataStoreLimitExceeded {
                limit_bytes: self.data_store_limit_bytes,
                size_bytes,
            });
        }

        Ok(())
    }

    fn guest_success_content_type(&self, output_format: MmdsOutputFormat) -> MmdsGuestContentType {
        if self.config.as_ref().is_some_and(MmdsConfig::imds_compat) {
            return MmdsGuestContentType::PlainText;
        }

        match output_format {
            MmdsOutputFormat::Json => MmdsGuestContentType::ApplicationJson,
            MmdsOutputFormat::Imds => MmdsGuestContentType::PlainText,
        }
    }

    fn guest_mmds_version(&self) -> MmdsVersion {
        self.config
            .as_ref()
            .map_or(MmdsVersion::V1, MmdsConfig::version)
    }

    fn guest_get_http_response(&self, request: &MmdsGuestGetRequest) -> MmdsGuestResponse {
        if self.guest_mmds_version() == MmdsVersion::V2 {
            match request.token() {
                MmdsGuestToken::Missing => {
                    return guest_request_parse_error_response(
                        MmdsGuestRequestParseError::MissingToken,
                    );
                }
                MmdsGuestToken::Header { token_value, .. }
                    if self.is_guest_token_valid(token_value) => {}
                MmdsGuestToken::Header { .. } | MmdsGuestToken::Duplicate => {
                    return guest_request_parse_error_response(
                        MmdsGuestRequestParseError::InvalidToken,
                    );
                }
            }
        }

        self.guest_get_response(request.uri(), request.output_format())
    }

    fn guest_token_put_response(
        &mut self,
        request: &MmdsGuestTokenPutRequest,
    ) -> MmdsGuestResponse {
        if sanitize_guest_uri(request.uri()) != MMDS_GUEST_TOKEN_PATH {
            return MmdsGuestResponse::new(
                MmdsGuestStatus::NotFound,
                MmdsGuestContentType::PlainText,
                format!("Resource not found: {}.", request.uri()),
            );
        }

        let (ttl_header, ttl_value) = match request.token_ttl() {
            MmdsGuestTokenTtl::Missing => {
                return guest_request_parse_error_response(
                    MmdsGuestRequestParseError::MissingTokenTtl,
                );
            }
            MmdsGuestTokenTtl::Header {
                ttl_header,
                ttl_value,
            } => (*ttl_header, ttl_value.as_str()),
            MmdsGuestTokenTtl::Duplicate => {
                return guest_request_parse_error_response(
                    MmdsGuestRequestParseError::DuplicateTokenTtl,
                );
            }
        };
        let ttl_seconds = match parse_guest_token_ttl(ttl_value) {
            Ok(ttl_seconds) => ttl_seconds,
            Err(err) => {
                return guest_request_parse_error_response(err);
            }
        };

        match self.generate_guest_token(ttl_seconds) {
            Ok(token) => {
                MmdsGuestResponse::new(MmdsGuestStatus::Ok, MmdsGuestContentType::PlainText, token)
                    .with_custom_header(ttl_header.name(), ttl_seconds.to_string())
            }
            Err(err) => MmdsGuestResponse::new(
                MmdsGuestStatus::BadRequest,
                MmdsGuestContentType::PlainText,
                err.to_string(),
            ),
        }
    }
}

fn token_expiry_millis(now_millis: u64, ttl_seconds: u32) -> u64 {
    now_millis.saturating_add(u64::from(ttl_seconds) * MMDS_MILLISECONDS_PER_SECOND)
}

fn generate_opaque_token() -> Result<String, MmdsTokenError> {
    let mut bytes = [0_u8; MMDS_TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| MmdsTokenError::RandomnessUnavailable)?;

    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }

    output
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        10..=15 => char::from(b'a' + (nibble - 10)),
        _ => '?',
    }
}

fn mmds_pointer_path(path: &str) -> &str {
    path.strip_suffix('/').unwrap_or(path)
}

fn parse_guest_request_line(
    request_line: &str,
) -> Result<(&str, &str, &str), MmdsGuestRequestParseError> {
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts
        .next()
        .ok_or(MmdsGuestRequestParseError::MalformedRequest)?;
    let uri = parts
        .next()
        .ok_or(MmdsGuestRequestParseError::MalformedRequest)?;
    let version = parts
        .next()
        .ok_or(MmdsGuestRequestParseError::MalformedRequest)?;
    if parts.next().is_some() {
        return Err(MmdsGuestRequestParseError::MalformedRequest);
    }

    Ok((method, uri, version))
}

fn guest_request_uri_path(uri: &str) -> Result<&str, MmdsGuestRequestParseError> {
    if uri.is_empty() {
        return Err(MmdsGuestRequestParseError::InvalidUri);
    }
    if uri.starts_with('/') {
        return Ok(uri);
    }
    if let Some(rest) = uri.strip_prefix("http://") {
        let Some(path_start) = rest.find('/') else {
            return Err(MmdsGuestRequestParseError::InvalidUri);
        };
        if path_start == 0 {
            return Err(MmdsGuestRequestParseError::InvalidUri);
        }
        let path = rest
            .get(path_start..)
            .ok_or(MmdsGuestRequestParseError::InvalidUri)?;
        if path.is_empty() {
            return Err(MmdsGuestRequestParseError::InvalidUri);
        }
        return Ok(path);
    }

    Err(MmdsGuestRequestParseError::InvalidUri)
}

fn parse_guest_request_header(line: &str) -> Result<(&str, &str), MmdsGuestRequestParseError> {
    let (name, value) = line
        .split_once(':')
        .ok_or(MmdsGuestRequestParseError::MalformedHeader)?;
    if !is_http_token(name) {
        return Err(MmdsGuestRequestParseError::MalformedHeader);
    }

    Ok((name, trim_http_optional_whitespace(value)))
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'a'..=b'z'
            )
        })
}

fn parse_guest_content_length(value: &str) -> Result<usize, MmdsGuestRequestParseError> {
    if value.is_empty() {
        return Err(MmdsGuestRequestParseError::InvalidContentLength);
    }

    let mut parsed = 0usize;
    for byte in value.bytes() {
        if !byte.is_ascii_digit() {
            return Err(MmdsGuestRequestParseError::InvalidContentLength);
        }

        parsed = parsed
            .checked_mul(10)
            .and_then(|parsed| parsed.checked_add(usize::from(byte - b'0')))
            .ok_or(MmdsGuestRequestParseError::InvalidContentLength)?;
    }

    Ok(parsed)
}

fn parse_guest_token_ttl(value: &str) -> Result<u32, MmdsGuestRequestParseError> {
    value
        .parse::<u32>()
        .map_err(|_| MmdsGuestRequestParseError::InvalidTokenTtl)
}

fn parse_guest_accept_header(value: &str) -> Result<MmdsOutputFormat, MmdsGuestRequestParseError> {
    if value.is_empty() || value == "*/*" || value.eq_ignore_ascii_case("text/plain") {
        return Ok(MmdsOutputFormat::Imds);
    }
    if value.eq_ignore_ascii_case("application/json") {
        return Ok(MmdsOutputFormat::Json);
    }

    Err(MmdsGuestRequestParseError::UnsupportedAccept)
}

fn trim_http_optional_whitespace(value: &str) -> &str {
    value.trim_matches(|character| matches!(character, ' ' | '\t'))
}

fn sanitize_guest_uri(uri: &str) -> String {
    let mut sanitized = String::with_capacity(uri.len());
    let mut last_was_slash = false;

    for character in uri.chars() {
        if character == '/' {
            if !last_was_slash {
                sanitized.push(character);
            }
            last_was_slash = true;
        } else {
            sanitized.push(character);
            last_was_slash = false;
        }
    }

    sanitized
}

fn guest_error_response(uri: &str, err: MmdsDataStoreError) -> MmdsGuestResponse {
    let (status, body) = match err {
        MmdsDataStoreError::NotFound => (
            MmdsGuestStatus::NotFound,
            format!("Resource not found: {uri}."),
        ),
        MmdsDataStoreError::UnsupportedValueType => {
            (MmdsGuestStatus::NotImplemented, err.to_string())
        }
        MmdsDataStoreError::InvalidObject
        | MmdsDataStoreError::NotInitialized
        | MmdsDataStoreError::DataStoreLimitExceeded { .. }
        | MmdsDataStoreError::Serialization => (MmdsGuestStatus::BadRequest, err.to_string()),
    };

    MmdsGuestResponse::new(status, MmdsGuestContentType::PlainText, body)
}

fn guest_request_parse_error_response(err: MmdsGuestRequestParseError) -> MmdsGuestResponse {
    let status = match err {
        MmdsGuestRequestParseError::UnsupportedMethod => MmdsGuestStatus::MethodNotAllowed,
        MmdsGuestRequestParseError::InvalidUtf8
        | MmdsGuestRequestParseError::MalformedRequest
        | MmdsGuestRequestParseError::UnsupportedHttpVersion
        | MmdsGuestRequestParseError::InvalidUri
        | MmdsGuestRequestParseError::MalformedHeader
        | MmdsGuestRequestParseError::DuplicateContentLength
        | MmdsGuestRequestParseError::InvalidContentLength
        | MmdsGuestRequestParseError::UnsupportedTransferEncoding
        | MmdsGuestRequestParseError::UnsupportedBody
        | MmdsGuestRequestParseError::UnsupportedAccept
        | MmdsGuestRequestParseError::MissingTokenTtl
        | MmdsGuestRequestParseError::InvalidTokenTtl
        | MmdsGuestRequestParseError::DuplicateTokenTtl
        | MmdsGuestRequestParseError::UnsupportedForwardedFor => MmdsGuestStatus::BadRequest,
        MmdsGuestRequestParseError::MissingToken | MmdsGuestRequestParseError::InvalidToken => {
            MmdsGuestStatus::Unauthorized
        }
    };

    let response = MmdsGuestResponse::new(status, MmdsGuestContentType::PlainText, err.to_string());
    if status == MmdsGuestStatus::MethodNotAllowed {
        return response.with_allow_header(MMDS_GUEST_ALLOW_METHODS);
    }

    response
}

fn format_imds(value: &Value) -> Result<String, MmdsDataStoreError> {
    if let Some(map) = value.as_object() {
        let entries = map
            .iter()
            .map(|(key, value)| {
                if value.is_object() {
                    format!("{key}/")
                } else {
                    key.clone()
                }
            })
            .collect::<Vec<_>>();
        return Ok(entries.join("\n"));
    }

    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or(MmdsDataStoreError::UnsupportedValueType)
}

fn validate_object(value: &Value) -> Result<(), MmdsDataStoreError> {
    if value.is_object() {
        Ok(())
    } else {
        Err(MmdsDataStoreError::InvalidObject)
    }
}

fn json_merge_patch(target: &mut Value, patch: &Value) {
    let Some(patch) = patch.as_object() else {
        *target = patch.clone();
        return;
    };

    if !target.is_object() {
        *target = Value::Object(Map::new());
    }

    let Some(target) = target.as_object_mut() else {
        return;
    };

    for (key, value) in patch {
        if value.is_null() {
            target.remove(key);
        } else {
            json_merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

fn is_valid_link_local_ipv4(ipv4_address: Ipv4Addr) -> bool {
    matches!(ipv4_address.octets(), [169, 254, 1..=254, _])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::NetworkInterfaceConfigInput;

    const ARP_TARGET_HARDWARE_ADDRESS_OFFSET: usize = 18;

    fn query_value() -> Value {
        serde_json::json!({
            "age": 43,
            "member": false,
            "meta-data": {
                "ami-id": "ami-123",
                "hostname": "demo.local",
            },
            "nothing": null,
            "phones": [
                "+401234567",
                "+441234567",
            ],
            "user-data": "hello",
        })
    }

    fn initialized_query_state() -> MmdsState {
        let mut state = MmdsState::default();
        state
            .put_data(MmdsContentInput::new(query_value()))
            .expect("test MMDS value should initialize");
        state
    }

    fn enable_imds_compat(state: &mut MmdsState) {
        state.config = Some(MmdsConfig {
            network_interfaces: vec!["eth0".to_string()],
            version: MmdsVersion::V1,
            ipv4_address: None,
            imds_compat: true,
        });
    }

    fn enable_mmds_v1(state: &mut MmdsState) {
        state.config = Some(MmdsConfig {
            network_interfaces: vec!["eth0".to_string()],
            version: MmdsVersion::V1,
            ipv4_address: None,
            imds_compat: false,
        });
    }

    fn enable_mmds_v2(state: &mut MmdsState) {
        state.config = Some(MmdsConfig {
            network_interfaces: vec!["eth0".to_string()],
            version: MmdsVersion::V2,
            ipv4_address: None,
            imds_compat: false,
        });
    }

    fn test_mmds_ipv4_address() -> Ipv4Addr {
        Ipv4Addr::new(169, 254, 169, 254)
    }

    fn test_source_ipv4_address() -> Ipv4Addr {
        Ipv4Addr::new(192, 0, 2, 10)
    }

    fn test_destination_ethernet_address() -> EthernetMacAddress {
        EthernetMacAddress::from_octets([0x02, 0x00, 0x00, 0x00, 0x00, 0x01])
    }

    fn test_source_ethernet_address() -> EthernetMacAddress {
        EthernetMacAddress::from_octets([0x02, 0x00, 0x00, 0x00, 0x00, 0x02])
    }

    fn test_arp_request(target_ipv4_address: Ipv4Addr) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&test_destination_ethernet_address().octets());
        packet.extend_from_slice(&test_source_ethernet_address().octets());
        packet.extend_from_slice(&ETHERNET_ETHERTYPE_ARP.to_be_bytes());
        packet.extend_from_slice(&ARP_HARDWARE_TYPE_ETHERNET.to_be_bytes());
        packet.extend_from_slice(&ARP_PROTOCOL_TYPE_IPV4.to_be_bytes());
        packet.push(ARP_HARDWARE_ADDRESS_LEN_ETHERNET);
        packet.push(ARP_PROTOCOL_ADDRESS_LEN_IPV4);
        packet.extend_from_slice(&ARP_OPERATION_REQUEST.to_be_bytes());
        packet.extend_from_slice(&test_source_ethernet_address().octets());
        packet.extend_from_slice(&test_source_ipv4_address().octets());
        packet.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        packet.extend_from_slice(&target_ipv4_address.octets());
        packet
    }

    fn test_tcp_sequence_number() -> u32 {
        0x0102_0304
    }

    fn test_tcp_acknowledgement_number() -> u32 {
        0xa1a2_a3a4
    }

    fn test_tcp_packet(
        destination_ipv4_address: Ipv4Addr,
        destination_port: u16,
        ipv4_options: &[u8],
        tcp_options: &[u8],
        payload: &[u8],
    ) -> Vec<u8> {
        let ipv4_header_len = IPV4_MIN_HEADER_LEN + ipv4_options.len();
        let tcp_header_len = TCP_MIN_HEADER_LEN + tcp_options.len();
        assert_eq!(ipv4_header_len % 4, 0);
        assert_eq!(tcp_header_len % 4, 0);

        let ipv4_total_len = ipv4_header_len
            .checked_add(tcp_header_len)
            .and_then(|len| len.checked_add(payload.len()))
            .expect("test IPv4 total length should not overflow");
        let ipv4_total_len =
            u16::try_from(ipv4_total_len).expect("test IPv4 total length should fit u16");

        let mut packet = Vec::new();
        packet.extend_from_slice(&test_destination_ethernet_address().octets());
        packet.extend_from_slice(&test_source_ethernet_address().octets());
        packet.extend_from_slice(&ETHERNET_ETHERTYPE_IPV4.to_be_bytes());

        packet.push(
            (IPV4_VERSION << 4)
                | u8::try_from(ipv4_header_len / 4).expect("test IPv4 header length should fit u8"),
        );
        packet.push(0);
        packet.extend_from_slice(&ipv4_total_len.to_be_bytes());
        packet.extend_from_slice(&0x1234_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.push(64);
        packet.push(IPV4_PROTOCOL_TCP);
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&test_source_ipv4_address().octets());
        packet.extend_from_slice(&destination_ipv4_address.octets());
        packet.extend_from_slice(ipv4_options);

        packet.extend_from_slice(&49152_u16.to_be_bytes());
        packet.extend_from_slice(&destination_port.to_be_bytes());
        packet.extend_from_slice(&test_tcp_sequence_number().to_be_bytes());
        packet.extend_from_slice(&test_tcp_acknowledgement_number().to_be_bytes());
        packet.push(
            u8::try_from(tcp_header_len / 4).expect("test TCP header length should fit u8") << 4,
        );
        packet.push(0x18);
        packet.extend_from_slice(&4096_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(tcp_options);
        packet.extend_from_slice(payload);

        packet
    }

    fn test_mmds_tcp_packet(payload: &[u8]) -> Vec<u8> {
        test_tcp_packet(
            test_mmds_ipv4_address(),
            MMDS_GUEST_TCP_PORT,
            &[],
            &[],
            payload,
        )
    }

    fn write_packet_u16(packet: &mut [u8], offset: usize, value: u16) {
        let bytes = value.to_be_bytes();
        packet[offset] = bytes[0];
        packet[offset + 1] = bytes[1];
    }

    fn write_packet_u32(packet: &mut [u8], offset: usize, value: u32) {
        let bytes = value.to_be_bytes();
        packet[offset] = bytes[0];
        packet[offset + 1] = bytes[1];
        packet[offset + 2] = bytes[2];
        packet[offset + 3] = bytes[3];
    }

    fn response_frame_tcp_segment(frame: &[u8]) -> &[u8] {
        frame
            .get(ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN..)
            .expect("response frame should include TCP segment")
    }

    fn response_frame_tcp_payload(frame: &[u8]) -> &[u8] {
        response_frame_tcp_segment(frame)
            .get(TCP_MIN_HEADER_LEN..)
            .expect("response frame should include TCP payload")
    }

    fn response_frame_arp_payload(frame: &[u8]) -> &[u8] {
        frame
            .get(ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + ARP_ETHERNET_IPV4_LEN)
            .expect("response frame should include ARP payload")
    }

    fn assert_empty_tcp_response_frame(
        response: &[u8],
        expected_sequence_number: u32,
        expected_acknowledgement_number: u32,
        expected_tcp_flags: u8,
    ) {
        assert_eq!(
            response.len(),
            ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN
        );
        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(response, ETHERNET_DESTINATION_ADDRESS_OFFSET),
            Some(test_source_ethernet_address().octets())
        );
        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(response, ETHERNET_SOURCE_ADDRESS_OFFSET),
            Some(test_destination_ethernet_address().octets())
        );

        let ipv4_header = response
            .get(ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN)
            .expect("response frame should include IPv4 header");
        assert_eq!(
            packet_u16(ipv4_header, IPV4_TOTAL_LENGTH_OFFSET),
            Some(u16::try_from(IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN).expect("len fits u16"))
        );
        assert_eq!(
            packet_ipv4_address(ipv4_header, IPV4_SOURCE_ADDRESS_OFFSET),
            Some(test_mmds_ipv4_address())
        );
        assert_eq!(
            packet_ipv4_address(ipv4_header, IPV4_DESTINATION_ADDRESS_OFFSET),
            Some(test_source_ipv4_address())
        );
        assert_eq!(internet_checksum(ipv4_header), 0);

        let tcp_segment = response_frame_tcp_segment(response);
        assert_eq!(
            packet_u16(tcp_segment, TCP_SOURCE_PORT_OFFSET),
            Some(MMDS_GUEST_TCP_PORT)
        );
        assert_eq!(
            packet_u16(tcp_segment, TCP_DESTINATION_PORT_OFFSET),
            Some(49152)
        );
        assert_eq!(
            packet_u32(tcp_segment, TCP_SEQUENCE_NUMBER_OFFSET),
            Some(expected_sequence_number)
        );
        assert_eq!(
            packet_u32(tcp_segment, TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET),
            Some(expected_acknowledgement_number)
        );
        assert_eq!(tcp_segment.get(TCP_FLAGS_OFFSET), Some(&expected_tcp_flags));
        assert_eq!(
            tcp_ipv4_checksum(
                test_mmds_ipv4_address(),
                test_source_ipv4_address(),
                u16::try_from(tcp_segment.len()).expect("TCP segment length should fit u16"),
                tcp_segment,
            ),
            0
        );
        assert!(response_frame_tcp_payload(response).is_empty());
    }

    fn assert_json_value(output: &str, expected: Value) {
        let value = serde_json::from_str::<Value>(output).expect("query output should be JSON");
        assert_eq!(value, expected);
    }

    fn assert_guest_response(
        response: MmdsGuestResponse,
        status: MmdsGuestStatus,
        content_type: MmdsGuestContentType,
        body: &str,
    ) {
        assert_eq!(response.status(), status);
        assert_eq!(response.content_type(), content_type);
        assert_eq!(response.body(), body);
    }

    fn assert_guest_http_response(
        bytes: &[u8],
        status: MmdsGuestStatus,
        content_type: MmdsGuestContentType,
        body: &str,
    ) {
        let mut state = initialized_query_state();
        assert_guest_response(state.guest_http_response(bytes), status, content_type, body);
    }

    #[test]
    fn classifies_mmds_guest_arp_request() {
        let packet = test_arp_request(test_mmds_ipv4_address());

        let classified = classify_mmds_guest_arp_request(&packet, test_mmds_ipv4_address())
            .expect("MMDS ARP request should classify");

        assert_eq!(
            classified.source_ethernet_address(),
            test_source_ethernet_address()
        );
        assert_eq!(
            classified.destination_ethernet_address(),
            test_destination_ethernet_address()
        );
        assert_eq!(
            classified.sender_hardware_address(),
            test_source_ethernet_address()
        );
        assert_eq!(
            classified.sender_protocol_address(),
            test_source_ipv4_address()
        );
        assert_eq!(
            classified.target_protocol_address(),
            test_mmds_ipv4_address()
        );
    }

    #[test]
    fn mmds_guest_arp_response_frame_targets_requester() {
        let packet = test_arp_request(test_mmds_ipv4_address());
        let classified = classify_mmds_guest_arp_request(&packet, test_mmds_ipv4_address())
            .expect("MMDS ARP request should classify");

        let response = classified
            .response_frame()
            .expect("ARP response frame should synthesize");

        assert_eq!(response.len(), ETHERNET_HEADER_LEN + ARP_ETHERNET_IPV4_LEN);
        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(
                &response,
                ETHERNET_DESTINATION_ADDRESS_OFFSET
            ),
            Some(test_source_ethernet_address().octets())
        );
        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(&response, ETHERNET_SOURCE_ADDRESS_OFFSET),
            Some(DEFAULT_MMDS_MAC_ADDRESS.octets())
        );
        assert_eq!(
            packet_u16(&response, ETHERNET_ETHERTYPE_OFFSET),
            Some(ETHERNET_ETHERTYPE_ARP)
        );

        let arp = response_frame_arp_payload(&response);
        assert_eq!(
            packet_u16(arp, ARP_HARDWARE_TYPE_OFFSET),
            Some(ARP_HARDWARE_TYPE_ETHERNET)
        );
        assert_eq!(
            packet_u16(arp, ARP_PROTOCOL_TYPE_OFFSET),
            Some(ARP_PROTOCOL_TYPE_IPV4)
        );
        assert_eq!(
            arp.get(ARP_HARDWARE_ADDRESS_LEN_OFFSET),
            Some(&ARP_HARDWARE_ADDRESS_LEN_ETHERNET)
        );
        assert_eq!(
            arp.get(ARP_PROTOCOL_ADDRESS_LEN_OFFSET),
            Some(&ARP_PROTOCOL_ADDRESS_LEN_IPV4)
        );
        assert_eq!(
            packet_u16(arp, ARP_OPERATION_OFFSET),
            Some(ARP_OPERATION_REPLY)
        );
        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(arp, ARP_SENDER_HARDWARE_ADDRESS_OFFSET),
            Some(DEFAULT_MMDS_MAC_ADDRESS.octets())
        );
        assert_eq!(
            packet_ipv4_address(arp, ARP_SENDER_PROTOCOL_ADDRESS_OFFSET),
            Some(test_mmds_ipv4_address())
        );
        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(arp, ARP_TARGET_HARDWARE_ADDRESS_OFFSET),
            Some(test_source_ethernet_address().octets())
        );
        assert_eq!(
            packet_ipv4_address(arp, ARP_TARGET_PROTOCOL_ADDRESS_OFFSET),
            Some(test_source_ipv4_address())
        );
    }

    #[test]
    fn mmds_guest_arp_classifier_rejects_non_mmds_or_malformed_requests() {
        let wrong_target = test_arp_request(Ipv4Addr::new(192, 0, 2, 99));
        let mut reply = test_arp_request(test_mmds_ipv4_address());
        write_packet_u16(
            &mut reply,
            ETHERNET_HEADER_LEN + ARP_OPERATION_OFFSET,
            ARP_OPERATION_REPLY,
        );
        let mut wrong_hardware_type = test_arp_request(test_mmds_ipv4_address());
        write_packet_u16(
            &mut wrong_hardware_type,
            ETHERNET_HEADER_LEN + ARP_HARDWARE_TYPE_OFFSET,
            2,
        );
        let mut wrong_protocol_type = test_arp_request(test_mmds_ipv4_address());
        write_packet_u16(
            &mut wrong_protocol_type,
            ETHERNET_HEADER_LEN + ARP_PROTOCOL_TYPE_OFFSET,
            0x86dd,
        );
        let mut wrong_hardware_len = test_arp_request(test_mmds_ipv4_address());
        wrong_hardware_len[ETHERNET_HEADER_LEN + ARP_HARDWARE_ADDRESS_LEN_OFFSET] = 5;
        let mut wrong_protocol_len = test_arp_request(test_mmds_ipv4_address());
        wrong_protocol_len[ETHERNET_HEADER_LEN + ARP_PROTOCOL_ADDRESS_LEN_OFFSET] = 16;
        let truncated = test_arp_request(test_mmds_ipv4_address())
            .into_iter()
            .take(ETHERNET_HEADER_LEN + ARP_ETHERNET_IPV4_LEN - 1)
            .collect::<Vec<_>>();

        for packet in [
            wrong_target,
            reply,
            wrong_hardware_type,
            wrong_protocol_type,
            wrong_hardware_len,
            wrong_protocol_len,
            truncated,
        ] {
            assert_eq!(
                classify_mmds_guest_arp_request(&packet, test_mmds_ipv4_address()),
                None
            );
        }
    }

    #[test]
    fn classifies_mmds_guest_tcp_packet() {
        let mut packet = test_tcp_packet(
            test_mmds_ipv4_address(),
            MMDS_GUEST_TCP_PORT,
            &[],
            &[],
            b"GET /latest/meta-data HTTP/1.1\r\n\r\n",
        );
        packet.extend_from_slice(&[0xaa; 8]);

        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP packet should classify");

        assert_eq!(
            classified.source_ethernet_address(),
            test_source_ethernet_address()
        );
        assert_eq!(
            classified.destination_ethernet_address(),
            test_destination_ethernet_address()
        );
        assert_eq!(classified.source_ipv4_address(), test_source_ipv4_address());
        assert_eq!(
            classified.destination_ipv4_address(),
            test_mmds_ipv4_address()
        );
        assert_eq!(classified.source_port(), 49152);
        assert_eq!(classified.destination_port(), MMDS_GUEST_TCP_PORT);
        assert_eq!(classified.sequence_number(), test_tcp_sequence_number());
        assert_eq!(
            classified.acknowledgement_number(),
            test_tcp_acknowledgement_number()
        );
        assert_eq!(classified.tcp_flags(), TCP_FLAG_PSH | TCP_FLAG_ACK);
        assert_eq!(
            classified.payload(),
            b"GET /latest/meta-data HTTP/1.1\r\n\r\n"
        );
    }

    #[test]
    fn identifies_initial_mmds_guest_tcp_syn_packet() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;
        let mut syn_packet = test_mmds_tcp_packet(b"");
        syn_packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_SYN;
        let syn = classify_mmds_guest_tcp_packet(&syn_packet, test_mmds_ipv4_address())
            .expect("MMDS TCP SYN packet should classify");

        assert!(syn.is_initial_synchronization_request());

        let mut syn_ack_packet = syn_packet.clone();
        syn_ack_packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_SYN | TCP_FLAG_ACK;
        let syn_ack = classify_mmds_guest_tcp_packet(&syn_ack_packet, test_mmds_ipv4_address())
            .expect("MMDS TCP SYN-ACK packet should classify");
        assert!(!syn_ack.is_initial_synchronization_request());

        let empty_ack_packet = test_mmds_tcp_packet(b"");
        let empty_ack = classify_mmds_guest_tcp_packet(&empty_ack_packet, test_mmds_ipv4_address())
            .expect("empty MMDS TCP packet should classify");
        assert!(!empty_ack.is_initial_synchronization_request());

        let mut syn_with_payload = test_mmds_tcp_packet(b"payload");
        syn_with_payload[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_SYN;
        let syn_with_payload =
            classify_mmds_guest_tcp_packet(&syn_with_payload, test_mmds_ipv4_address())
                .expect("MMDS TCP SYN with payload should classify");
        assert!(!syn_with_payload.is_initial_synchronization_request());
    }

    #[test]
    fn identifies_mmds_guest_tcp_acknowledgement_only_packet() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;
        let mut ack_packet = test_mmds_tcp_packet(b"");
        ack_packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_ACK;
        let ack = classify_mmds_guest_tcp_packet(&ack_packet, test_mmds_ipv4_address())
            .expect("MMDS TCP ACK-only packet should classify");

        assert!(ack.is_acknowledgement_only());

        for flags in [
            TCP_FLAG_SYN,
            TCP_FLAG_SYN | TCP_FLAG_ACK,
            TCP_FLAG_FIN,
            TCP_FLAG_RST,
        ] {
            let mut control_packet = ack_packet.clone();
            control_packet[tcp_start + TCP_FLAGS_OFFSET] = flags;
            let control = classify_mmds_guest_tcp_packet(&control_packet, test_mmds_ipv4_address())
                .expect("MMDS TCP control packet should classify");
            assert!(!control.is_acknowledgement_only());
        }

        let mut ack_with_payload = test_mmds_tcp_packet(b"payload");
        ack_with_payload[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_ACK;
        let ack_with_payload =
            classify_mmds_guest_tcp_packet(&ack_with_payload, test_mmds_ipv4_address())
                .expect("MMDS TCP ACK with payload should classify");
        assert!(!ack_with_payload.is_acknowledgement_only());
    }

    #[test]
    fn identifies_empty_mmds_guest_tcp_fin_close_packet() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;

        for flags in [TCP_FLAG_FIN, TCP_FLAG_FIN | TCP_FLAG_ACK] {
            let mut fin_packet = test_mmds_tcp_packet(b"");
            fin_packet[tcp_start + TCP_FLAGS_OFFSET] = flags;
            let fin = classify_mmds_guest_tcp_packet(&fin_packet, test_mmds_ipv4_address())
                .expect("MMDS TCP FIN close packet should classify");
            assert!(fin.is_empty_fin_close_request());
        }

        for flags in [
            TCP_FLAG_ACK,
            TCP_FLAG_SYN,
            TCP_FLAG_RST,
            TCP_FLAG_FIN | TCP_FLAG_RST,
        ] {
            let mut control_packet = test_mmds_tcp_packet(b"");
            control_packet[tcp_start + TCP_FLAGS_OFFSET] = flags;
            let control = classify_mmds_guest_tcp_packet(&control_packet, test_mmds_ipv4_address())
                .expect("MMDS TCP control packet should classify");
            assert!(!control.is_empty_fin_close_request());
        }

        let mut fin_with_payload = test_mmds_tcp_packet(b"payload");
        fin_with_payload[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_FIN | TCP_FLAG_ACK;
        let fin_with_payload =
            classify_mmds_guest_tcp_packet(&fin_with_payload, test_mmds_ipv4_address())
                .expect("MMDS TCP FIN with payload should classify");
        assert!(!fin_with_payload.is_empty_fin_close_request());
    }

    #[test]
    fn identifies_empty_mmds_guest_tcp_reset_control_packet() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;

        for flags in [
            TCP_FLAG_RST,
            TCP_FLAG_RST | TCP_FLAG_ACK,
            TCP_FLAG_FIN | TCP_FLAG_RST,
        ] {
            let mut reset_packet = test_mmds_tcp_packet(b"");
            reset_packet[tcp_start + TCP_FLAGS_OFFSET] = flags;
            let reset = classify_mmds_guest_tcp_packet(&reset_packet, test_mmds_ipv4_address())
                .expect("MMDS TCP reset control packet should classify");
            assert!(reset.is_empty_reset_control());
        }

        let mut reset_with_payload = test_mmds_tcp_packet(b"payload");
        reset_with_payload[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_RST | TCP_FLAG_ACK;
        let reset_with_payload =
            classify_mmds_guest_tcp_packet(&reset_with_payload, test_mmds_ipv4_address())
                .expect("MMDS TCP reset with payload should classify");
        assert!(!reset_with_payload.is_empty_reset_control());

        let mut psh_packet = test_mmds_tcp_packet(b"");
        psh_packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_PSH;
        let psh = classify_mmds_guest_tcp_packet(&psh_packet, test_mmds_ipv4_address())
            .expect("MMDS TCP PSH control packet should classify");
        assert!(!psh.is_empty_reset_control());
    }

    #[test]
    fn identifies_unsupported_empty_mmds_guest_tcp_control_reset_request() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;

        for flags in [
            0,
            TCP_FLAG_PSH,
            TCP_FLAG_PSH | TCP_FLAG_ACK,
            TCP_FLAG_SYN | TCP_FLAG_ACK,
            TCP_FLAG_FIN | TCP_FLAG_PSH,
        ] {
            let mut unsupported_packet = test_mmds_tcp_packet(b"");
            unsupported_packet[tcp_start + TCP_FLAGS_OFFSET] = flags;
            let unsupported =
                classify_mmds_guest_tcp_packet(&unsupported_packet, test_mmds_ipv4_address())
                    .expect("MMDS TCP unsupported control packet should classify");
            assert!(unsupported.is_unsupported_empty_control_reset_request());
        }

        for flags in [
            TCP_FLAG_SYN,
            TCP_FLAG_ACK,
            TCP_FLAG_FIN,
            TCP_FLAG_FIN | TCP_FLAG_ACK,
            TCP_FLAG_RST,
            TCP_FLAG_RST | TCP_FLAG_ACK,
        ] {
            let mut excluded_packet = test_mmds_tcp_packet(b"");
            excluded_packet[tcp_start + TCP_FLAGS_OFFSET] = flags;
            let excluded =
                classify_mmds_guest_tcp_packet(&excluded_packet, test_mmds_ipv4_address())
                    .expect("MMDS TCP excluded control packet should classify");
            assert!(!excluded.is_unsupported_empty_control_reset_request());
        }

        let mut psh_with_payload = test_mmds_tcp_packet(b"payload");
        psh_with_payload[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_PSH | TCP_FLAG_ACK;
        let psh_with_payload =
            classify_mmds_guest_tcp_packet(&psh_with_payload, test_mmds_ipv4_address())
                .expect("MMDS TCP payload packet should classify");
        assert!(!psh_with_payload.is_unsupported_empty_control_reset_request());
    }

    #[test]
    fn mmds_guest_tcp_syn_ack_response_frame_targets_requester() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;
        let mut packet = test_mmds_tcp_packet(b"");
        packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_SYN;
        write_packet_u32(
            &mut packet,
            tcp_start + TCP_SEQUENCE_NUMBER_OFFSET,
            u32::MAX,
        );
        write_packet_u32(
            &mut packet,
            tcp_start + TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET,
            0,
        );
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP SYN packet should classify");

        let response = classified
            .syn_ack_response_frame()
            .expect("SYN-ACK response frame should synthesize");

        assert_eq!(
            response.len(),
            ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN
        );
        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(
                &response,
                ETHERNET_DESTINATION_ADDRESS_OFFSET
            ),
            Some(test_source_ethernet_address().octets())
        );
        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(&response, ETHERNET_SOURCE_ADDRESS_OFFSET),
            Some(test_destination_ethernet_address().octets())
        );

        let ipv4_header = response
            .get(ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN)
            .expect("response frame should include IPv4 header");
        assert_eq!(
            packet_u16(ipv4_header, IPV4_TOTAL_LENGTH_OFFSET),
            Some(u16::try_from(IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN).expect("len fits u16"))
        );
        assert_eq!(
            packet_ipv4_address(ipv4_header, IPV4_SOURCE_ADDRESS_OFFSET),
            Some(test_mmds_ipv4_address())
        );
        assert_eq!(
            packet_ipv4_address(ipv4_header, IPV4_DESTINATION_ADDRESS_OFFSET),
            Some(test_source_ipv4_address())
        );
        assert_eq!(internet_checksum(ipv4_header), 0);

        let tcp_segment = response_frame_tcp_segment(&response);
        assert_eq!(
            packet_u16(tcp_segment, TCP_SOURCE_PORT_OFFSET),
            Some(MMDS_GUEST_TCP_PORT)
        );
        assert_eq!(
            packet_u16(tcp_segment, TCP_DESTINATION_PORT_OFFSET),
            Some(49152)
        );
        assert_eq!(
            packet_u32(tcp_segment, TCP_SEQUENCE_NUMBER_OFFSET),
            Some(MMDS_GUEST_TCP_SYN_ACK_SEQUENCE_NUMBER)
        );
        assert_eq!(
            packet_u32(tcp_segment, TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET),
            Some(0)
        );
        assert_eq!(
            tcp_segment.get(TCP_FLAGS_OFFSET),
            Some(&(TCP_FLAG_SYN | TCP_FLAG_ACK))
        );
        assert_eq!(
            tcp_ipv4_checksum(
                test_mmds_ipv4_address(),
                test_source_ipv4_address(),
                u16::try_from(tcp_segment.len()).expect("TCP segment length should fit u16"),
                tcp_segment,
            ),
            0
        );
        assert!(response_frame_tcp_payload(&response).is_empty());
    }

    #[test]
    fn mmds_guest_tcp_syn_ack_response_frame_rejects_non_initial_syn() {
        let packet = test_mmds_tcp_packet(b"");
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("empty MMDS TCP packet should classify");

        assert_eq!(
            classified.syn_ack_response_frame(),
            Err(MmdsGuestTcpResponseFrameError::NotInitialSynchronizationRequest)
        );
    }

    #[test]
    fn mmds_guest_tcp_fin_close_response_frames_ack_and_close_requester() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;
        let mut packet = test_mmds_tcp_packet(b"");
        packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_FIN | TCP_FLAG_ACK;
        write_packet_u32(
            &mut packet,
            tcp_start + TCP_SEQUENCE_NUMBER_OFFSET,
            0x0102_0304,
        );
        write_packet_u32(
            &mut packet,
            tcp_start + TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET,
            0x1112_1314,
        );
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP FIN close packet should classify");

        let [ack, fin_ack] = classified
            .fin_close_response_frames()
            .expect("FIN close response frames should synthesize");

        for (response, flags) in [
            (ack.as_slice(), TCP_FLAG_ACK),
            (fin_ack.as_slice(), TCP_FLAG_FIN | TCP_FLAG_ACK),
        ] {
            assert_eq!(
                response.len(),
                ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN
            );
            assert_eq!(
                packet_array::<ETHERNET_MAC_ADDRESS_LEN>(
                    response,
                    ETHERNET_DESTINATION_ADDRESS_OFFSET
                ),
                Some(test_source_ethernet_address().octets())
            );
            assert_eq!(
                packet_array::<ETHERNET_MAC_ADDRESS_LEN>(response, ETHERNET_SOURCE_ADDRESS_OFFSET),
                Some(test_destination_ethernet_address().octets())
            );

            let ipv4_header = response
                .get(ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN)
                .expect("response frame should include IPv4 header");
            assert_eq!(
                packet_u16(ipv4_header, IPV4_TOTAL_LENGTH_OFFSET),
                Some(
                    u16::try_from(IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN).expect("len fits u16")
                )
            );
            assert_eq!(
                packet_ipv4_address(ipv4_header, IPV4_SOURCE_ADDRESS_OFFSET),
                Some(test_mmds_ipv4_address())
            );
            assert_eq!(
                packet_ipv4_address(ipv4_header, IPV4_DESTINATION_ADDRESS_OFFSET),
                Some(test_source_ipv4_address())
            );
            assert_eq!(internet_checksum(ipv4_header), 0);

            let tcp_segment = response_frame_tcp_segment(response);
            assert_eq!(
                packet_u16(tcp_segment, TCP_SOURCE_PORT_OFFSET),
                Some(MMDS_GUEST_TCP_PORT)
            );
            assert_eq!(
                packet_u16(tcp_segment, TCP_DESTINATION_PORT_OFFSET),
                Some(49152)
            );
            assert_eq!(
                packet_u32(tcp_segment, TCP_SEQUENCE_NUMBER_OFFSET),
                Some(0x1112_1314)
            );
            assert_eq!(
                packet_u32(tcp_segment, TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET),
                Some(0x0102_0305)
            );
            assert_eq!(tcp_segment.get(TCP_FLAGS_OFFSET), Some(&flags));
            assert_eq!(
                tcp_ipv4_checksum(
                    test_mmds_ipv4_address(),
                    test_source_ipv4_address(),
                    u16::try_from(tcp_segment.len()).expect("TCP segment length should fit u16"),
                    tcp_segment,
                ),
                0
            );
            assert!(response_frame_tcp_payload(response).is_empty());
        }
    }

    #[test]
    fn mmds_guest_tcp_fin_close_response_frames_reject_non_fin_close() {
        let packet = test_mmds_tcp_packet(b"");
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("empty MMDS TCP packet should classify");

        assert_eq!(
            classified.fin_close_response_frames(),
            Err(MmdsGuestTcpResponseFrameError::NotConnectionCloseRequest)
        );
    }

    #[test]
    fn mmds_guest_tcp_reset_response_frame_targets_requester_with_ack_flag() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;
        let mut packet = test_mmds_tcp_packet(b"");
        packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_PSH | TCP_FLAG_ACK;
        write_packet_u32(
            &mut packet,
            tcp_start + TCP_SEQUENCE_NUMBER_OFFSET,
            0x0102_0304,
        );
        write_packet_u32(
            &mut packet,
            tcp_start + TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET,
            0x1112_1314,
        );
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP reset candidate should classify");

        let response = classified
            .reset_response_frame()
            .expect("RST response frame should synthesize");

        assert_empty_tcp_response_frame(&response, 0x1112_1314, 0, TCP_FLAG_RST);
    }

    #[test]
    fn mmds_guest_tcp_reset_response_frame_targets_requester_without_ack_flag() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;
        let mut packet = test_mmds_tcp_packet(b"");
        packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_FIN | TCP_FLAG_PSH;
        write_packet_u32(
            &mut packet,
            tcp_start + TCP_SEQUENCE_NUMBER_OFFSET,
            0x0102_0304,
        );
        write_packet_u32(
            &mut packet,
            tcp_start + TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET,
            0x1112_1314,
        );
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP reset candidate should classify");

        let response = classified
            .reset_response_frame()
            .expect("RST response frame should synthesize");

        assert_empty_tcp_response_frame(&response, 0, 0x0102_0304, TCP_FLAG_RST | TCP_FLAG_ACK);
    }

    #[test]
    fn mmds_guest_tcp_reset_response_frame_rejects_supported_reset_and_payload_packets() {
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;

        for flags in [
            TCP_FLAG_SYN,
            TCP_FLAG_ACK,
            TCP_FLAG_FIN,
            TCP_FLAG_FIN | TCP_FLAG_ACK,
            TCP_FLAG_RST,
            TCP_FLAG_RST | TCP_FLAG_ACK,
        ] {
            let mut packet = test_mmds_tcp_packet(b"");
            packet[tcp_start + TCP_FLAGS_OFFSET] = flags;
            let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
                .expect("MMDS TCP packet should classify");
            assert_eq!(
                classified.reset_response_frame(),
                Err(MmdsGuestTcpResponseFrameError::NotUnsupportedEmptyControlRequest)
            );
        }

        let mut payload_packet = test_mmds_tcp_packet(b"payload");
        payload_packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_PSH | TCP_FLAG_ACK;
        let payload = classify_mmds_guest_tcp_packet(&payload_packet, test_mmds_ipv4_address())
            .expect("MMDS TCP payload packet should classify");
        assert_eq!(
            payload.reset_response_frame(),
            Err(MmdsGuestTcpResponseFrameError::NotUnsupportedEmptyControlRequest)
        );
    }

    #[test]
    fn mmds_guest_tcp_response_frame_swaps_addresses_and_carries_payload() {
        let request = b"GET /meta-data/hostname HTTP/1.1\r\n\r\n";
        let packet = test_mmds_tcp_packet(request);
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP packet should classify");
        let payload = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ntest";

        let response = classified
            .response_frame(payload)
            .expect("response frame should synthesize");

        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(
                &response,
                ETHERNET_DESTINATION_ADDRESS_OFFSET
            ),
            Some(test_source_ethernet_address().octets())
        );
        assert_eq!(
            packet_array::<ETHERNET_MAC_ADDRESS_LEN>(&response, ETHERNET_SOURCE_ADDRESS_OFFSET),
            Some(test_destination_ethernet_address().octets())
        );
        assert_eq!(
            packet_u16(&response, ETHERNET_ETHERTYPE_OFFSET),
            Some(ETHERNET_ETHERTYPE_IPV4)
        );

        let ipv4_header = response
            .get(ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN)
            .expect("response frame should include IPv4 header");
        assert_eq!(
            packet_u16(ipv4_header, IPV4_TOTAL_LENGTH_OFFSET),
            Some(
                u16::try_from(IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN + payload.len())
                    .expect("test response length should fit u16")
            )
        );
        assert_eq!(
            packet_ipv4_address(ipv4_header, IPV4_SOURCE_ADDRESS_OFFSET),
            Some(test_mmds_ipv4_address())
        );
        assert_eq!(
            packet_ipv4_address(ipv4_header, IPV4_DESTINATION_ADDRESS_OFFSET),
            Some(test_source_ipv4_address())
        );
        assert_eq!(internet_checksum(ipv4_header), 0);

        let tcp_segment = response_frame_tcp_segment(&response);
        assert_eq!(
            packet_u16(tcp_segment, TCP_SOURCE_PORT_OFFSET),
            Some(MMDS_GUEST_TCP_PORT)
        );
        assert_eq!(
            packet_u16(tcp_segment, TCP_DESTINATION_PORT_OFFSET),
            Some(49152)
        );
        assert_eq!(
            packet_u32(tcp_segment, TCP_SEQUENCE_NUMBER_OFFSET),
            Some(test_tcp_acknowledgement_number())
        );
        assert_eq!(
            packet_u32(tcp_segment, TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET),
            Some(
                test_tcp_sequence_number().wrapping_add(
                    u32::try_from(request.len()).expect("request len should fit u32")
                )
            )
        );
        assert_eq!(
            tcp_segment.get(TCP_FLAGS_OFFSET),
            Some(&(TCP_FLAG_PSH | TCP_FLAG_ACK))
        );
        assert_eq!(
            tcp_ipv4_checksum(
                test_mmds_ipv4_address(),
                test_source_ipv4_address(),
                u16::try_from(tcp_segment.len()).expect("TCP segment length should fit u16"),
                tcp_segment,
            ),
            0
        );
        assert_eq!(response_frame_tcp_payload(&response), payload);
    }

    #[test]
    fn mmds_guest_tcp_response_frame_uses_configured_mmds_ipv4_address() {
        let configured_address = Ipv4Addr::new(169, 254, 169, 253);
        let packet = test_tcp_packet(
            configured_address,
            MMDS_GUEST_TCP_PORT,
            &[],
            &[],
            b"GET /meta-data/hostname HTTP/1.1\r\n\r\n",
        );
        let classified = classify_mmds_guest_tcp_packet(&packet, configured_address)
            .expect("configured MMDS TCP packet should classify");

        let response = classified
            .response_frame(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            .expect("response frame should synthesize");
        let ipv4_header = response
            .get(ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN)
            .expect("response frame should include IPv4 header");

        assert_eq!(
            packet_ipv4_address(ipv4_header, IPV4_SOURCE_ADDRESS_OFFSET),
            Some(configured_address)
        );
        assert_eq!(
            packet_ipv4_address(ipv4_header, IPV4_DESTINATION_ADDRESS_OFFSET),
            Some(test_source_ipv4_address())
        );
    }

    #[test]
    fn mmds_guest_tcp_response_frame_acknowledges_syn_and_fin_sequence_space() {
        let request = b"payload";
        let mut packet = test_mmds_tcp_packet(request);
        let tcp_start = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN;
        write_packet_u32(
            &mut packet,
            tcp_start + TCP_SEQUENCE_NUMBER_OFFSET,
            u32::MAX - 1,
        );
        packet[tcp_start + TCP_FLAGS_OFFSET] = TCP_FLAG_SYN | TCP_FLAG_FIN;
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP packet should classify");

        let response = classified
            .response_frame(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("response frame should synthesize");
        let tcp_segment = response_frame_tcp_segment(&response);

        assert_eq!(
            packet_u32(tcp_segment, TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET),
            Some(
                (u32::MAX - 1)
                    .wrapping_add(u32::try_from(request.len()).expect("request len should fit u32"))
                    .wrapping_add(2)
            )
        );
    }

    #[test]
    fn mmds_guest_tcp_response_context_acknowledges_explicit_payload_len() {
        let first_fragment = b"GET /meta-data/";
        let full_request_len = first_fragment.len() + b"hostname HTTP/1.1\r\n\r\n".len();
        let packet = test_mmds_tcp_packet(first_fragment);
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP packet should classify");
        let response_context = classified.response_context();

        let response = response_context
            .response_frame(
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
                full_request_len,
            )
            .expect("response frame should synthesize");
        let tcp_segment = response_frame_tcp_segment(&response);

        assert_eq!(
            packet_u32(tcp_segment, TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET),
            Some(
                test_tcp_sequence_number()
                    .wrapping_add(u32::try_from(full_request_len).expect("test len should fit"))
            )
        );
    }

    #[test]
    fn mmds_guest_tcp_response_context_rejects_request_payload_len_past_tcp_capacity() {
        let packet = test_mmds_tcp_packet(b"GET /meta-data/");
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP packet should classify");
        let request_payload_len = (u32::MAX as usize) + 1;

        assert_eq!(
            classified.response_context().response_frame(
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
                request_payload_len
            ),
            Err(MmdsGuestTcpResponseFrameError::RequestPayloadTooLarge {
                request_payload_len
            })
        );
    }

    #[test]
    fn mmds_guest_tcp_response_frame_accepts_exact_ipv4_capacity() {
        let packet = test_mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP packet should classify");
        let payload = vec![0; IPV4_MAX_TOTAL_LENGTH - IPV4_MIN_HEADER_LEN - TCP_MIN_HEADER_LEN];

        let response = classified
            .response_frame(&payload)
            .expect("max-sized response frame should synthesize");

        let ipv4_header = response
            .get(ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN)
            .expect("response frame should include IPv4 header");
        assert_eq!(
            packet_u16(ipv4_header, IPV4_TOTAL_LENGTH_OFFSET),
            Some(u16::MAX)
        );
        assert_eq!(response_frame_tcp_payload(&response).len(), payload.len());
        let tcp_segment = response_frame_tcp_segment(&response);
        assert_eq!(
            tcp_ipv4_checksum(
                test_mmds_ipv4_address(),
                test_source_ipv4_address(),
                u16::try_from(tcp_segment.len()).expect("TCP segment length should fit u16"),
                tcp_segment,
            ),
            0
        );
    }

    #[test]
    fn mmds_guest_tcp_response_frame_rejects_payload_past_ipv4_capacity() {
        let packet = test_mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP packet should classify");
        let payload = vec![0; IPV4_MAX_TOTAL_LENGTH - IPV4_MIN_HEADER_LEN - TCP_MIN_HEADER_LEN + 1];

        assert_eq!(
            classified.response_frame(&payload),
            Err(MmdsGuestTcpResponseFrameError::PayloadTooLarge {
                payload_len: payload.len()
            })
        );
    }

    #[test]
    fn internet_checksum_matches_known_ipv4_header_fixture() {
        let header = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];

        assert_eq!(internet_checksum(&header), 0xb861);
    }

    #[test]
    fn classifies_mmds_guest_tcp_packet_with_ipv4_and_tcp_options() {
        let packet = test_tcp_packet(
            test_mmds_ipv4_address(),
            MMDS_GUEST_TCP_PORT,
            &[1, 2, 3, 4],
            &[1, 1, 1, 1],
            b"body",
        );

        let classified = classify_mmds_guest_tcp_packet(&packet, test_mmds_ipv4_address())
            .expect("MMDS TCP packet with options should classify");

        assert_eq!(classified.payload(), b"body");
    }

    #[test]
    fn mmds_guest_tcp_classifier_rejects_non_mmds_destination() {
        let wrong_ip = test_tcp_packet(Ipv4Addr::new(169, 254, 169, 250), 80, &[], &[], b"");
        let wrong_port = test_tcp_packet(test_mmds_ipv4_address(), 8080, &[], &[], b"");

        assert_eq!(
            classify_mmds_guest_tcp_packet(&wrong_ip, test_mmds_ipv4_address()),
            None
        );
        assert_eq!(
            classify_mmds_guest_tcp_packet(&wrong_port, test_mmds_ipv4_address()),
            None
        );
    }

    #[test]
    fn mmds_guest_tcp_classifier_rejects_non_ipv4_or_non_tcp_packets() {
        let mut arp_packet = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"");
        write_packet_u16(&mut arp_packet, ETHERNET_ETHERTYPE_OFFSET, 0x0806);
        let mut udp_packet = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"");
        udp_packet[ETHERNET_HEADER_LEN + IPV4_PROTOCOL_OFFSET] = 17;
        let mut ipv6_version_packet = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"");
        ipv6_version_packet[ETHERNET_HEADER_LEN + IPV4_VERSION_IHL_OFFSET] = (6 << 4)
            | u8::try_from(IPV4_MIN_HEADER_LEN / 4).expect("test IPv4 header length should fit u8");

        assert_eq!(
            classify_mmds_guest_tcp_packet(&arp_packet, test_mmds_ipv4_address()),
            None
        );
        assert_eq!(
            classify_mmds_guest_tcp_packet(&udp_packet, test_mmds_ipv4_address()),
            None
        );
        assert_eq!(
            classify_mmds_guest_tcp_packet(&ipv6_version_packet, test_mmds_ipv4_address()),
            None
        );
    }

    #[test]
    fn mmds_guest_tcp_classifier_rejects_truncated_packets() {
        let packet = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"");

        for len in [
            0,
            ETHERNET_HEADER_LEN - 1,
            ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN - 1,
            ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN - 1,
        ] {
            assert_eq!(
                classify_mmds_guest_tcp_packet(&packet[..len], test_mmds_ipv4_address()),
                None
            );
        }
    }

    #[test]
    fn mmds_guest_tcp_classifier_rejects_invalid_ipv4_flags_and_fragments() {
        let fragment_offset = ETHERNET_HEADER_LEN + IPV4_FLAGS_FRAGMENT_OFFSET_OFFSET;
        let mut reserved_flag = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"payload");
        write_packet_u16(&mut reserved_flag, fragment_offset, 0x8000);
        let mut more_fragments =
            test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"payload");
        write_packet_u16(&mut more_fragments, fragment_offset, 0x2000);
        let mut nonzero_fragment_offset =
            test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"payload");
        write_packet_u16(&mut nonzero_fragment_offset, fragment_offset, 1);
        let mut dont_fragment = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"payload");
        write_packet_u16(&mut dont_fragment, fragment_offset, 0x4000);

        assert_eq!(
            classify_mmds_guest_tcp_packet(&reserved_flag, test_mmds_ipv4_address()),
            None
        );
        assert_eq!(
            classify_mmds_guest_tcp_packet(&more_fragments, test_mmds_ipv4_address()),
            None
        );
        assert_eq!(
            classify_mmds_guest_tcp_packet(&nonzero_fragment_offset, test_mmds_ipv4_address()),
            None
        );
        assert!(classify_mmds_guest_tcp_packet(&dont_fragment, test_mmds_ipv4_address()).is_some());
    }

    #[test]
    fn mmds_guest_tcp_classifier_rejects_invalid_ipv4_lengths() {
        let mut short_ihl = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"");
        short_ihl[ETHERNET_HEADER_LEN + IPV4_VERSION_IHL_OFFSET] = (IPV4_VERSION << 4) | 4;
        let mut short_total_len = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"");
        write_packet_u16(
            &mut short_total_len,
            ETHERNET_HEADER_LEN + IPV4_TOTAL_LENGTH_OFFSET,
            39,
        );
        let mut total_len_past_frame = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"");
        write_packet_u16(
            &mut total_len_past_frame,
            ETHERNET_HEADER_LEN + IPV4_TOTAL_LENGTH_OFFSET,
            41,
        );

        assert_eq!(
            classify_mmds_guest_tcp_packet(&short_ihl, test_mmds_ipv4_address()),
            None
        );
        assert_eq!(
            classify_mmds_guest_tcp_packet(&short_total_len, test_mmds_ipv4_address()),
            None
        );
        assert_eq!(
            classify_mmds_guest_tcp_packet(&total_len_past_frame, test_mmds_ipv4_address()),
            None
        );
    }

    #[test]
    fn mmds_guest_tcp_classifier_rejects_invalid_tcp_lengths() {
        let tcp_data_offset = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN + TCP_DATA_OFFSET_OFFSET;
        let mut short_tcp_header = test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"");
        short_tcp_header[tcp_data_offset] = 4 << 4;
        let mut tcp_header_past_segment =
            test_tcp_packet(test_mmds_ipv4_address(), 80, &[], &[], b"");
        tcp_header_past_segment[tcp_data_offset] = 6 << 4;

        assert_eq!(
            classify_mmds_guest_tcp_packet(&short_tcp_header, test_mmds_ipv4_address()),
            None
        );
        assert_eq!(
            classify_mmds_guest_tcp_packet(&tcp_header_past_segment, test_mmds_ipv4_address()),
            None
        );
    }

    #[test]
    fn mmds_guest_tcp_packet_response_bytes_return_http_response() {
        let request = b"GET /meta-data/hostname HTTP/1.1\r\nAccept: */*\r\n\r\n";
        let packet = test_mmds_tcp_packet(request);
        let mut expected_state = initialized_query_state();
        let expected = expected_state.guest_http_response_bytes(request);
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_tcp_packet_response_bytes(&packet, test_mmds_ipv4_address()),
            Some(expected)
        );
    }

    #[test]
    fn mmds_guest_tcp_packet_response_bytes_preserve_http_10_response_version() {
        let request = b"GET /meta-data/hostname HTTP/1.0\r\nAccept: */*\r\n\r\n";
        let packet = test_mmds_tcp_packet(request);
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_tcp_packet_response_bytes(&packet, test_mmds_ipv4_address()),
            Some(
                b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n\r\ndemo.local"
                    .to_vec()
            )
        );
    }

    #[test]
    fn mmds_guest_tcp_packet_response_bytes_ignore_non_candidates_without_mutating() {
        let wrong_destination = test_tcp_packet(
            Ipv4Addr::new(169, 254, 169, 250),
            MMDS_GUEST_TCP_PORT,
            &[],
            &[],
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        );
        let truncated = test_tcp_packet(
            test_mmds_ipv4_address(),
            MMDS_GUEST_TCP_PORT,
            &[],
            &[],
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        )
        .into_iter()
        .take(ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN - 1)
        .collect::<Vec<_>>();

        for packet in [wrong_destination, truncated] {
            let mut state = initialized_query_state();
            state.token_authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
            let original = state.get_data().expect("data store should be initialized");

            assert_eq!(
                state.guest_tcp_packet_response_bytes(&packet, test_mmds_ipv4_address()),
                None
            );
            assert_eq!(state.get_data(), Ok(original));
            let token = state
                .generate_guest_token(1)
                .expect("ignored packet should not consume token capacity");
            assert!(state.is_guest_token_valid(&token));
        }
    }

    #[test]
    fn mmds_guest_tcp_packet_response_bytes_ignore_empty_payload_without_mutating() {
        let packet = test_mmds_tcp_packet(b"");
        let mut state = initialized_query_state();
        state.token_authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
        let original = state.get_data().expect("data store should be initialized");

        assert_eq!(
            state.guest_tcp_packet_response_bytes(&packet, test_mmds_ipv4_address()),
            None
        );
        assert_eq!(state.get_data(), Ok(original));
        let token = state
            .generate_guest_token(1)
            .expect("empty TCP payload should not consume token capacity");
        assert!(state.is_guest_token_valid(&token));
    }

    #[test]
    fn mmds_guest_tcp_packet_response_bytes_serialize_parse_errors() {
        let request = b"GET /meta-data/hostname\r\n\r\n";
        let packet = test_mmds_tcp_packet(request);
        let mut expected_state = initialized_query_state();
        let expected = expected_state.guest_http_response_bytes(request);
        let mut state = initialized_query_state();
        let original = state.get_data().expect("data store should be initialized");

        assert_eq!(
            state.guest_tcp_packet_response_bytes(&packet, test_mmds_ipv4_address()),
            Some(expected)
        );
        assert_eq!(state.get_data(), Ok(original));
    }

    #[test]
    fn mmds_guest_tcp_packet_response_bytes_preserve_token_flow() {
        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);
        state.token_authority = MmdsTokenAuthority::with_manual_clock(2, 1_000);
        let put_packet = test_mmds_tcp_packet(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        );

        let token_response = state
            .guest_tcp_packet_response_bytes(&put_packet, test_mmds_ipv4_address())
            .expect("token PUT packet should produce response bytes");
        let token_response =
            String::from_utf8(token_response).expect("token response should be UTF-8");
        let (_head, token) = token_response
            .split_once("\r\n\r\n")
            .expect("token response should include header terminator");
        assert_mmds_token_shape(token);
        assert!(state.is_guest_token_valid(token));

        let get_request = format!(
            "GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\nX-metadata-token: {token}\r\n\r\n"
        );
        let get_packet = test_mmds_tcp_packet(get_request.as_bytes());

        assert_eq!(
            state.guest_tcp_packet_response_bytes(&get_packet, test_mmds_ipv4_address()),
            Some(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12\r\n\r\n\"demo.local\""
                    .to_vec()
            )
        );
    }

    fn assert_guest_request(
        bytes: &[u8],
        expected_uri: &str,
        expected_output_format: MmdsOutputFormat,
    ) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };

        assert_eq!(request.uri(), expected_uri);
        assert_eq!(request.output_format(), expected_output_format);
        assert_eq!(request.token(), &MmdsGuestToken::Missing);
    }

    fn assert_guest_token_get_request(
        bytes: &[u8],
        expected_uri: &str,
        expected_token_header: MmdsGuestTokenHeader,
        expected_token_value: &str,
    ) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };

        assert_eq!(request.uri(), expected_uri);
        assert_eq!(
            request.token(),
            &MmdsGuestToken::Header {
                token_header: expected_token_header,
                token_value: expected_token_value.to_string(),
            }
        );
    }

    fn assert_guest_token_get_duplicate(bytes: &[u8]) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };

        assert_eq!(request.token(), &MmdsGuestToken::Duplicate);
    }

    fn assert_guest_token_put_request(
        bytes: &[u8],
        expected_uri: &str,
        expected_ttl_header: MmdsGuestTokenTtlHeader,
        expected_ttl_value: &str,
    ) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::TokenPut(request) = request else {
            panic!("test MMDS guest HTTP request should be token PUT");
        };

        assert_eq!(request.uri(), expected_uri);
        assert_eq!(
            request.token_ttl(),
            &MmdsGuestTokenTtl::Header {
                ttl_header: expected_ttl_header,
                ttl_value: expected_ttl_value.to_string(),
            }
        );
    }

    fn assert_guest_token_put_duplicate_ttl(bytes: &[u8]) {
        let request =
            MmdsGuestRequest::parse_http(bytes).expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::TokenPut(request) = request else {
            panic!("test MMDS guest HTTP request should be token PUT");
        };

        assert_eq!(request.token_ttl(), &MmdsGuestTokenTtl::Duplicate);
    }

    fn serialized_len(value: &Value) -> usize {
        serde_json::to_vec(value)
            .expect("test JSON value should serialize")
            .len()
    }

    fn assert_mmds_token_shape(token: &str) {
        assert_eq!(token.len(), MMDS_TOKEN_BYTES * 2);
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }

    #[test]
    fn mmds_token_authority_accepts_ttl_boundaries() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(2, 1_000);

        let min_token = authority
            .generate_token(MMDS_TOKEN_MIN_TTL_SECONDS)
            .expect("minimum token TTL should be accepted");
        let max_token = authority
            .generate_token(MMDS_TOKEN_MAX_TTL_SECONDS)
            .expect("maximum token TTL should be accepted");

        assert_mmds_token_shape(&min_token);
        assert_mmds_token_shape(&max_token);
        assert!(authority.is_valid(&min_token));
        assert!(authority.is_valid(&max_token));
    }

    #[test]
    fn mmds_token_authority_rejects_invalid_ttl_values() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(2, 1_000);

        assert_eq!(
            authority.generate_token(0),
            Err(MmdsTokenError::InvalidTtl { ttl_seconds: 0 })
        );
        assert_eq!(
            authority.generate_token(MMDS_TOKEN_MAX_TTL_SECONDS + 1),
            Err(MmdsTokenError::InvalidTtl {
                ttl_seconds: MMDS_TOKEN_MAX_TTL_SECONDS + 1,
            })
        );
        assert!(authority.tokens.is_empty());
    }

    #[test]
    fn mmds_token_errors_display_deterministic_messages() {
        assert_eq!(
            MmdsTokenError::InvalidTtl { ttl_seconds: 0 }.to_string(),
            "Invalid MMDS token TTL: 0. Please provide a value between 1 and 21600."
        );
        assert_eq!(
            MmdsTokenError::ActiveTokenLimitExceeded { limit: 1 }.to_string(),
            "The MMDS active token limit was exceeded: 1."
        );
        assert_eq!(
            MmdsTokenError::RandomnessUnavailable.to_string(),
            "MMDS token randomness is unavailable."
        );
        assert_eq!(
            MmdsTokenError::TokenCollision.to_string(),
            "MMDS token generation collided repeatedly."
        );
    }

    #[test]
    fn mmds_token_authority_rejects_unknown_empty_and_expired_tokens() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
        let token = authority
            .generate_token(1)
            .expect("token generation should succeed");

        assert!(authority.is_valid(&token));
        assert!(!authority.is_valid(""));
        assert!(!authority.is_valid("not-a-generated-token"));

        authority.set_now_millis(1_999);
        assert!(authority.is_valid(&token));

        authority.set_now_millis(2_000);
        assert!(!authority.is_valid(&token));
    }

    #[test]
    fn mmds_token_authority_cleans_expired_tokens_before_capacity_check() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
        let first = authority
            .generate_token(1)
            .expect("first token generation should succeed");
        assert_eq!(authority.tokens.len(), 1);

        authority.set_now_millis(2_000);
        assert!(!authority.is_valid(&first));

        let second = authority
            .generate_token(1)
            .expect("expired token should be cleaned before capacity check");

        assert!(authority.is_valid(&second));
        assert_eq!(authority.tokens.len(), 1);
    }

    #[test]
    fn mmds_token_authority_reports_capacity_exhaustion() {
        let mut authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
        authority
            .generate_token(1)
            .expect("first token generation should succeed");

        assert_eq!(
            authority.generate_token(1),
            Err(MmdsTokenError::ActiveTokenLimitExceeded { limit: 1 })
        );
        assert_eq!(authority.tokens.len(), 1);
    }

    #[test]
    fn mmds_state_guest_token_delegates_to_token_authority() {
        let mut state = MmdsState {
            token_authority: MmdsTokenAuthority::with_manual_clock(1, 1_000),
            ..MmdsState::default()
        };
        let token = state
            .generate_guest_token(1)
            .expect("state token generation should succeed");

        assert!(state.is_guest_token_valid(&token));

        state.token_authority.set_now_millis(2_000);
        assert!(!state.is_guest_token_valid(&token));
    }

    #[test]
    fn mmds_state_equality_ignores_token_clock_origin() {
        assert_eq!(MmdsState::default(), MmdsState::default());
    }

    #[test]
    fn mmds_state_handle_shares_mutations() {
        let handle = MmdsStateHandle::default();
        let cloned = handle.clone();
        let value = query_value();

        handle
            .with_mut(|state| state.put_data(MmdsContentInput::new(value.clone())))
            .expect("MMDS handle should lock")
            .expect("MMDS data should store");

        assert_eq!(
            cloned
                .with(MmdsState::get_data)
                .expect("cloned MMDS handle should lock"),
            Ok(value)
        );
    }

    #[test]
    fn mmds_config_effective_ipv4_address_uses_default_or_configured_value() {
        let mut state = MmdsState::default();
        enable_mmds_v1(&mut state);
        assert_eq!(
            state
                .config()
                .expect("MMDS config should be present")
                .effective_ipv4_address(),
            DEFAULT_MMDS_IPV4_ADDRESS
        );

        state
            .put_config(
                MmdsConfigInput::new(vec!["eth0".to_string()])
                    .with_ipv4_address(Ipv4Addr::new(169, 254, 169, 253)),
                &[NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0")
                    .validate()
                    .expect("network config should validate")],
            )
            .expect("MMDS config should store");
        assert_eq!(
            state
                .config()
                .expect("MMDS config should be present")
                .effective_ipv4_address(),
            Ipv4Addr::new(169, 254, 169, 253)
        );
    }

    #[test]
    fn put_data_accepts_exact_data_store_limit() {
        let value = serde_json::json!({"a": ""});
        let mut state = MmdsState::new(serialized_len(&value));

        state
            .put_data(MmdsContentInput::new(value.clone()))
            .expect("exact-limit MMDS value should be accepted");

        assert_eq!(state.get_data(), Ok(value));
    }

    #[test]
    fn put_data_rejects_one_byte_over_data_store_limit_without_initializing() {
        let value = serde_json::json!({"a": ""});
        let limit_bytes = serialized_len(&value) - 1;
        let mut state = MmdsState::new(limit_bytes);

        assert_eq!(
            state.put_data(MmdsContentInput::new(value.clone())),
            Err(MmdsDataStoreError::DataStoreLimitExceeded {
                limit_bytes,
                size_bytes: serialized_len(&value),
            })
        );
        assert_eq!(state.get_data(), Err(MmdsDataStoreError::NotInitialized));
    }

    #[test]
    fn patch_data_accepts_exact_data_store_limit() {
        let original = serde_json::json!({"a": ""});
        let patch = serde_json::json!({"b": ""});
        let patched = serde_json::json!({"a": "", "b": ""});
        let mut state = MmdsState::new(serialized_len(&patched));

        state
            .put_data(MmdsContentInput::new(original))
            .expect("initial MMDS value should fit");
        state
            .patch_data(MmdsContentInput::new(patch))
            .expect("exact-limit patched MMDS value should be accepted");

        assert_eq!(state.get_data(), Ok(patched));
    }

    #[test]
    fn patch_data_rejects_one_byte_over_data_store_limit_without_mutating() {
        let original = serde_json::json!({"a": ""});
        let patch = serde_json::json!({"b": ""});
        let patched = serde_json::json!({"a": "", "b": ""});
        let limit_bytes = serialized_len(&patched) - 1;
        let mut state = MmdsState::new(limit_bytes);

        state
            .put_data(MmdsContentInput::new(original.clone()))
            .expect("initial MMDS value should fit");
        assert_eq!(
            state.patch_data(MmdsContentInput::new(patch)),
            Err(MmdsDataStoreError::DataStoreLimitExceeded {
                limit_bytes,
                size_bytes: serialized_len(&patched),
            })
        );
        assert_eq!(state.get_data(), Ok(original));
    }

    #[test]
    fn query_data_requires_initialized_data_store() {
        let state = MmdsState::default();

        assert_eq!(
            state.query_data("/", MmdsOutputFormat::Json),
            Err(MmdsDataStoreError::NotInitialized)
        );
    }

    #[test]
    fn query_data_returns_root_object_json() {
        let state = initialized_query_state();
        let output = state
            .query_data("/", MmdsOutputFormat::Json)
            .expect("root JSON query should succeed");

        assert_json_value(&output, query_value());
    }

    #[test]
    fn query_data_lists_root_object_as_imds() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/", MmdsOutputFormat::Imds),
            Ok("age\nmember\nmeta-data/\nnothing\nphones\nuser-data".to_string())
        );
    }

    #[test]
    fn query_data_lists_nested_object_and_formats_string_leaf_as_imds() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/meta-data", MmdsOutputFormat::Imds),
            Ok("ami-id\nhostname".to_string())
        );
        assert_eq!(
            state.query_data("/meta-data/hostname", MmdsOutputFormat::Imds),
            Ok("demo.local".to_string())
        );
    }

    #[test]
    fn query_data_ignores_trailing_slash_for_lookup() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/meta-data/", MmdsOutputFormat::Imds),
            Ok("ami-id\nhostname".to_string())
        );
        assert_eq!(
            state.query_data("/phones/", MmdsOutputFormat::Json),
            Ok(r#"["+401234567","+441234567"]"#.to_string())
        );
    }

    #[test]
    fn query_data_returns_json_for_arrays_and_scalars() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/phones", MmdsOutputFormat::Json),
            Ok(r#"["+401234567","+441234567"]"#.to_string())
        );
        assert_eq!(
            state.query_data("/phones/0", MmdsOutputFormat::Json),
            Ok(r#""+401234567""#.to_string())
        );
        assert_eq!(
            state.query_data("/age", MmdsOutputFormat::Json),
            Ok("43".to_string())
        );
        assert_eq!(
            state.query_data("/member", MmdsOutputFormat::Json),
            Ok("false".to_string())
        );
        assert_eq!(
            state.query_data("/nothing", MmdsOutputFormat::Json),
            Ok("null".to_string())
        );
    }

    #[test]
    fn query_data_uses_json_pointer_escaping() {
        let mut state = MmdsState::default();
        state
            .put_data(MmdsContentInput::new(serde_json::json!({
                "with/slash": {
                    "tilde~key": "escaped",
                },
            })))
            .expect("test MMDS value should initialize");

        assert_eq!(
            state.query_data("/with~1slash/tilde~0key", MmdsOutputFormat::Json),
            Ok(r#""escaped""#.to_string())
        );
    }

    #[test]
    fn query_data_rejects_missing_path() {
        let state = initialized_query_state();

        assert_eq!(
            state.query_data("/meta-data/missing", MmdsOutputFormat::Json),
            Err(MmdsDataStoreError::NotFound)
        );
    }

    #[test]
    fn query_data_rejects_unsupported_imds_value_types() {
        let state = initialized_query_state();

        for path in ["/age", "/member", "/nothing", "/phones"] {
            assert_eq!(
                state.query_data(path, MmdsOutputFormat::Imds),
                Err(MmdsDataStoreError::UnsupportedValueType)
            );
        }
    }

    #[test]
    fn query_data_error_messages_match_firecracker_shape() {
        assert_eq!(
            MmdsDataStoreError::NotFound.to_string(),
            "The MMDS resource does not exist."
        );
        assert_eq!(
            MmdsDataStoreError::UnsupportedValueType.to_string(),
            "Cannot retrieve value. The value has an unsupported type."
        );
    }

    #[test]
    fn query_data_imds_compat_forces_imds_formatting() {
        let mut state = initialized_query_state();
        enable_imds_compat(&mut state);

        assert_eq!(
            state.query_data("/meta-data", MmdsOutputFormat::Json),
            Ok("ami-id\nhostname".to_string())
        );
        assert_eq!(
            state.query_data("/age", MmdsOutputFormat::Json),
            Err(MmdsDataStoreError::UnsupportedValueType)
        );
    }

    #[test]
    fn query_data_does_not_mutate_data_store() {
        let state = initialized_query_state();
        let original = state.get_data().expect("data store should be initialized");

        assert_eq!(
            state.query_data("/meta-data", MmdsOutputFormat::Imds),
            Ok("ami-id\nhostname".to_string())
        );
        assert_eq!(state.get_data(), Ok(original));
    }

    #[test]
    fn guest_status_codes_match_http_values() {
        assert_eq!(MmdsGuestStatus::Ok.as_u16(), 200);
        assert_eq!(MmdsGuestStatus::BadRequest.as_u16(), 400);
        assert_eq!(MmdsGuestStatus::Unauthorized.as_u16(), 401);
        assert_eq!(MmdsGuestStatus::NotFound.as_u16(), 404);
        assert_eq!(MmdsGuestStatus::MethodNotAllowed.as_u16(), 405);
        assert_eq!(MmdsGuestStatus::NotImplemented.as_u16(), 501);
    }

    #[test]
    fn guest_status_reason_phrases_match_http_values() {
        assert_eq!(MmdsGuestStatus::Ok.reason_phrase(), "OK");
        assert_eq!(MmdsGuestStatus::BadRequest.reason_phrase(), "Bad Request");
        assert_eq!(
            MmdsGuestStatus::Unauthorized.reason_phrase(),
            "Unauthorized"
        );
        assert_eq!(MmdsGuestStatus::NotFound.reason_phrase(), "Not Found");
        assert_eq!(
            MmdsGuestStatus::NotImplemented.reason_phrase(),
            "Not Implemented"
        );
        assert_eq!(
            MmdsGuestStatus::MethodNotAllowed.reason_phrase(),
            "Method Not Allowed"
        );
    }

    #[test]
    fn guest_content_type_names_match_http_values() {
        assert_eq!(
            MmdsGuestContentType::ApplicationJson.as_str(),
            "application/json"
        );
        assert_eq!(MmdsGuestContentType::PlainText.as_str(), "text/plain");
    }

    #[test]
    fn mmds_guest_request_parses_get_without_accept_as_imds() {
        assert_guest_request(
            b"GET /latest/meta-data/hostname HTTP/1.1\r\nHost: 169.254.169.254\r\n\r\n",
            "/latest/meta-data/hostname",
            MmdsOutputFormat::Imds,
        );
    }

    #[test]
    fn mmds_guest_request_parses_absolute_form_uri_path() {
        assert_guest_request(
            b"GET http://169.254.169.254/latest/meta-data/hostname HTTP/1.0\r\n\r\n",
            "/latest/meta-data/hostname",
            MmdsOutputFormat::Imds,
        );
    }

    #[test]
    fn mmds_guest_request_parses_application_json_accept() {
        assert_guest_request(
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
            "/meta-data/hostname",
            MmdsOutputFormat::Json,
        );
    }

    #[test]
    fn mmds_guest_request_preserves_supported_http_versions() {
        let request = MmdsGuestRequest::parse_http(
            b"GET /meta-data/hostname HTTP/1.0\r\nAccept: application/json\r\n\r\n",
        )
        .expect("HTTP/1.0 GET request should parse");
        assert_eq!(request.http_version(), MmdsGuestHttpVersion::Http10);
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };
        assert_eq!(request.http_version(), MmdsGuestHttpVersion::Http10);

        let request = MmdsGuestRequest::parse_http(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        )
        .expect("HTTP/1.1 token PUT request should parse");
        assert_eq!(request.http_version(), MmdsGuestHttpVersion::Http11);
        let MmdsGuestRequest::TokenPut(request) = request else {
            panic!("test MMDS guest HTTP request should be token PUT");
        };
        assert_eq!(request.http_version(), MmdsGuestHttpVersion::Http11);
    }

    #[test]
    fn mmds_guest_request_parses_imds_accept_variants() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept:\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: */*\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept:\ttext/plain \r\n\r\n",
        ] {
            assert_guest_request(request, "/meta-data/hostname", MmdsOutputFormat::Imds);
        }
    }

    #[test]
    fn mmds_guest_request_accepts_zero_content_length() {
        assert_guest_request(
            b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length:\t0 \r\n\r\n",
            "/meta-data/hostname",
            MmdsOutputFormat::Imds,
        );
    }

    #[test]
    fn mmds_guest_request_rejects_invalid_utf8() {
        let request = b"GET /meta-data/host\xffname HTTP/1.1\r\n\r\n";

        assert_eq!(
            MmdsGuestRequest::parse_http(request),
            Err(MmdsGuestRequestParseError::InvalidUtf8)
        );
    }

    #[test]
    fn mmds_guest_request_rejects_malformed_request_line() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1 extra\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname\r\n\r\n",
            b"\r\n\r\n",
        ] {
            assert_eq!(
                MmdsGuestRequest::parse_http(request),
                Err(MmdsGuestRequestParseError::MalformedRequest)
            );
        }
    }

    #[test]
    fn mmds_guest_request_rejects_unsupported_method_and_version() {
        assert_eq!(
            MmdsGuestRequest::parse_http(b"POST /meta-data/hostname HTTP/1.1\r\n\r\n"),
            Err(MmdsGuestRequestParseError::UnsupportedMethod)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(b"POST /meta-data/hostname HTTP/2\r\n\r\n"),
            Err(MmdsGuestRequestParseError::UnsupportedMethod)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(b"GET /meta-data/hostname HTTP/2\r\n\r\n"),
            Err(MmdsGuestRequestParseError::UnsupportedHttpVersion)
        );
    }

    #[test]
    fn mmds_guest_request_rejects_invalid_uri() {
        for request in [
            b"GET http://169.254.169.254 HTTP/1.1\r\n\r\n".as_slice(),
            b"GET http:///meta-data/hostname HTTP/1.1\r\n\r\n",
            b"GET http:// HTTP/1.1\r\n\r\n",
            b"GET * HTTP/1.1\r\n\r\n",
        ] {
            assert_eq!(
                MmdsGuestRequest::parse_http(request),
                Err(MmdsGuestRequestParseError::InvalidUri)
            );
        }
    }

    #[test]
    fn mmds_guest_request_rejects_malformed_headers() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept application/json\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nBad Header: value\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\n: value\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nBad\x7fHeader: value\r\n\r\n",
        ] {
            assert_eq!(
                MmdsGuestRequest::parse_http(request),
                Err(MmdsGuestRequestParseError::MalformedHeader)
            );
        }
    }

    #[test]
    fn mmds_guest_request_rejects_body_framing() {
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 0\r\n\r\nbody"
            ),
            Err(MmdsGuestRequestParseError::UnsupportedBody)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody"
            ),
            Err(MmdsGuestRequestParseError::UnsupportedBody)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            ),
            Err(MmdsGuestRequestParseError::DuplicateContentLength)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: +0\r\n\r\n"
            ),
            Err(MmdsGuestRequestParseError::InvalidContentLength)
        );
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"
            ),
            Err(MmdsGuestRequestParseError::UnsupportedTransferEncoding)
        );
    }

    #[test]
    fn mmds_guest_request_rejects_unsupported_accept_header() {
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/xml\r\n\r\n"
            ),
            Err(MmdsGuestRequestParseError::UnsupportedAccept)
        );
    }

    #[test]
    fn mmds_guest_request_parses_token_put_ttl_headers() {
        assert_guest_token_put_request(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
            "/latest/api/token",
            MmdsGuestTokenTtlHeader::Metadata,
            "60",
        );
        assert_guest_token_put_request(
            b"PUT /latest/api/token HTTP/1.1\r\nX-aws-ec2-metadata-token-ttl-seconds: 21600\r\n\r\n",
            "/latest/api/token",
            MmdsGuestTokenTtlHeader::AwsEc2Metadata,
            "21600",
        );
        assert_guest_token_put_request(
            b"PUT http://169.254.169.254/latest/api/token HTTP/1.1\r\nx-MeTaDaTa-ToKeN-TtL-SeCoNdS: 1\r\n\r\n",
            "/latest/api/token",
            MmdsGuestTokenTtlHeader::Metadata,
            "1",
        );
        assert_guest_token_put_request(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: application/json\r\n\r\n",
            "/latest/api/token",
            MmdsGuestTokenTtlHeader::Metadata,
            "application/json",
        );
        assert_guest_token_put_duplicate_ttl(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
        );
    }

    #[test]
    fn mmds_guest_request_parses_get_token_headers() {
        assert_guest_token_get_request(
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: token-1\r\n\r\n",
            "/meta-data/hostname",
            MmdsGuestTokenHeader::Metadata,
            "token-1",
        );
        assert_guest_token_get_request(
            b"GET /meta-data/hostname HTTP/1.1\r\nX-aws-ec2-metadata-token: token-2\r\n\r\n",
            "/meta-data/hostname",
            MmdsGuestTokenHeader::AwsEc2Metadata,
            "token-2",
        );
        assert_guest_token_get_request(
            b"GET http://169.254.169.254/meta-data/hostname HTTP/1.1\r\nx-MeTaDaTa-ToKeN: token-3\r\n\r\n",
            "/meta-data/hostname",
            MmdsGuestTokenHeader::Metadata,
            "token-3",
        );
    }

    #[test]
    fn mmds_guest_request_records_duplicate_get_token_headers() {
        assert_guest_token_get_duplicate(
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: token-1\r\nX-aws-ec2-metadata-token: token-1\r\n\r\n",
        );
        assert_guest_token_get_duplicate(
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: token-1\r\nX-metadata-token: token-2\r\n\r\n",
        );
    }

    #[test]
    fn mmds_guest_request_rejects_forwarded_for_token_put_header() {
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-Forwarded-For: 127.0.0.1\r\n\r\n",
            ),
            Err(MmdsGuestRequestParseError::UnsupportedForwardedFor)
        );
    }

    #[test]
    fn mmds_guest_request_rejects_token_put_body() {
        assert_eq!(
            MmdsGuestRequest::parse_http(
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\nContent-Length: 4\r\n\r\nbody",
            ),
            Err(MmdsGuestRequestParseError::UnsupportedBody)
        );
    }

    #[test]
    fn mmds_guest_request_feeds_guest_get_response_path() {
        let state = initialized_query_state();
        let request = MmdsGuestRequest::parse_http(
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
        )
        .expect("test MMDS guest HTTP request should parse");
        let MmdsGuestRequest::Get(request) = request else {
            panic!("test MMDS guest HTTP request should be GET");
        };
        let response = state.guest_get_response(request.uri(), request.output_format());

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(
            response.content_type(),
            MmdsGuestContentType::ApplicationJson
        );
        assert_eq!(response.body(), r#""demo.local""#);
    }

    #[test]
    fn mmds_guest_http_response_returns_json_success() {
        let mut state = initialized_query_state();
        let response = state.guest_http_response(
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
        );

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(
            response.content_type(),
            MmdsGuestContentType::ApplicationJson
        );
        assert_eq!(response.body(), r#""demo.local""#);
    }

    #[test]
    fn mmds_guest_http_response_bytes_return_imds_success() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: */*\r\n\r\n"
            ),
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n\r\ndemo.local"
                .to_vec()
        );
    }

    #[test]
    fn mmds_guest_http_response_bytes_preserve_http_10_get_success() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(
                b"GET /meta-data/hostname HTTP/1.0\r\nAccept: */*\r\n\r\n"
            ),
            b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n\r\ndemo.local"
                .to_vec()
        );
    }

    #[test]
    fn mmds_guest_http_response_generates_token_for_put() {
        let mut state = initialized_query_state();
        let response = state.guest_http_response(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        );

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(response.content_type(), MmdsGuestContentType::PlainText);
        assert_mmds_token_shape(response.body());
        assert!(state.is_guest_token_valid(response.body()));
    }

    #[test]
    fn mmds_guest_http_response_bytes_preserve_http_10_token_put_success() {
        let mut state = initialized_query_state();
        let bytes = state.guest_http_response_bytes(
            b"PUT /latest/api/token HTTP/1.0\r\nX-aws-ec2-metadata-token-ttl-seconds: +60\r\n\r\n",
        );
        let response = String::from_utf8(bytes).expect("token response should be UTF-8");
        let (head, token) = response
            .split_once("\r\n\r\n")
            .expect("token response should include header terminator");

        assert_eq!(
            head,
            "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nX-aws-ec2-metadata-token-ttl-seconds: 60\r\nContent-Length: 64"
        );
        assert_mmds_token_shape(token);
        assert!(state.is_guest_token_valid(token));
    }

    #[test]
    fn mmds_guest_http_response_bytes_include_token_ttl_header() {
        let mut state = initialized_query_state();
        let bytes = state.guest_http_response_bytes(
            b"PUT /latest/api/token HTTP/1.1\r\nX-aws-ec2-metadata-token-ttl-seconds: +60\r\n\r\n",
        );
        let response = String::from_utf8(bytes).expect("token response should be UTF-8");
        let (head, token) = response
            .split_once("\r\n\r\n")
            .expect("token response should include header terminator");

        assert_eq!(
            head,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-aws-ec2-metadata-token-ttl-seconds: 60\r\nContent-Length: 64"
        );
        assert_mmds_token_shape(token);
        assert!(state.is_guest_token_valid(token));
    }

    #[test]
    fn mmds_guest_http_response_maps_token_put_errors() {
        for (request, status, body) in [
            (
                b"PUT /latest/api/token HTTP/1.1\r\n\r\n".as_slice(),
                MmdsGuestStatus::BadRequest,
                "Token time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: application/json\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header value is invalid.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: \r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header value is invalid.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 4294967296\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header value is invalid.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header is duplicated.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 0\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "Invalid MMDS token TTL: 0. Please provide a value between 1 and 21600.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 21601\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "Invalid MMDS token TTL: 21601. Please provide a value between 1 and 21600.",
            ),
            (
                b"PUT /wrong HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
                MmdsGuestStatus::NotFound,
                "Resource not found: /wrong.",
            ),
            (
                b"PUT /wrong HTTP/1.1\r\n\r\n",
                MmdsGuestStatus::NotFound,
                "Resource not found: /wrong.",
            ),
            (
                b"PUT /wrong HTTP/1.1\r\nX-metadata-token-ttl-seconds: application/json\r\n\r\n",
                MmdsGuestStatus::NotFound,
                "Resource not found: /wrong.",
            ),
            (
                b"PUT /wrong HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
                MmdsGuestStatus::NotFound,
                "Resource not found: /wrong.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\nX-Forwarded-For: 127.0.0.1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token PUT request does not support X-Forwarded-For.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\nContent-Length: 4\r\n\r\nbody",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request body is not supported.",
            ),
        ] {
            let mut state = initialized_query_state();
            assert_guest_response(
                state.guest_http_response(request),
                status,
                MmdsGuestContentType::PlainText,
                body,
            );
        }
    }

    #[test]
    fn mmds_guest_http_response_bytes_preserve_http_10_errors_with_supported_version() {
        let mut state = MmdsState::default();
        assert_eq!(
            state.guest_http_response_bytes(
                b"GET /meta-data/hostname HTTP/1.0\r\nAccept: application/json\r\n\r\n"
            ),
            b"HTTP/1.0 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 39\r\n\r\nThe MMDS data store is not initialized."
                .to_vec()
        );

        let mut state = initialized_query_state();
        assert_eq!(
            state.guest_http_response_bytes(b"POST /meta-data/hostname HTTP/1.0\r\n\r\n"),
            b"HTTP/1.0 405 Method Not Allowed\r\nContent-Type: text/plain\r\nAllow: GET, PUT\r\nContent-Length: 48\r\n\r\nMMDS guest HTTP request method is not supported."
                .to_vec()
        );

        let mut state = initialized_query_state();
        assert_eq!(
            state.guest_http_response_bytes(
                b"GET /meta-data/hostname HTTP/1.0\r\nAccept: application/xml\r\n\r\n"
            ),
            b"HTTP/1.0 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 55\r\n\r\nMMDS guest HTTP request Accept header is not supported."
                .to_vec()
        );

        let mut state = initialized_query_state();
        assert_eq!(
            state.guest_http_response_bytes(b"PUT /latest/api/token HTTP/1.0\r\n\r\n"),
            b"HTTP/1.0 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 152\r\n\r\nToken time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime."
                .to_vec()
        );
    }

    #[test]
    fn mmds_guest_http_response_token_put_errors_do_not_create_tokens() {
        for request in [
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: application/json\r\n\r\n".as_slice(),
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
            b"PUT /wrong HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\n\r\n",
        ] {
            let mut state = initialized_query_state();
            state.token_authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);

            assert_ne!(
                state.guest_http_response(request).status(),
                MmdsGuestStatus::Ok
            );
            let token = state
                .generate_guest_token(1)
                .expect("failed token PUT should not consume token capacity");
            assert!(state.is_guest_token_valid(&token));
        }
    }

    #[test]
    fn mmds_guest_http_response_default_get_does_not_enforce_tokens() {
        let mut state = initialized_query_state();
        let response = state.guest_http_response(
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
        );

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(response.body(), r#""demo.local""#);
    }

    #[test]
    fn mmds_guest_http_response_v1_get_does_not_enforce_tokens() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\nX-metadata-token: unknown\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\nX-metadata-token: unknown\r\nX-aws-ec2-metadata-token: duplicate\r\n\r\n",
        ] {
            let mut state = initialized_query_state();
            enable_mmds_v1(&mut state);

            assert_guest_response(
                state.guest_http_response(request),
                MmdsGuestStatus::Ok,
                MmdsGuestContentType::ApplicationJson,
                r#""demo.local""#,
            );
        }
    }

    #[test]
    fn mmds_guest_http_response_v2_requires_token() {
        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);

        assert_guest_response(
            state.guest_http_response(
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
            ),
            MmdsGuestStatus::Unauthorized,
            MmdsGuestContentType::PlainText,
            MMDS_GUEST_MISSING_TOKEN,
        );
    }

    #[test]
    fn mmds_guest_http_response_v2_rejects_invalid_tokens() {
        for request in [
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: unknown\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nX-aws-ec2-metadata-token: \r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: unknown\r\nX-aws-ec2-metadata-token: unknown\r\n\r\n",
        ] {
            let mut state = initialized_query_state();
            enable_mmds_v2(&mut state);

            assert_guest_response(
                state.guest_http_response(request),
                MmdsGuestStatus::Unauthorized,
                MmdsGuestContentType::PlainText,
                MMDS_GUEST_INVALID_TOKEN,
            );
        }
    }

    #[test]
    fn mmds_guest_http_response_v2_rejects_expired_token() {
        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);
        state.token_authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
        let token = state
            .generate_guest_token(1)
            .expect("test token generation should succeed");
        state.token_authority.set_now_millis(2_000);
        let request =
            format!("GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: {token}\r\n\r\n");

        assert_guest_response(
            state.guest_http_response(request.as_bytes()),
            MmdsGuestStatus::Unauthorized,
            MmdsGuestContentType::PlainText,
            MMDS_GUEST_INVALID_TOKEN,
        );
    }

    #[test]
    fn mmds_guest_http_response_v2_accepts_valid_tokens() {
        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);
        state.token_authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
        let token = state
            .generate_guest_token(1)
            .expect("test token generation should succeed");
        let request = format!(
            "GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\nX-aws-ec2-metadata-token: {token}\r\n\r\n"
        );

        assert_guest_response(
            state.guest_http_response(request.as_bytes()),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::ApplicationJson,
            r#""demo.local""#,
        );
    }

    #[test]
    fn mmds_guest_http_response_v2_token_errors_do_not_mutate_state() {
        let requests = [
            b"GET /meta-data/hostname HTTP/1.1\r\n\r\n".as_slice(),
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: unknown\r\n\r\n",
            b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: unknown\r\nX-aws-ec2-metadata-token: duplicate\r\n\r\n",
        ];

        for request in requests {
            let mut state = initialized_query_state();
            enable_mmds_v2(&mut state);
            state.token_authority = MmdsTokenAuthority::with_manual_clock(1, 1_000);
            let token = state
                .generate_guest_token(1)
                .expect("test token generation should succeed");
            let original = state.get_data().expect("data store should be initialized");

            assert_eq!(
                state.guest_http_response(request).status(),
                MmdsGuestStatus::Unauthorized
            );
            assert_eq!(state.get_data(), Ok(original));
            assert!(state.is_guest_token_valid(&token));
        }
    }

    #[test]
    fn mmds_guest_http_response_v2_authenticates_before_data_lookup() {
        let mut state = MmdsState::default();
        enable_mmds_v2(&mut state);

        assert_guest_response(
            state.guest_http_response(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n"),
            MmdsGuestStatus::Unauthorized,
            MmdsGuestContentType::PlainText,
            MMDS_GUEST_MISSING_TOKEN,
        );

        let token = state
            .generate_guest_token(1)
            .expect("test token generation should succeed");
        let request =
            format!("GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: {token}\r\n\r\n");

        assert_guest_response(
            state.guest_http_response(request.as_bytes()),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "The MMDS data store is not initialized.",
        );
    }

    #[test]
    fn mmds_guest_http_response_bytes_serialize_missing_v2_token() {
        let mut state = initialized_query_state();
        enable_mmds_v2(&mut state);
        let expected = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            MMDS_GUEST_MISSING_TOKEN.len(),
            MMDS_GUEST_MISSING_TOKEN
        )
        .into_bytes();

        assert_eq!(
            state.guest_http_response_bytes(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n"),
            expected
        );
    }

    #[test]
    fn mmds_guest_http_response_maps_uninitialized_store() {
        let mut state = MmdsState::default();

        assert_guest_response(
            state.guest_http_response(
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n",
            ),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "The MMDS data store is not initialized.",
        );
    }

    #[test]
    fn mmds_guest_http_response_maps_parse_errors() {
        for (request, status, body) in [
            (
                b"GET /meta-data/host\xffname HTTP/1.1\r\n\r\n".as_slice(),
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request is not valid UTF-8.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1 extra\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request is malformed.",
            ),
            (
                b"POST /meta-data/hostname HTTP/1.1\r\n\r\n",
                MmdsGuestStatus::MethodNotAllowed,
                "MMDS guest HTTP request method is not supported.",
            ),
            (
                b"GET /meta-data/hostname HTTP/2\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request version is not supported.",
            ),
            (
                b"GET * HTTP/1.1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "Invalid URI.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nBad Header: value\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request header is malformed.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request has duplicate Content-Length headers.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: +0\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request Content-Length is invalid.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request Transfer-Encoding is not supported.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request body is not supported.",
            ),
            (
                b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/xml\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest HTTP request Accept header is not supported.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "Token time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: abc\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header value is invalid.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-aws-ec2-metadata-token-ttl-seconds: 1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token TTL header is duplicated.",
            ),
            (
                b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 1\r\nX-Forwarded-For: 127.0.0.1\r\n\r\n",
                MmdsGuestStatus::BadRequest,
                "MMDS guest token PUT request does not support X-Forwarded-For.",
            ),
        ] {
            assert_guest_http_response(
                request,
                status,
                MmdsGuestContentType::PlainText,
                body,
            );
        }
    }

    #[test]
    fn mmds_guest_http_response_bytes_serialize_parse_error() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(b"GET /meta-data/hostname\r\n\r\n"),
            b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 37\r\n\r\nMMDS guest HTTP request is malformed."
                .to_vec()
        );
    }

    #[test]
    fn mmds_guest_http_response_bytes_keep_default_version_for_unsupported_version_error() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(b"GET /meta-data/hostname HTTP/2\r\n\r\n"),
            b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 49\r\n\r\nMMDS guest HTTP request version is not supported."
                .to_vec()
        );
    }

    #[test]
    fn mmds_guest_http_response_bytes_serialize_method_not_allowed() {
        let mut state = initialized_query_state();

        assert_eq!(
            state.guest_http_response_bytes(b"POST /meta-data/hostname HTTP/1.1\r\n\r\n"),
            b"HTTP/1.1 405 Method Not Allowed\r\nContent-Type: text/plain\r\nAllow: GET, PUT\r\nContent-Length: 48\r\n\r\nMMDS guest HTTP request method is not supported."
                .to_vec()
        );
    }

    #[test]
    fn mmds_guest_http_response_parse_error_does_not_mutate_data_store() {
        let mut state = initialized_query_state();
        let original = state.get_data().expect("data store should be initialized");

        assert_guest_response(
            state.guest_http_response(
                b"GET /meta-data/hostname HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody",
            ),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "MMDS guest HTTP request body is not supported.",
        );
        assert_eq!(state.get_data(), Ok(original));
    }

    #[test]
    fn mmds_guest_request_parse_errors_display_deterministic_messages() {
        assert_eq!(
            MmdsGuestRequestParseError::InvalidUtf8.to_string(),
            "MMDS guest HTTP request is not valid UTF-8."
        );
        assert_eq!(
            MmdsGuestRequestParseError::MalformedRequest.to_string(),
            "MMDS guest HTTP request is malformed."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedMethod.to_string(),
            "MMDS guest HTTP request method is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedHttpVersion.to_string(),
            "MMDS guest HTTP request version is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::InvalidUri.to_string(),
            "Invalid URI."
        );
        assert_eq!(
            MmdsGuestRequestParseError::MalformedHeader.to_string(),
            "MMDS guest HTTP request header is malformed."
        );
        assert_eq!(
            MmdsGuestRequestParseError::DuplicateContentLength.to_string(),
            "MMDS guest HTTP request has duplicate Content-Length headers."
        );
        assert_eq!(
            MmdsGuestRequestParseError::InvalidContentLength.to_string(),
            "MMDS guest HTTP request Content-Length is invalid."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedTransferEncoding.to_string(),
            "MMDS guest HTTP request Transfer-Encoding is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedBody.to_string(),
            "MMDS guest HTTP request body is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedAccept.to_string(),
            "MMDS guest HTTP request Accept header is not supported."
        );
        assert_eq!(
            MmdsGuestRequestParseError::MissingToken.to_string(),
            MMDS_GUEST_MISSING_TOKEN
        );
        assert_eq!(
            MmdsGuestRequestParseError::InvalidToken.to_string(),
            MMDS_GUEST_INVALID_TOKEN
        );
        assert_eq!(
            MmdsGuestRequestParseError::MissingTokenTtl.to_string(),
            "Token time to live value not found. Use `X-metadata-token-ttl-seconds` or `X-aws-ec2-metadata-token-ttl-seconds` header to specify the token's lifetime."
        );
        assert_eq!(
            MmdsGuestRequestParseError::InvalidTokenTtl.to_string(),
            "MMDS guest token TTL header value is invalid."
        );
        assert_eq!(
            MmdsGuestRequestParseError::DuplicateTokenTtl.to_string(),
            "MMDS guest token TTL header is duplicated."
        );
        assert_eq!(
            MmdsGuestRequestParseError::UnsupportedForwardedFor.to_string(),
            "MMDS guest token PUT request does not support X-Forwarded-For."
        );
    }

    #[test]
    fn guest_get_response_returns_json_body() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/", MmdsOutputFormat::Json);

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(
            response.content_type(),
            MmdsGuestContentType::ApplicationJson
        );
        assert_json_value(response.body(), query_value());
    }

    #[test]
    fn guest_get_response_returns_imds_body() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("/", MmdsOutputFormat::Imds),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "age\nmember\nmeta-data/\nnothing\nphones\nuser-data",
        );
    }

    #[test]
    fn guest_get_response_imds_compat_forces_plain_text_response() {
        let mut state = initialized_query_state();
        enable_imds_compat(&mut state);

        assert_guest_response(
            state.guest_get_response("/meta-data", MmdsOutputFormat::Json),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "ami-id\nhostname",
        );
    }

    #[test]
    fn guest_get_response_rejects_empty_uri() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("", MmdsOutputFormat::Json),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "Invalid URI.",
        );
    }

    #[test]
    fn guest_get_response_uses_original_uri_in_missing_path_body() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("//meta-data//missing", MmdsOutputFormat::Json),
            MmdsGuestStatus::NotFound,
            MmdsGuestContentType::PlainText,
            "Resource not found: //meta-data//missing.",
        );
    }

    #[test]
    fn guest_get_response_maps_unsupported_imds_value_type() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("/age", MmdsOutputFormat::Imds),
            MmdsGuestStatus::NotImplemented,
            MmdsGuestContentType::PlainText,
            "Cannot retrieve value. The value has an unsupported type.",
        );
    }

    #[test]
    fn guest_get_response_sanitizes_repeated_slashes_for_lookup() {
        let state = initialized_query_state();

        assert_guest_response(
            state.guest_get_response("//meta-data//hostname", MmdsOutputFormat::Imds),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "demo.local",
        );
    }

    #[test]
    fn guest_get_response_sanitizes_slash_only_uri_to_root() {
        let state = initialized_query_state();
        let response = state.guest_get_response("////", MmdsOutputFormat::Json);

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(
            response.content_type(),
            MmdsGuestContentType::ApplicationJson
        );
        assert_json_value(response.body(), query_value());
    }

    #[test]
    fn guest_get_response_maps_uninitialized_store() {
        let state = MmdsState::default();

        assert_guest_response(
            state.guest_get_response("/", MmdsOutputFormat::Json),
            MmdsGuestStatus::BadRequest,
            MmdsGuestContentType::PlainText,
            "The MMDS data store is not initialized.",
        );
    }

    #[test]
    fn guest_get_response_does_not_mutate_data_store() {
        let state = initialized_query_state();
        let original = state.get_data().expect("data store should be initialized");

        assert_guest_response(
            state.guest_get_response("/meta-data", MmdsOutputFormat::Imds),
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "ami-id\nhostname",
        );
        assert_eq!(state.get_data(), Ok(original));
    }

    #[test]
    fn guest_response_http_bytes_serialize_json_success() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/meta-data/hostname", MmdsOutputFormat::Json);

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12\r\n\r\n\"demo.local\""
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_serialize_imds_success() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/meta-data/hostname", MmdsOutputFormat::Imds);

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n\r\ndemo.local"
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_serialize_not_found_error() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/missing", MmdsOutputFormat::Json);

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 29\r\n\r\nResource not found: /missing."
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_serialize_not_implemented_error() {
        let state = initialized_query_state();
        let response = state.guest_get_response("/age", MmdsOutputFormat::Imds);

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 501 Not Implemented\r\nContent-Type: text/plain\r\nContent-Length: 57\r\n\r\nCannot retrieve value. The value has an unsupported type."
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_use_body_byte_length() {
        let response = MmdsGuestResponse::new(
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            "héllo".to_string(),
        );

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 6\r\n\r\nh\xc3\xa9llo"
                .to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_allow_empty_body() {
        let response = MmdsGuestResponse::new(
            MmdsGuestStatus::Ok,
            MmdsGuestContentType::PlainText,
            String::new(),
        );

        assert_eq!(
            response.to_http_bytes(),
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 0\r\n\r\n".to_vec()
        );
    }

    #[test]
    fn guest_response_http_bytes_do_not_mutate_response_or_data_store() {
        let state = initialized_query_state();
        let original = state.get_data().expect("data store should be initialized");
        let response = state.guest_get_response("/meta-data/hostname", MmdsOutputFormat::Imds);
        let first_bytes = response.to_http_bytes();

        assert_eq!(response.status(), MmdsGuestStatus::Ok);
        assert_eq!(response.content_type(), MmdsGuestContentType::PlainText);
        assert_eq!(response.body(), "demo.local");
        assert_eq!(response.to_http_bytes(), first_bytes);
        assert_eq!(state.get_data(), Ok(original));
    }
}
