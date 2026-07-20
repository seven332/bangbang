//! Closed native-endian vhost-user message codec.

use std::convert::TryInto;

pub(crate) const HEADER_BYTES: usize = 12;
pub(crate) const MAX_BODY_BYTES: usize = 0x1000;
pub(crate) const MAX_ATTACHED_FDS: usize = 32;

const VERSION_MASK: u32 = 0x3;
const VERSION_ONE: u32 = 0x1;
const REPLY_FLAG: u32 = 0x4;
const NEED_REPLY_FLAG: u32 = 0x8;
const KNOWN_FLAGS: u32 = VERSION_MASK | REPLY_FLAG | NEED_REPLY_FLAG;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageError {
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    GetFeatures,
    SetFeatures,
    SetOwner,
    SetMemoryTable,
    SetVringNum,
    SetVringAddr,
    SetVringBase,
    SetVringKick,
    SetVringCall,
    GetProtocolFeatures,
    SetProtocolFeatures,
    SetVringEnable,
    GetConfig,
}

impl Request {
    pub(crate) const fn code(self) -> u32 {
        match self {
            Self::GetFeatures => 1,
            Self::SetFeatures => 2,
            Self::SetOwner => 3,
            Self::SetMemoryTable => 5,
            Self::SetVringNum => 8,
            Self::SetVringAddr => 9,
            Self::SetVringBase => 10,
            Self::SetVringKick => 12,
            Self::SetVringCall => 13,
            Self::GetProtocolFeatures => 15,
            Self::SetProtocolFeatures => 16,
            Self::SetVringEnable => 18,
            Self::GetConfig => 24,
        }
    }

