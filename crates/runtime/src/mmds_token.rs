use std::fmt;
use std::str;
use std::time::Instant;

use aes_gcm::aead::{AeadInOut, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce, Tag};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use zeroize::Zeroizing;

pub const MMDS_TOKEN_MIN_TTL_SECONDS: u32 = 1;
pub const MMDS_TOKEN_MAX_TTL_SECONDS: u32 = 21_600;

const MMDS_TOKEN_KEY_BYTES: usize = 32;
const MMDS_TOKEN_NONCE_BYTES: usize = 12;
const MMDS_TOKEN_EXPIRY_BYTES: usize = 8;
const MMDS_TOKEN_TAG_BYTES: usize = 16;
const MMDS_TOKEN_DECODED_BYTES: usize =
    MMDS_TOKEN_NONCE_BYTES + MMDS_TOKEN_EXPIRY_BYTES + MMDS_TOKEN_TAG_BYTES;
const MMDS_TOKEN_ENCODED_BYTES: usize = 48;
const MMDS_TOKEN_INPUT_LIMIT_BYTES: usize = 70;
const MMDS_TOKEN_MAX_DECODED_INPUT_BYTES: usize = 54;
const MMDS_MILLISECONDS_PER_SECOND: u64 = 1_000;
const MMDS_DEFAULT_INSTANCE_ID: &str = "anonymous";
const MMDS_INSTANCE_AAD_PREFIX: &[u8] = b"microvmid=";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmdsTokenError {
    InvalidTtl { ttl_seconds: u32 },
    TimeUnavailable,
    RandomnessUnavailable,
    TokenEncryption,
    TokenEncoding,
}

impl fmt::Display for MmdsTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTtl { ttl_seconds } => write!(
                f,
                "Invalid MMDS token TTL: {ttl_seconds}. Please provide a value between {MMDS_TOKEN_MIN_TTL_SECONDS} and {MMDS_TOKEN_MAX_TTL_SECONDS}."
            ),
            Self::TimeUnavailable => f.write_str("MMDS token time is unavailable."),
            Self::RandomnessUnavailable => f.write_str("MMDS token randomness is unavailable."),
            Self::TokenEncryption => f.write_str("MMDS token encryption failed."),
            Self::TokenEncoding => f.write_str("MMDS token encoding failed."),
        }
    }
}

impl std::error::Error for MmdsTokenError {}

enum MmdsTokenClock {
    System {
        origin: Instant,
    },
    #[cfg(test)]
    Manual {
        now_millis: Option<u64>,
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
    fn now_millis(&self) -> Result<u64, MmdsTokenError> {
        match self {
            Self::System { origin } => {
                Ok(u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX))
            }
            #[cfg(test)]
            Self::Manual { now_millis } => now_millis.ok_or(MmdsTokenError::TimeUnavailable),
        }
    }
}

#[derive(Default)]
enum MmdsTokenEntropy {
    #[default]
    System,
    #[cfg(test)]
    Deterministic {
        next_value: u64,
        fills: usize,
        fail_on_fill: Option<usize>,
    },
}

impl MmdsTokenEntropy {
    fn fill(&mut self, output: &mut [u8]) -> Result<(), MmdsTokenError> {
        match self {
            Self::System => {
                getrandom::fill(output).map_err(|_| MmdsTokenError::RandomnessUnavailable)
            }
            #[cfg(test)]
            Self::Deterministic {
                next_value,
                fills,
                fail_on_fill,
            } => {
                let current_fill = *fills;
                *fills = fills.saturating_add(1);
                if *fail_on_fill == Some(current_fill) {
                    return Err(MmdsTokenError::RandomnessUnavailable);
                }
                let value_bytes = next_value.to_le_bytes();
                *next_value = next_value.wrapping_add(1);
                for (byte, value) in output.iter_mut().zip(value_bytes.iter().cycle()) {
                    *byte = *value;
                }
                Ok(())
            }
        }
    }
}

/// Stateless MMDS v2 token authority bound to one immutable VM instance ID.
pub struct MmdsTokenAuthority {
    current_key: Option<Zeroizing<[u8; MMDS_TOKEN_KEY_BYTES]>>,
    additional_authenticated_data: Zeroizing<Vec<u8>>,
    num_encrypted_tokens: u32,
    clock: MmdsTokenClock,
    entropy: MmdsTokenEntropy,
    #[cfg(test)]
    fail_encryption: bool,
    #[cfg(test)]
    fail_encoding: bool,
}

