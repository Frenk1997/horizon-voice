//! The voice packets' encryption: AES-128-GCM with the 16-byte secret from
//! `voicechat:secret`, a random 12-byte IV prepended, a 128-bit tag appended
//! and no associated data (Java's `AES/GCM/NoPadding`).

use ring::aead::{AES_128_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

pub const SECRET_LEN: usize = 16;
pub const IV_LEN: usize = NONCE_LEN;
pub const TAG_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CryptoError;

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "voice packet encryption failed")
    }
}

impl std::error::Error for CryptoError {}

pub struct Secret {
    bytes: [u8; SECRET_LEN],
    key: LessSafeKey,
    rng: SystemRandom,
}

impl Secret {
    pub fn new(bytes: [u8; SECRET_LEN]) -> Self {
        let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &bytes).expect("16-byte key"));
        Secret { bytes, key, rng: SystemRandom::new() }
    }

    pub fn bytes(&self) -> &[u8; SECRET_LEN] {
        &self.bytes
    }

    /// `iv || ciphertext || tag`.
    pub fn encrypt(&self, plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut iv = [0u8; IV_LEN];
        self.rng.fill(&mut iv).map_err(|_| CryptoError)?;
        self.encrypt_with_iv(iv, plain)
    }

    fn encrypt_with_iv(&self, iv: [u8; IV_LEN], plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = Vec::with_capacity(IV_LEN + plain.len() + TAG_LEN);
        out.extend_from_slice(&iv);
        out.extend_from_slice(plain);
        let tag = self
            .key
            .seal_in_place_separate_tag(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut out[IV_LEN..])
            .map_err(|_| CryptoError)?;
        out.extend_from_slice(tag.as_ref());
        Ok(out)
    }

    pub fn decrypt(&self, payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if payload.len() < IV_LEN + TAG_LEN {
            return Err(CryptoError);
        }
        let (iv, rest) = payload.split_at(IV_LEN);
        let nonce = Nonce::try_assume_unique_for_key(iv).map_err(|_| CryptoError)?;
        let mut buf = rest.to_vec();
        let len = self.key.open_in_place(nonce, Aad::empty(), &mut buf).map_err(|_| CryptoError)?.len();
        buf.truncate(len);
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // NIST GCM test vector (gcmEncryptExtIV128, Count 0 of the 128-bit
    // plaintext set): AES-128, 96-bit IV, no AAD — the shape Java's
    // AES/GCM/NoPadding produces for Simple Voice Chat.
    #[test]
    fn matches_nist_vector() {
        let key: [u8; 16] = hex("7fddb57453c241d03efbed3ac44e371c").try_into().unwrap();
        let iv: [u8; 12] = hex("ee283a3fc75575e33efd4887").try_into().unwrap();
        let plain = hex("d5de42b461646c255c87bd2962d3b9a2");
        let s = Secret::new(key);
        let out = s.encrypt_with_iv(iv, &plain).unwrap();
        assert_eq!(&out[..12], &iv);
        assert_eq!(&out[12..28], hex("2ccda4a5415cb91e135c2a0f78c9b2fd").as_slice());
        assert_eq!(&out[28..], hex("b36d1df9b9d5e596f83e8b7f52971cb3").as_slice());
        assert_eq!(s.decrypt(&out).unwrap(), plain);
    }

    #[test]
    fn round_trips_with_random_iv() {
        let s = Secret::new([7; 16]);
        let a = s.encrypt(b"hello voice").unwrap();
        let b = s.encrypt(b"hello voice").unwrap();
        assert_ne!(a, b, "a fresh IV each time");
        assert_eq!(s.decrypt(&a).unwrap(), b"hello voice");
    }

    #[test]
    fn rejects_tampering_and_wrong_key() {
        let s = Secret::new([1; 16]);
        let mut enc = s.encrypt(b"data").unwrap();
        assert!(Secret::new([2; 16]).decrypt(&enc).is_err());
        let last = enc.len() - 1;
        enc[last] ^= 1;
        assert!(s.decrypt(&enc).is_err());
        assert!(s.decrypt(&[0; 10]).is_err());
    }
}
