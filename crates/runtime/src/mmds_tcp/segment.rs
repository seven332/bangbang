// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Safe TCP segment values adapted from Firecracker v1.16.0 `dumbo/pdu/tcp.rs`.

use std::fmt;
use std::ops::{BitOr, BitOrAssign};

use super::MIN_MSS;

const MIN_HEADER_LEN: usize = 20;
const MAX_HEADER_LEN: usize = 60;
const SOURCE_PORT_OFFSET: usize = 0;
const DESTINATION_PORT_OFFSET: usize = 2;
const SEQUENCE_NUMBER_OFFSET: usize = 4;
const ACKNOWLEDGEMENT_NUMBER_OFFSET: usize = 8;
const DATA_OFFSET_OFFSET: usize = 12;
const FLAGS_OFFSET: usize = 13;
const WINDOW_SIZE_OFFSET: usize = 14;
const OPTIONS_OFFSET: usize = 20;
const OPTION_END: u8 = 0;
const OPTION_NOP: u8 = 1;
const OPTION_MSS: u8 = 2;
const OPTION_MSS_LEN: usize = 4;

/// TCP flags carried in the second control byte.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TcpFlags(u8);

impl TcpFlags {
    /// Congestion-window-reduced flag.
    pub const CWR: Self = Self(1 << 7);
    /// ECN-echo flag.
    pub const ECE: Self = Self(1 << 6);
    /// Urgent-pointer flag.
    pub const URGENT: Self = Self(1 << 5);
    /// Acknowledgement-number-valid flag.
    pub const ACK: Self = Self(1 << 4);
    /// Push flag.
    pub const PUSH: Self = Self(1 << 3);
    /// Reset flag.
    pub const RESET: Self = Self(1 << 2);
    /// Synchronize flag.
    pub const SYNCHRONIZE: Self = Self(1 << 1);
    /// Finish flag.
    pub const FINISH: Self = Self(1);

    /// Creates flags from the raw control byte.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the raw control byte.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns whether every supplied flag is present.
    pub const fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }

    /// Returns whether any supplied flag is present.
    pub const fn intersects(self, flags: Self) -> bool {
        self.0 & flags.0 != 0
    }

    /// Returns the union of two flag sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOr for TcpFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl BitOrAssign for TcpFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

/// A validated, borrowed TCP segment.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TcpSegment<'a> {
    source_port: u16,
    destination_port: u16,
    sequence_number: u32,
    acknowledgement_number: u32,
    flags: TcpFlags,
    window_size: u16,
    maximum_segment_size: Option<u16>,
    payload: &'a [u8],
}

impl fmt::Debug for TcpSegment<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TcpSegment")
            .field("source_port", &self.source_port)
            .field("destination_port", &self.destination_port)
            .field("sequence_number", &self.sequence_number)
            .field("acknowledgement_number", &self.acknowledgement_number)
            .field("flags", &self.flags)
            .field("window_size", &self.window_size)
            .field("maximum_segment_size", &self.maximum_segment_size)
            .field("payload", &"[REDACTED]")
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

impl<'a> TcpSegment<'a> {
    /// Parses a complete TCP segment without validating its checksum.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, SegmentParseError> {
        if bytes.len() > usize::from(u16::MAX) {
            return Err(SegmentParseError::SegmentTooLong { len: bytes.len() });
        }
        if bytes.len() < MIN_HEADER_LEN {
            return Err(SegmentParseError::SliceTooShort { len: bytes.len() });
        }

        let data_offset = bytes
            .get(DATA_OFFSET_OFFSET)
            .copied()
            .map(|value| usize::from(value >> 4) * 4)
            .ok_or(SegmentParseError::SliceTooShort { len: bytes.len() })?;
        if !(MIN_HEADER_LEN..=MAX_HEADER_LEN).contains(&data_offset) || data_offset > bytes.len() {
            return Err(SegmentParseError::HeaderLength {
                header_len: data_offset,
                segment_len: bytes.len(),
            });
        }