impl fmt::Debug for MmdsTokenAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MmdsTokenAuthority")
            .field("current_key", &"[REDACTED]")
            .field("additional_authenticated_data", &"[REDACTED]")
            .field("num_encrypted_tokens", &self.num_encrypted_tokens)
            .field("clock", &"[REDACTED]")
            .field("entropy", &"[REDACTED]")
            .finish()
    }
}

impl Default for MmdsTokenAuthority {
    fn default() -> Self {
        Self::new(MMDS_DEFAULT_INSTANCE_ID)
    }
}

impl MmdsTokenAuthority {
    pub fn new(instance_id: impl AsRef<str>) -> Self {
        let instance_id = instance_id.as_ref();
        let mut additional_authenticated_data = Zeroizing::new(Vec::with_capacity(
            MMDS_INSTANCE_AAD_PREFIX.len() + instance_id.len(),
        ));
        additional_authenticated_data.extend_from_slice(MMDS_INSTANCE_AAD_PREFIX);
        additional_authenticated_data.extend_from_slice(instance_id.as_bytes());

        Self {
            current_key: None,
            additional_authenticated_data,
            num_encrypted_tokens: 0,
            clock: MmdsTokenClock::default(),
            entropy: MmdsTokenEntropy::default(),
            #[cfg(test)]
            fail_encryption: false,
            #[cfg(test)]
            fail_encoding: false,
        }
    }

    pub fn generate_token(&mut self, ttl_seconds: u32) -> Result<String, MmdsTokenError> {
        validate_ttl(ttl_seconds)?;
        let now_millis = self.clock.now_millis()?;
        let expiry_millis = token_expiry_millis(now_millis, ttl_seconds);
        let needs_new_key = self.current_key.is_none() || self.num_encrypted_tokens == u32::MAX;
        let next_encrypted_tokens = if needs_new_key {
            1
        } else {
            self.num_encrypted_tokens
                .checked_add(1)
                .ok_or(MmdsTokenError::TokenEncryption)?
        };

        let mut candidate_key = if needs_new_key {
            let mut key = Zeroizing::new([0_u8; MMDS_TOKEN_KEY_BYTES]);
            self.entropy.fill(key.as_mut())?;
            Some(key)
        } else {
            None
        };
        let Some(encryption_key) = candidate_key.as_ref().or(self.current_key.as_ref()) else {
            return Err(MmdsTokenError::TokenEncryption);
        };

        let mut nonce_bytes = [0_u8; MMDS_TOKEN_NONCE_BYTES];
        self.entropy.fill(&mut nonce_bytes)?;
        let mut encrypted_expiry = Zeroizing::new(expiry_millis.to_le_bytes());
        let tag = self.encrypt_expiry(encryption_key, nonce_bytes, &mut encrypted_expiry)?;
        let encoded_token = self.encode_token(nonce_bytes, &encrypted_expiry, tag)?;

        if let Some(new_key) = candidate_key.take() {
            self.current_key = Some(new_key);
        }
        self.num_encrypted_tokens = next_encrypted_tokens;
        Ok(encoded_token)
    }

    pub fn is_valid(&self, encoded_token: &str) -> bool {
        if encoded_token.len() > MMDS_TOKEN_INPUT_LIMIT_BYTES {
            return false;
        }

        let Some(key) = self.current_key.as_ref() else {
            return false;
        };
        let Ok(now_millis) = self.clock.now_millis() else {
            return false;
        };
        let Some((nonce_bytes, encrypted_expiry_bytes, tag_bytes)) = decode_token(encoded_token)
        else {
            return false;
        };

        let cipher = cipher_for_key(key);
        let nonce = Nonce::from(nonce_bytes);
        let tag = Tag::from(tag_bytes);
        let mut expiry_bytes = Zeroizing::new(encrypted_expiry_bytes);
        if cipher
            .decrypt_inout_detached(
                &nonce,
                self.additional_authenticated_data.as_slice(),
                expiry_bytes.as_mut_slice().into(),
                &tag,
            )
            .is_err()
        {
            return false;
        }

        u64::from_le_bytes(*expiry_bytes) > now_millis
    }

