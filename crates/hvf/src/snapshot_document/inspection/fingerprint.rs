use std::fmt;

use serde::Serialize;
use serde::ser::SerializeStruct;
use sha2::{Digest, Sha256};

use super::SCHEMA;

pub(super) const REDACTED: &str = "<redacted>";

macro_rules! hex_scalar {
    ($name:ident, $inner:ty, $width:literal) => {
        pub(super) struct $name(pub(super) $inner);

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!("0x{:0", $width, "x}"), self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.collect_str(self)
            }
        }
    };
}

hex_scalar!(HexU8, u8, 2);
hex_scalar!(HexU16, u16, 4);
hex_scalar!(HexU32, u32, 8);
hex_scalar!(HexU64, u64, 16);
hex_scalar!(HexU128, u128, 32);

pub(super) struct Redacted;

impl Serialize for Redacted {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(REDACTED)
    }
}

pub(super) struct RedactedOption(pub(super) bool);

impl Serialize for RedactedOption {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.0 {
            serializer.serialize_str(REDACTED)
        } else {
            serializer.serialize_none()
        }
    }
}

pub(super) struct Fingerprint {
    content_bytes: usize,
    digest: [u8; 32],
}

impl Serialize for Fingerprint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("ConfidentialFingerprint", 3)?;
        state.serialize_field("algorithm", "sha256")?;
        state.serialize_field("byte_length", &self.content_bytes)?;
        state.serialize_field("digest", &HexDigest(&self.digest))?;
        state.end()
    }
}

struct HexDigest<'a>(&'a [u8; 32]);

impl fmt::Display for HexDigest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for HexDigest<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

pub(super) struct FingerprintBuilder {
    hasher: Sha256,
    content_bytes: usize,
}

impl FingerprintBuilder {
    pub(super) fn new(domain: &'static str) -> Self {
        let mut hasher = Sha256::new();
        update_framed(&mut hasher, SCHEMA.as_bytes());
        update_framed(&mut hasher, domain.as_bytes());
        Self {
            hasher,
            content_bytes: 0,
        }
    }

    pub(super) fn tag(&mut self, tag: u8) {
        self.hasher.update([tag]);
    }

    pub(super) fn sequence_len(&mut self, length: usize) {
        self.hasher.update((length as u64).to_be_bytes());
    }

    pub(super) fn bytes(&mut self, bytes: &[u8]) {
        self.hasher.update((bytes.len() as u64).to_be_bytes());
        self.hasher.update(bytes);
        self.content_bytes = self.content_bytes.saturating_add(bytes.len());
    }

    pub(super) fn bool(&mut self, value: bool) {
        self.hasher.update([u8::from(value)]);
        self.content_bytes = self.content_bytes.saturating_add(1);
    }

    pub(super) fn u8(&mut self, value: u8) {
        self.hasher.update([value]);
        self.content_bytes = self.content_bytes.saturating_add(1);
    }

    pub(super) fn u64(&mut self, value: u64) {
        self.hasher.update(value.to_be_bytes());
        self.content_bytes = self.content_bytes.saturating_add(8);
    }

    pub(super) fn u128(&mut self, value: u128) {
        self.hasher.update(value.to_be_bytes());
        self.content_bytes = self.content_bytes.saturating_add(16);
    }

    pub(super) fn finish(self) -> Fingerprint {
        Fingerprint {
            content_bytes: self.content_bytes,
            digest: self.hasher.finalize().into(),
        }
    }
}

pub(super) fn confidential_bytes(domain: &'static str, bytes: &[u8]) -> Fingerprint {
    let mut builder = FingerprintBuilder::new(domain);
    builder.bytes(bytes);
    builder.finish()
}

fn update_framed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}