        let maximum_segment_size = parse_options(bytes.get(OPTIONS_OFFSET..data_offset).ok_or(
            SegmentParseError::HeaderLength {
                header_len: data_offset,
                segment_len: bytes.len(),
            },
        )?)?;
        let payload = bytes
            .get(data_offset..)
            .ok_or(SegmentParseError::HeaderLength {
                header_len: data_offset,
                segment_len: bytes.len(),
            })?;

        Ok(Self {
            source_port: read_u16(bytes, SOURCE_PORT_OFFSET)?,
            destination_port: read_u16(bytes, DESTINATION_PORT_OFFSET)?,
            sequence_number: read_u32(bytes, SEQUENCE_NUMBER_OFFSET)?,
            acknowledgement_number: read_u32(bytes, ACKNOWLEDGEMENT_NUMBER_OFFSET)?,
            flags: TcpFlags::from_bits(
                bytes
                    .get(FLAGS_OFFSET)
                    .copied()
                    .ok_or(SegmentParseError::SliceTooShort { len: bytes.len() })?,
            ),
            window_size: read_u16(bytes, WINDOW_SIZE_OFFSET)?,
            maximum_segment_size,
            payload,
        })
    }

    /// Source TCP port.
    pub const fn source_port(self) -> u16 {
        self.source_port
    }

    /// Destination TCP port.
    pub const fn destination_port(self) -> u16 {
        self.destination_port
    }

    /// First sequence number carried by the segment.
    pub const fn sequence_number(self) -> u32 {
        self.sequence_number
    }

    /// Acknowledgement number carried by the segment.
    pub const fn acknowledgement_number(self) -> u32 {
        self.acknowledgement_number
    }

    /// TCP control flags.
    pub const fn flags(self) -> TcpFlags {
        self.flags
    }

    /// Advertised receive-window size.
    pub const fn window_size(self) -> u16 {
        self.window_size
    }

    /// Parsed MSS option, if present.
    pub const fn maximum_segment_size(self) -> Option<u16> {
        self.maximum_segment_size
    }

    /// Borrowed TCP payload.
    pub const fn payload(self) -> &'a [u8] {
        self.payload
    }
}

/// Metadata for one outgoing TCP segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutgoingSegment {
    sequence_number: u32,
    acknowledgement_number: u32,
    flags: TcpFlags,
    window_size: u16,
    maximum_segment_size: Option<u16>,
    payload_len: usize,
}

impl OutgoingSegment {
    pub(crate) const fn new(
        sequence_number: u32,
        acknowledgement_number: u32,
        flags: TcpFlags,
        window_size: u16,
        maximum_segment_size: Option<u16>,
        payload_len: usize,
    ) -> Self {
        Self {
            sequence_number,
            acknowledgement_number,
            flags,
            window_size,
            maximum_segment_size,
            payload_len,
        }
    }

    /// First sequence number carried by the segment.
    pub const fn sequence_number(self) -> u32 {
        self.sequence_number
    }

    /// Acknowledgement number carried by the segment.
    pub const fn acknowledgement_number(self) -> u32 {
        self.acknowledgement_number
    }

    /// TCP control flags.
    pub const fn flags(self) -> TcpFlags {
        self.flags
    }

    /// Advertised receive-window size.
    pub const fn window_size(self) -> u16 {
        self.window_size
    }

    /// MSS option to encode, if present.
    pub const fn maximum_segment_size(self) -> Option<u16> {
        self.maximum_segment_size
    }

    /// Bytes written into the caller-provided payload buffer.
    pub const fn payload_len(self) -> usize {
        self.payload_len
    }
}

/// Error returned while parsing a raw TCP segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentParseError {
    /// The slice is shorter than a basic TCP header.
    SliceTooShort { len: usize },
    /// The segment cannot fit in an IPv4 packet.
    SegmentTooLong { len: usize },
    /// The encoded header length is invalid.
    HeaderLength {
        header_len: usize,
        segment_len: usize,
    },
    /// An option is missing its length byte.
    TruncatedOption { offset: usize },
    /// An option length is smaller than its kind/length prefix.
    InvalidOptionLength { offset: usize, len: usize },
    /// An option extends past the validated TCP header.
    TruncatedOptionValue {
        offset: usize,
        len: usize,
        options_len: usize,
    },
    /// An MSS option has an invalid encoded length.
    InvalidMssLength { len: usize },
    /// An MSS option advertises a value below the supported minimum.
    InvalidMssValue { value: u16 },
    /// More than one MSS option was supplied.
    DuplicateMss,
}