    fn from_code(code: u32) -> Result<Self, MessageError> {
        match code {
            1 => Ok(Self::GetFeatures),
            2 => Ok(Self::SetFeatures),
            3 => Ok(Self::SetOwner),
            5 => Ok(Self::SetMemoryTable),
            8 => Ok(Self::SetVringNum),
            9 => Ok(Self::SetVringAddr),
            10 => Ok(Self::SetVringBase),
            12 => Ok(Self::SetVringKick),
            13 => Ok(Self::SetVringCall),
            15 => Ok(Self::GetProtocolFeatures),
            16 => Ok(Self::SetProtocolFeatures),
            18 => Ok(Self::SetVringEnable),
            24 => Ok(Self::GetConfig),
            _ => Err(MessageError::Invalid),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) request: Request,
    pub(crate) body_size: usize,
    pub(crate) need_reply: bool,
    pub(crate) is_reply: bool,
}

impl Header {
    pub(crate) fn request(
        request: Request,
        body_size: usize,
        need_reply: bool,
    ) -> Result<Self, MessageError> {
        if body_size > MAX_BODY_BYTES {
            return Err(MessageError::Invalid);
        }
        Ok(Self {
            request,
            body_size,
            need_reply,
            is_reply: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn reply(request: Request, body_size: usize) -> Result<Self, MessageError> {
        if body_size > MAX_BODY_BYTES {
            return Err(MessageError::Invalid);
        }
        Ok(Self {
            request,
            body_size,
            need_reply: false,
            is_reply: true,
        })
    }

    pub(crate) fn encode(self) -> Result<[u8; HEADER_BYTES], MessageError> {
        let mut encoded = [0_u8; HEADER_BYTES];
        let flags = VERSION_ONE
            | if self.is_reply { REPLY_FLAG } else { 0 }
            | if self.need_reply { NEED_REPLY_FLAG } else { 0 };
        write_u32(&mut encoded, 0, self.request.code());
        write_u32(&mut encoded, 4, flags);
        write_u32(
            &mut encoded,
            8,
            u32::try_from(self.body_size).map_err(|_| MessageError::Invalid)?,
        );
        Ok(encoded)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        if bytes.len() != HEADER_BYTES {
            return Err(MessageError::Invalid);
        }
        let request = Request::from_code(read_u32(bytes, 0)?)?;
        let flags = read_u32(bytes, 4)?;
        if flags & VERSION_MASK != VERSION_ONE || flags & !KNOWN_FLAGS != 0 {
            return Err(MessageError::Invalid);
        }
        let is_reply = flags & REPLY_FLAG != 0;
        let need_reply = flags & NEED_REPLY_FLAG != 0;
        if is_reply && need_reply {
            return Err(MessageError::Invalid);
        }
        let body_size = usize::try_from(read_u32(bytes, 8)?).map_err(|_| MessageError::Invalid)?;
        if body_size > MAX_BODY_BYTES {
            return Err(MessageError::Invalid);
        }
        Ok(Self {
            request,
            body_size,
            need_reply,
            is_reply,
        })
    }
}

pub(crate) fn frame(
    request: Request,
    body: &[u8],
    need_reply: bool,
) -> Result<Vec<u8>, MessageError> {
    let header = Header::request(request, body.len(), need_reply)?;
    let mut encoded = Vec::with_capacity(HEADER_BYTES + body.len());
    encoded.extend_from_slice(&header.encode()?);
    encoded.extend_from_slice(body);
    Ok(encoded)
}

#[cfg(test)]
pub(crate) fn reply_frame(request: Request, body: &[u8]) -> Result<Vec<u8>, MessageError> {
    let header = Header::reply(request, body.len())?;
    let mut encoded = Vec::with_capacity(HEADER_BYTES + body.len());
    encoded.extend_from_slice(&header.encode()?);
    encoded.extend_from_slice(body);
    Ok(encoded)
}

pub(crate) fn encode_u64(value: u64) -> Vec<u8> {
    value.to_ne_bytes().to_vec()
}

pub(crate) fn decode_u64(bytes: &[u8]) -> Result<u64, MessageError> {
    let array: [u8; 8] = bytes.try_into().map_err(|_| MessageError::Invalid)?;
    Ok(u64::from_ne_bytes(array))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryRegionWire {
    pub(crate) guest_phys_addr: u64,
    pub(crate) memory_size: u64,
    pub(crate) userspace_addr: u64,
    pub(crate) mmap_offset: u64,
}

pub(crate) fn encode_memory_table(regions: &[MemoryRegionWire]) -> Result<Vec<u8>, MessageError> {
    if regions.is_empty() || regions.len() > MAX_ATTACHED_FDS {
        return Err(MessageError::Invalid);
    }
    let body_size = 8_usize
        .checked_add(regions.len().checked_mul(32).ok_or(MessageError::Invalid)?)
        .ok_or(MessageError::Invalid)?;
    if body_size > MAX_BODY_BYTES {
        return Err(MessageError::Invalid);
    }
    let mut encoded = Vec::with_capacity(body_size);
    append_u32(
        &mut encoded,
        u32::try_from(regions.len()).map_err(|_| MessageError::Invalid)?,
    );
    append_u32(&mut encoded, 0);
    for region in regions {
        append_u64(&mut encoded, region.guest_phys_addr);
        append_u64(&mut encoded, region.memory_size);
        append_u64(&mut encoded, region.userspace_addr);
        append_u64(&mut encoded, region.mmap_offset);
    }
    Ok(encoded)
}

pub(crate) fn encode_vring_state(index: u32, value: u32) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(8);
    append_u32(&mut encoded, index);
    append_u32(&mut encoded, value);
    encoded
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VringAddressWire {
    pub(crate) index: u32,
    pub(crate) descriptor: u64,
    pub(crate) used: u64,
    pub(crate) available: u64,
}

pub(crate) fn encode_vring_address(address: VringAddressWire) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(40);
    append_u32(&mut encoded, address.index);
    append_u32(&mut encoded, 0);
    append_u64(&mut encoded, address.descriptor);
    append_u64(&mut encoded, address.used);
    append_u64(&mut encoded, address.available);
    append_u64(&mut encoded, 0);
    encoded
}

pub(crate) fn encode_config_request(offset: u32, size: u32, flags: u32) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(12_usize.saturating_add(size as usize));
    append_u32(&mut encoded, offset);
    append_u32(&mut encoded, size);
    append_u32(&mut encoded, flags);
    encoded.resize(12_usize.saturating_add(size as usize), 0);
    encoded
}

pub(crate) fn decode_config_reply(
    bytes: &[u8],
    expected_offset: u32,
    expected_size: u32,
    expected_flags: u32,
) -> Result<Option<Vec<u8>>, MessageError> {
    let data_size = usize::try_from(expected_size).map_err(|_| MessageError::Invalid)?;
    let expected_total = 12_usize
        .checked_add(data_size)
        .ok_or(MessageError::Invalid)?;
    if read_u32(bytes, 0)? != expected_offset || read_u32(bytes, 8)? != expected_flags {
        return Err(MessageError::Invalid);
    }
    let actual_size = read_u32(bytes, 4)?;
    if actual_size == 0 && bytes.len() == 12 {
        return Ok(None);
    }
    if actual_size != expected_size || bytes.len() != expected_total {
        return Err(MessageError::Invalid);
    }
    bytes
        .get(12..)
        .map(ToOwned::to_owned)
        .map(Some)
        .ok_or(MessageError::Invalid)
}

fn append_u32(encoded: &mut Vec<u8>, value: u32) {
    encoded.extend_from_slice(&value.to_ne_bytes());
}

fn append_u64(encoded: &mut Vec<u8>, value: u64) {
    encoded.extend_from_slice(&value.to_ne_bytes());
}

fn write_u32(encoded: &mut [u8], offset: usize, value: u32) {
    if let Some(destination) = encoded.get_mut(offset..offset.saturating_add(4)) {
        destination.copy_from_slice(&value.to_ne_bytes());
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, MessageError> {
    let source = bytes
        .get(offset..offset.checked_add(4).ok_or(MessageError::Invalid)?)
        .ok_or(MessageError::Invalid)?;
    let array: [u8; 4] = source.try_into().map_err(|_| MessageError::Invalid)?;
    Ok(u32::from_ne_bytes(array))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_ne_bytes(chunk.try_into().expect("word should be exact")))
            .collect()
    }

    #[test]
    fn request_ids_match_the_pinned_subset() {
        let requests = [
            (Request::GetFeatures, 1),
            (Request::SetFeatures, 2),
            (Request::SetOwner, 3),
            (Request::SetMemoryTable, 5),
            (Request::SetVringNum, 8),
            (Request::SetVringAddr, 9),
            (Request::SetVringBase, 10),
            (Request::SetVringKick, 12),
            (Request::SetVringCall, 13),
            (Request::GetProtocolFeatures, 15),
            (Request::SetProtocolFeatures, 16),
            (Request::SetVringEnable, 18),
            (Request::GetConfig, 24),
        ];
        for (request, code) in requests {
            assert_eq!(request.code(), code);
            assert_eq!(Request::from_code(code), Ok(request));
        }
        assert_eq!(Request::from_code(4), Err(MessageError::Invalid));
        assert_eq!(Request::from_code(25), Err(MessageError::Invalid));
    }

    #[test]
    fn headers_use_exact_native_endian_flags_and_sizes() {
        let request =
            Header::request(Request::SetFeatures, 8, true).expect("request header should encode");
        let request_bytes = request.encode().expect("request header should encode");
        assert_eq!(words(&request_bytes), vec![2, 0x9, 8]);
        assert_eq!(Header::decode(&request_bytes), Ok(request));

        let reply = Header::reply(Request::GetFeatures, 8).expect("reply header should encode");
        let reply_bytes = reply.encode().expect("reply header should encode");
        assert_eq!(words(&reply_bytes), vec![1, 0x5, 8]);
        assert_eq!(Header::decode(&reply_bytes), Ok(reply));
    }

    #[test]
    fn headers_reject_unknown_flags_versions_ids_and_lengths() {
        for (request, flags, size) in [
            (99, 1, 0),
            (1, 0, 0),
            (1, 2, 0),
            (1, 0x11, 0),
            (1, 0xd, 8),
            (1, 1, 0x1001),
        ] {
            let mut bytes = Vec::new();
            append_u32(&mut bytes, request);
            append_u32(&mut bytes, flags);
            append_u32(&mut bytes, size);
            assert_eq!(Header::decode(&bytes), Err(MessageError::Invalid));
        }
        assert_eq!(
            Header::decode(&[0; HEADER_BYTES - 1]),
            Err(MessageError::Invalid)
        );
    }

    #[test]
    fn memory_table_has_exact_zero_padding_and_region_order() {
        let regions = [
            MemoryRegionWire {
                guest_phys_addr: 0x1000,
                memory_size: 0x2000,
                userspace_addr: 0x3000,
                mmap_offset: 0x4000,
            },
            MemoryRegionWire {
                guest_phys_addr: 0x5000,
                memory_size: 0x6000,
                userspace_addr: 0x7000,
                mmap_offset: 0x8000,
            },
        ];
        let encoded = encode_memory_table(&regions).expect("memory table should encode");
        assert_eq!(encoded.len(), 72);
        assert_eq!(
            words(encoded.get(..8).expect("header should exist")),
            vec![2, 0]
        );
        assert_eq!(
            encoded.get(8..16),
            Some(0x1000_u64.to_ne_bytes().as_slice())
        );
        assert_eq!(
            encoded.get(64..72),
            Some(0x8000_u64.to_ne_bytes().as_slice())
        );
    }

    #[test]
    fn vring_and_config_payloads_have_exact_layouts() {
        assert_eq!(words(&encode_vring_state(7, 256)), vec![7, 256]);
        let address = encode_vring_address(VringAddressWire {
            index: 3,
            descriptor: 0x1000,
            used: 0x2000,
            available: 0x3000,
        });
        assert_eq!(address.len(), 40);
        assert_eq!(
            words(address.get(..8).expect("header should exist")),
            vec![3, 0]
        );
        assert_eq!(address.get(32..40), Some(0_u64.to_ne_bytes().as_slice()));

        let config = encode_config_request(0, 4, 1);
        assert_eq!(words(&config), vec![0, 4, 1, 0]);
        let mut reply = config;
        reply
            .get_mut(12..16)
            .expect("config payload should exist")
            .copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(
            decode_config_reply(&reply, 0, 4, 1),
            Ok(Some(vec![1, 2, 3, 4]))
        );
        assert_eq!(
            decode_config_reply(&reply, 1, 4, 1),
            Err(MessageError::Invalid)
        );
    }
}