    fn encrypt_expiry(
        &self,
        key: &Zeroizing<[u8; MMDS_TOKEN_KEY_BYTES]>,
        nonce_bytes: [u8; MMDS_TOKEN_NONCE_BYTES],
        expiry_bytes: &mut Zeroizing<[u8; MMDS_TOKEN_EXPIRY_BYTES]>,
    ) -> Result<[u8; MMDS_TOKEN_TAG_BYTES], MmdsTokenError> {
        #[cfg(test)]
        if self.fail_encryption {
            return Err(MmdsTokenError::TokenEncryption);
        }

        let cipher = cipher_for_key(key);
        let nonce = Nonce::from(nonce_bytes);
        let tag = cipher
            .encrypt_inout_detached(
                &nonce,
                self.additional_authenticated_data.as_slice(),
                expiry_bytes.as_mut_slice().into(),
            )
            .map_err(|_| MmdsTokenError::TokenEncryption)?;
        Ok(tag.into())
    }

    fn encode_token(
        &self,
        nonce_bytes: [u8; MMDS_TOKEN_NONCE_BYTES],
        encrypted_expiry: &[u8; MMDS_TOKEN_EXPIRY_BYTES],
        tag_bytes: [u8; MMDS_TOKEN_TAG_BYTES],
    ) -> Result<String, MmdsTokenError> {
        #[cfg(test)]
        if self.fail_encoding {
            return Err(MmdsTokenError::TokenEncoding);
        }

        let mut token_bytes = Zeroizing::new([0_u8; MMDS_TOKEN_DECODED_BYTES]);
        let (nonce_output, remaining) = token_bytes.split_at_mut(MMDS_TOKEN_NONCE_BYTES);
        let (expiry_output, tag_output) = remaining.split_at_mut(MMDS_TOKEN_EXPIRY_BYTES);
        nonce_output.copy_from_slice(&nonce_bytes);
        expiry_output.copy_from_slice(encrypted_expiry);
        tag_output.copy_from_slice(&tag_bytes);

        let mut encoded = Zeroizing::new([0_u8; MMDS_TOKEN_ENCODED_BYTES]);
        let encoded_len = STANDARD
            .encode_slice(token_bytes.as_slice(), encoded.as_mut_slice())
            .map_err(|_| MmdsTokenError::TokenEncoding)?;
        let encoded_bytes = encoded
            .get(..encoded_len)
            .ok_or(MmdsTokenError::TokenEncoding)?;
        let encoded_str =
            str::from_utf8(encoded_bytes).map_err(|_| MmdsTokenError::TokenEncoding)?;
        Ok(encoded_str.to_owned())
    }

    #[cfg(test)]
    pub(crate) fn with_manual_clock(instance_id: impl AsRef<str>, now_millis: u64) -> Self {
        let mut authority = Self::new(instance_id);
        authority.clock = MmdsTokenClock::Manual {
            now_millis: Some(now_millis),
        };
        authority.entropy = MmdsTokenEntropy::Deterministic {
            next_value: 1,
            fills: 0,
            fail_on_fill: None,
        };
        authority
    }

    #[cfg(test)]
    pub(crate) fn set_now_millis(&mut self, now_millis: u64) {
        self.clock = MmdsTokenClock::Manual {
            now_millis: Some(now_millis),
        };
    }

    #[cfg(test)]
    pub(crate) fn is_bound_to_instance_id(&self, instance_id: &str) -> bool {
        let mut expected = Vec::with_capacity(MMDS_INSTANCE_AAD_PREFIX.len() + instance_id.len());
        expected.extend_from_slice(MMDS_INSTANCE_AAD_PREFIX);
        expected.extend_from_slice(instance_id.as_bytes());
        self.additional_authenticated_data.as_slice() == expected
    }
}

fn cipher_for_key(key_bytes: &Zeroizing<[u8; MMDS_TOKEN_KEY_BYTES]>) -> Aes256Gcm {
    let key: &Key<Aes256Gcm> = (&**key_bytes).into();
    Aes256Gcm::new(key)
}

fn validate_ttl(ttl_seconds: u32) -> Result<(), MmdsTokenError> {
    if (MMDS_TOKEN_MIN_TTL_SECONDS..=MMDS_TOKEN_MAX_TTL_SECONDS).contains(&ttl_seconds) {
        return Ok(());
    }

    Err(MmdsTokenError::InvalidTtl { ttl_seconds })
}

