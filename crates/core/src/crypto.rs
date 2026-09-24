//! Encryption at rest for refresh tokens and other secrets (N7).
//!
//! XChaCha20-Poly1305 with a random 192-bit nonce per message, so nonces
//! never need tracking. Every ciphertext is bound to what it protects
//! through associated data (e.g. `token:<character id>`), so a row copied
//! onto another row fails to decrypt. The key comes from `ENCRYPTION_KEY`
//! and never enters the database.
//!
//! Layout: `version (1) || nonce (24) || ciphertext+tag`.

use std::fmt;

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};

use crate::Secret;

const VERSION: u8 = 1;
const NONCE_LEN: usize = 24;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("ENCRYPTION_KEY must be 64 hex characters (32 bytes)")]
    BadKey,
    #[error("could not generate a nonce")]
    Random,
    #[error("ciphertext is malformed or was not encrypted with this key for this purpose")]
    Decrypt,
}

/// The instance's data key. `Debug` never shows it.
#[derive(Clone)]
pub struct EncryptionKey {
    cipher: XChaCha20Poly1305,
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptionKey([redacted])")
    }
}

impl EncryptionKey {
    /// Parses the 64-hex-character key from `.env`.
    pub fn from_hex(hex: &Secret<String>) -> Result<Self, CryptoError> {
        let hex = hex.expose().trim();
        if hex.len() != 64 {
            return Err(CryptoError::BadKey);
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2).ok_or(CryptoError::BadKey)?, 16)
                .map_err(|_| CryptoError::BadKey)?;
        }
        let cipher = XChaCha20Poly1305::new_from_slice(&bytes).map_err(|_| CryptoError::BadKey)?;
        Ok(Self { cipher })
    }

    pub fn encrypt(&self, plaintext: &[u8], context: &str) -> Result<Vec<u8>, CryptoError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| CryptoError::Random)?;
        let ciphertext = self
            .cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| CryptoError::Decrypt)?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn decrypt(&self, sealed: &[u8], context: &str) -> Result<Vec<u8>, CryptoError> {
        let (&version, rest) = sealed.split_first().ok_or(CryptoError::Decrypt)?;
        if version != VERSION || rest.len() < NONCE_LEN {
            return Err(CryptoError::Decrypt);
        }
        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Decrypt)?;
        self.cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: ciphertext,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| CryptoError::Decrypt)
    }

    /// Encrypts a secret string.
    pub fn seal(&self, secret: &Secret<String>, context: &str) -> Result<Vec<u8>, CryptoError> {
        self.encrypt(secret.expose().as_bytes(), context)
    }

    /// Decrypts a secret string sealed with [`EncryptionKey::seal`].
    pub fn open(&self, sealed: &[u8], context: &str) -> Result<Secret<String>, CryptoError> {
        let bytes = self.decrypt(sealed, context)?;
        String::from_utf8(bytes)
            .map(Secret::new)
            .map_err(|_| CryptoError::Decrypt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(hex: &str) -> EncryptionKey {
        EncryptionKey::from_hex(&Secret::new(hex.to_owned())).unwrap()
    }

    const K1: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const K2: &str = "ff0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[test]
    fn round_trips_and_never_repeats_ciphertext() {
        let k = key(K1);
        let secret = Secret::new("refresh-token-value".to_owned());
        let a = k.seal(&secret, "token:1").unwrap();
        let b = k.seal(&secret, "token:1").unwrap();
        assert_ne!(a, b, "random nonces");
        assert!(
            !a.windows(7).any(|w| w == b"refresh"),
            "no plaintext in the output"
        );
        assert_eq!(
            k.open(&a, "token:1").unwrap().expose(),
            "refresh-token-value"
        );
    }

    #[test]
    fn wrong_context_wrong_key_or_tampering_fail() {
        let k = key(K1);
        let sealed = k.encrypt(b"secret", "token:1").unwrap();
        assert_eq!(k.decrypt(&sealed, "token:2"), Err(CryptoError::Decrypt));
        assert_eq!(
            key(K2).decrypt(&sealed, "token:1"),
            Err(CryptoError::Decrypt)
        );
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(k.decrypt(&tampered, "token:1"), Err(CryptoError::Decrypt));
        assert_eq!(k.decrypt(&[], "token:1"), Err(CryptoError::Decrypt));
        assert_eq!(k.decrypt(&[9, 1, 2], "token:1"), Err(CryptoError::Decrypt));
    }

    #[test]
    fn key_must_be_64_hex_characters() {
        for bad in ["", "abc", &"g".repeat(64), &"0".repeat(63), &"0".repeat(66)] {
            assert_eq!(
                EncryptionKey::from_hex(&Secret::new(bad.to_owned())).err(),
                Some(CryptoError::BadKey),
                "{bad:?}"
            );
        }
        assert!(EncryptionKey::from_hex(&Secret::new(format!(" {K1}\n"))).is_ok());
        assert_eq!(format!("{:?}", key(K1)), "EncryptionKey([redacted])");
    }
}