impl fmt::Display for SegmentParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SliceTooShort { len } => {
                write!(formatter, "TCP segment is only {len} bytes")
            }
            Self::SegmentTooLong { len } => {
                write!(formatter, "TCP segment length {len} exceeds the IPv4 limit")
            }
            Self::HeaderLength {
                header_len,
                segment_len,
            } => write!(
                formatter,
                "TCP header length {header_len} is invalid for a {segment_len}-byte segment"
            ),
            Self::TruncatedOption { offset } => {
                write!(formatter, "TCP option at offset {offset} has no length")
            }
            Self::InvalidOptionLength { offset, len } => write!(
                formatter,
                "TCP option at offset {offset} has invalid length {len}"
            ),
            Self::TruncatedOptionValue {
                offset,
                len,
                options_len,
            } => write!(
                formatter,
                "TCP option at offset {offset} with length {len} exceeds {options_len} option bytes"
            ),
            Self::InvalidMssLength { len } => {
                write!(formatter, "TCP MSS option has invalid length {len}")
            }
            Self::InvalidMssValue { value } => {
                write!(formatter, "TCP MSS value {value} is below {MIN_MSS}")
            }
            Self::DuplicateMss => formatter.write_str("TCP segment contains duplicate MSS options"),
        }
    }
}

impl std::error::Error for SegmentParseError {}

fn parse_options(options: &[u8]) -> Result<Option<u16>, SegmentParseError> {
    let mut maximum_segment_size = None;
    let mut offset = 0;
    while offset < options.len() {
        let Some(&kind) = options.get(offset) else {
            break;
        };
        match kind {
            OPTION_END => break,
            OPTION_NOP => {
                offset += 1;
            }
            _ => {
                let len = options
                    .get(offset + 1)
                    .copied()
                    .map(usize::from)
                    .ok_or(SegmentParseError::TruncatedOption { offset })?;
                if len < 2 {
                    return Err(SegmentParseError::InvalidOptionLength { offset, len });
                }
                let end =
                    offset
                        .checked_add(len)
                        .ok_or(SegmentParseError::TruncatedOptionValue {
                            offset,
                            len,
                            options_len: options.len(),
                        })?;
                let option =
                    options
                        .get(offset..end)
                        .ok_or(SegmentParseError::TruncatedOptionValue {
                            offset,
                            len,
                            options_len: options.len(),
                        })?;
                if kind == OPTION_MSS {
                    if len != OPTION_MSS_LEN {
                        return Err(SegmentParseError::InvalidMssLength { len });
                    }
                    if maximum_segment_size.is_some() {
                        return Err(SegmentParseError::DuplicateMss);
                    }
                    let value = u16::from_be_bytes([
                        *option
                            .get(2)
                            .ok_or(SegmentParseError::InvalidMssLength { len })?,
                        *option
                            .get(3)
                            .ok_or(SegmentParseError::InvalidMssLength { len })?,
                    ]);
                    if value < MIN_MSS {
                        return Err(SegmentParseError::InvalidMssValue { value });
                    }
                    maximum_segment_size = Some(value);
                }
                offset = end;
            }
        }
    }
    Ok(maximum_segment_size)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, SegmentParseError> {
    let value = bytes
        .get(offset..offset + 2)
        .and_then(|slice| <[u8; 2]>::try_from(slice).ok())
        .ok_or(SegmentParseError::SliceTooShort { len: bytes.len() })?;
    Ok(u16::from_be_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, SegmentParseError> {
    let value = bytes
        .get(offset..offset + 4)
        .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        .ok_or(SegmentParseError::SliceTooShort { len: bytes.len() })?;
    Ok(u32::from_be_bytes(value))
}