fn token_expiry_millis(now_millis: u64, ttl_seconds: u32) -> u64 {
    now_millis.saturating_add(u64::from(ttl_seconds) * MMDS_MILLISECONDS_PER_SECOND)
}

fn decode_token(
    encoded_token: &str,
) -> Option<(
    [u8; MMDS_TOKEN_NONCE_BYTES],
    [u8; MMDS_TOKEN_EXPIRY_BYTES],
    [u8; MMDS_TOKEN_TAG_BYTES],
)> {
    let mut decoded = Zeroizing::new([0_u8; MMDS_TOKEN_MAX_DECODED_INPUT_BYTES]);
    let decoded_len = STANDARD
        .decode_slice(encoded_token, decoded.as_mut_slice())
        .ok()?;
    if decoded_len != MMDS_TOKEN_DECODED_BYTES {
        return None;
    }

    let token_bytes = decoded.get(..decoded_len)?;
    let (nonce, remaining) = token_bytes.split_at_checked(MMDS_TOKEN_NONCE_BYTES)?;
    let (encrypted_expiry, tag) = remaining.split_at_checked(MMDS_TOKEN_EXPIRY_BYTES)?;
    Some((
        nonce.try_into().ok()?,
        encrypted_expiry.try_into().ok()?,
        tag.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(instance_id: &str) -> MmdsTokenAuthority {
        MmdsTokenAuthority::with_manual_clock(instance_id, 1_000)
    }

    fn decoded_token(token: &str) -> Vec<u8> {
        STANDARD
            .decode(token)
            .expect("test MMDS token should decode")
    }

    fn entropy_fill_count(authority: &MmdsTokenAuthority) -> usize {
        match authority.entropy {
            MmdsTokenEntropy::Deterministic { fills, .. } => fills,
            MmdsTokenEntropy::System => 0,
        }
    }

    fn set_entropy_failure(authority: &mut MmdsTokenAuthority, fill_offset: usize) {
        if let MmdsTokenEntropy::Deterministic {
            fills,
            ref mut fail_on_fill,
            ..
        } = authority.entropy
        {
            *fail_on_fill = Some(fills + fill_offset);
        }
    }

    #[test]
    fn accepts_ttl_boundaries_and_emits_firecracker_shape() {
        let mut authority = authority("shape-instance");

        let minimum = authority
            .generate_token(MMDS_TOKEN_MIN_TTL_SECONDS)
            .expect("minimum TTL should generate");
        let maximum = authority
            .generate_token(MMDS_TOKEN_MAX_TTL_SECONDS)
            .expect("maximum TTL should generate");

        for token in [&minimum, &maximum] {
            assert_eq!(token.len(), MMDS_TOKEN_ENCODED_BYTES);
            assert_eq!(decoded_token(token).len(), MMDS_TOKEN_DECODED_BYTES);
            assert!(authority.is_valid(token));
        }
        assert!(
            decoded_token(&minimum)[..MMDS_TOKEN_NONCE_BYTES]
                != decoded_token(&maximum)[..MMDS_TOKEN_NONCE_BYTES]
        );
    }

    #[test]
    fn rejects_invalid_ttl_before_initializing_key_or_entropy() {
        let mut authority = authority("ttl-instance");

        assert!(matches!(
            authority.generate_token(0),
            Err(MmdsTokenError::InvalidTtl { ttl_seconds: 0 })
        ));
        assert!(matches!(
            authority.generate_token(MMDS_TOKEN_MAX_TTL_SECONDS + 1),
            Err(MmdsTokenError::InvalidTtl { ttl_seconds })
                if ttl_seconds == MMDS_TOKEN_MAX_TTL_SECONDS + 1
        ));
        assert!(authority.current_key.is_none());
        assert_eq!(authority.num_encrypted_tokens, 0);
        assert_eq!(entropy_fill_count(&authority), 0);
    }

    #[test]
    fn rejects_empty_oversized_malformed_and_wrong_length_tokens() {
        let mut authority = authority("invalid-instance");
        let token = authority
            .generate_token(60)
            .expect("valid token should generate");

        assert!(!authority.is_valid(""));
        assert!(!authority.is_valid(&"A".repeat(MMDS_TOKEN_INPUT_LIMIT_BYTES + 1)));
        assert!(!authority.is_valid("not base64"));
        assert!(!authority.is_valid(&STANDARD.encode([0_u8; MMDS_TOKEN_DECODED_BYTES - 1])));
        assert!(!authority.is_valid(&STANDARD.encode([0_u8; MMDS_TOKEN_DECODED_BYTES + 1])));
        assert!(authority.is_valid(&token));
    }

    #[test]
    fn rejects_modified_nonce_ciphertext_and_tag() {
        let mut authority = authority("modified-instance");
        let token = authority
            .generate_token(60)
            .expect("valid token should generate");

        for offset in [
            0,
            MMDS_TOKEN_NONCE_BYTES,
            MMDS_TOKEN_NONCE_BYTES + MMDS_TOKEN_EXPIRY_BYTES,
        ] {
            let mut bytes = decoded_token(&token);
            bytes[offset] ^= 1;
            assert!(!authority.is_valid(&STANDARD.encode(bytes)));
        }
        assert!(authority.is_valid(&token));
    }

    #[test]
    fn binds_tokens_to_instance_aad_even_with_same_key() {
        let shared_key = Zeroizing::new([0x5a; MMDS_TOKEN_KEY_BYTES]);
        let mut first = authority("instance-a");
        first.current_key = Some(Zeroizing::new(*shared_key));
        let mut second = authority("instance-b");
        second.current_key = Some(shared_key);
        let token = first
            .generate_token(60)
            .expect("first instance token should generate");

        assert!(first.is_valid(&token));
        assert!(!second.is_valid(&token));
    }

    #[test]
    fn expiry_is_strict_and_validation_does_not_mutate_counter() {
        let mut authority = authority("expiry-instance");
        let token = authority.generate_token(1).expect("token should generate");
        let generated_count = authority.num_encrypted_tokens;

        authority.set_now_millis(1_999);
        assert!(authority.is_valid(&token));
        authority.set_now_millis(2_000);
        assert!(!authority.is_valid(&token));
        assert_eq!(authority.num_encrypted_tokens, generated_count);
    }

    #[test]
    fn rotates_before_encrypting_after_u32_max_tokens() {
        let mut authority = authority("rotation-instance");
        let old_token = authority
            .generate_token(60)
            .expect("old token should generate");
        let old_key = **authority
            .current_key
            .as_ref()
            .expect("authority should have a key");
        authority.num_encrypted_tokens = u32::MAX;

        let new_token = authority
            .generate_token(60)
            .expect("rotation token should generate");

        assert!(
            authority
                .current_key
                .as_ref()
                .expect("rotated authority should have a key")
                .as_ref()
                != old_key
        );
        assert_eq!(authority.num_encrypted_tokens, 1);
        assert!(!authority.is_valid(&old_token));
        assert!(authority.is_valid(&new_token));
    }

    #[test]
    fn failed_rotation_key_generation_preserves_old_authority() {
        let mut authority = authority("rotation-key-failure");
        let old_token = authority
            .generate_token(60)
            .expect("old token should generate");
        let old_key = **authority
            .current_key
            .as_ref()
            .expect("authority should have a key");
        authority.num_encrypted_tokens = u32::MAX;
        set_entropy_failure(&mut authority, 0);

        assert!(matches!(
            authority.generate_token(60),
            Err(MmdsTokenError::RandomnessUnavailable)
        ));
        assert!(
            authority
                .current_key
                .as_ref()
                .expect("old key should remain")
                .as_ref()
                == old_key
        );
        assert_eq!(authority.num_encrypted_tokens, u32::MAX);
        assert!(authority.is_valid(&old_token));
    }

    #[test]
    fn failed_rotation_nonce_generation_preserves_old_authority() {
        let mut authority = authority("rotation-nonce-failure");
        let old_token = authority
            .generate_token(60)
            .expect("old token should generate");
        let old_key = **authority
            .current_key
            .as_ref()
            .expect("authority should have a key");
        authority.num_encrypted_tokens = u32::MAX;
        set_entropy_failure(&mut authority, 1);

        assert!(matches!(
            authority.generate_token(60),
            Err(MmdsTokenError::RandomnessUnavailable)
        ));
        assert!(
            authority
                .current_key
                .as_ref()
                .expect("old key should remain")
                .as_ref()
                == old_key
        );
        assert_eq!(authority.num_encrypted_tokens, u32::MAX);
        assert!(authority.is_valid(&old_token));
    }

    #[test]
    fn clock_encryption_and_encoding_failures_are_nonmutating() {
        enum GenerationFailure {
            Clock,
            Encryption,
            Encoding,
        }

        for failure in [
            GenerationFailure::Clock,
            GenerationFailure::Encryption,
            GenerationFailure::Encoding,
        ] {
            let mut authority = authority("transaction-instance");
            match failure {
                GenerationFailure::Clock => {
                    authority.clock = MmdsTokenClock::Manual { now_millis: None };
                }
                GenerationFailure::Encryption => authority.fail_encryption = true,
                GenerationFailure::Encoding => authority.fail_encoding = true,
            }

            assert!(authority.generate_token(60).is_err());
            assert!(authority.current_key.is_none());
            assert_eq!(authority.num_encrypted_tokens, 0);
        }
    }

    #[test]
    fn failed_clock_encryption_and_encoding_rotation_preserve_old_authority() {
        enum GenerationFailure {
            Clock,
            Encryption,
            Encoding,
        }

        for failure in [
            GenerationFailure::Clock,
            GenerationFailure::Encryption,
            GenerationFailure::Encoding,
        ] {
            let mut authority = authority("rotation-transaction-instance");
            let old_token = authority
                .generate_token(60)
                .expect("old token should generate");
            let old_key = **authority
                .current_key
                .as_ref()
                .expect("authority should have a key");
            authority.num_encrypted_tokens = u32::MAX;
            match failure {
                GenerationFailure::Clock => {
                    authority.clock = MmdsTokenClock::Manual { now_millis: None };
                }
                GenerationFailure::Encryption => authority.fail_encryption = true,
                GenerationFailure::Encoding => authority.fail_encoding = true,
            }

            assert!(authority.generate_token(60).is_err());
            assert!(
                authority
                    .current_key
                    .as_ref()
                    .expect("old key should remain")
                    .as_ref()
                    == old_key
            );
            assert_eq!(authority.num_encrypted_tokens, u32::MAX);
            authority.set_now_millis(1_000);
            assert!(authority.is_valid(&old_token));
        }
    }

    #[test]
    fn token_generation_has_no_active_token_capacity() {
        let mut authority = authority("capacity-instance");
        for _ in 0..1_025 {
            authority
                .generate_token(60)
                .expect("stateless token generation should not exhaust capacity");
        }
        assert_eq!(authority.num_encrypted_tokens, 1_025);
    }

    #[test]
    fn debug_and_errors_redact_instance_key_and_tokens() {
        let instance_id = "private-instance-identity";
        let mut authority = authority(instance_id);
        authority.current_key = Some(Zeroizing::new([0x41; MMDS_TOKEN_KEY_BYTES]));
        let token = authority.generate_token(60).expect("token should generate");
        let debug = format!("{authority:?}");

        assert!(!debug.contains(instance_id));
        assert!(!debug.contains(&"A".repeat(MMDS_TOKEN_KEY_BYTES)));
        assert!(!debug.contains(&token));
        assert!(debug.contains("[REDACTED]"));
        for error in [
            MmdsTokenError::TimeUnavailable,
            MmdsTokenError::RandomnessUnavailable,
            MmdsTokenError::TokenEncryption,
            MmdsTokenError::TokenEncoding,
        ] {
            assert!(!error.to_string().contains(instance_id));
            assert!(!error.to_string().contains(&token));
        }
    }

    #[test]
    fn error_messages_are_stable_and_secret_free() {
        assert_eq!(
            MmdsTokenError::InvalidTtl { ttl_seconds: 0 }.to_string(),
            "Invalid MMDS token TTL: 0. Please provide a value between 1 and 21600."
        );
        assert_eq!(
            MmdsTokenError::TimeUnavailable.to_string(),
            "MMDS token time is unavailable."
        );
        assert_eq!(
            MmdsTokenError::RandomnessUnavailable.to_string(),
            "MMDS token randomness is unavailable."
        );
        assert_eq!(
            MmdsTokenError::TokenEncryption.to_string(),
            "MMDS token encryption failed."
        );
        assert_eq!(
            MmdsTokenError::TokenEncoding.to_string(),
            "MMDS token encoding failed."
        );
    }
}
