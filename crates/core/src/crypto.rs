//! Encryption at rest for refresh tokens and other secrets (N7).
//!
//! XChaCha20-Poly1305 with a random 192-bit nonce per message, so nonces
//! never need tracking. Every ciphertext is bound to what it protects
//! through associated data (e.g. `token:<character id>`), so a row copied
//! onto another row fails to decrypt. The key comes from `ENCRYPTION_KEY`
//! and never enters the database.
//!
//! Layout: `version (1) || nonce (24) || ciphertext+tag`.
//!
//! Large data (snapshots) is sealed in chunks with [`StreamSealer`], under
//! a key [derived](EncryptionKey::derive) for that purpose, never the
//! instance key itself.

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
    #[error("a stream chunk is bigger than the chunk size")]
    ChunkTooBig,
    #[error("the stream already ended")]
    StreamEnded,
    #[error("the stream has too many chunks")]
    StreamTooLong,
}

/// The instance's data key. `Debug` never shows it.
#[derive(Clone)]
pub struct EncryptionKey {
    /// Kept for [`EncryptionKey::derive`].
    bytes: [u8; 32],
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
        Self::from_bytes(bytes)
    }

    fn from_bytes(bytes: [u8; 32]) -> Result<Self, CryptoError> {
        let cipher = XChaCha20Poly1305::new_from_slice(&bytes).map_err(|_| CryptoError::BadKey)?;
        Ok(Self { bytes, cipher })
    }

    /// A key for one purpose (`label`, e.g. `tether snapshots v1`), so data
    /// sealed for one use can never be opened as another, and the instance
    /// key itself never seals it. HKDF-SHA-256's expand step (RFC 5869)
    /// with the instance key as the pseudorandom key: it is already 32
    /// uniformly random bytes, so the extract step would add nothing.
    pub fn derive(&self, label: &str) -> Result<Self, CryptoError> {
        let okm = crate::scram::hmac(&self.bytes, &[label.as_bytes(), &[1]]);
        Self::from_bytes(okm)
    }

    /// Starts sealing a stream, with a fresh random nonce prefix. `aad` is
    /// bound to every chunk (a snapshot's header, for one).
    pub fn stream_sealer(&self, aad: &[u8]) -> Result<StreamSealer, CryptoError> {
        let mut prefix = [0u8; STREAM_PREFIX_LEN];
        getrandom::fill(&mut prefix).map_err(|_| CryptoError::Random)?;
        Ok(StreamSealer(Stream::new(self.cipher.clone(), prefix, aad)))
    }

    /// Opens a stream sealed by [`EncryptionKey::stream_sealer`] with this
    /// key, that prefix and the same `aad`.
    pub fn stream_opener(&self, prefix: [u8; STREAM_PREFIX_LEN], aad: &[u8]) -> StreamOpener {
        StreamOpener(Stream::new(self.cipher.clone(), prefix, aad))
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

/// Plaintext bytes per chunk of a sealed stream.
pub const STREAM_CHUNK: usize = 64 * 1024;
/// A sealed chunk is its plaintext plus this tag.
pub const STREAM_TAG: usize = 16;
/// The random part of every chunk's nonce, stored once per stream.
pub const STREAM_PREFIX_LEN: usize = 19;

/// The STREAM construction (Hoang, Reyhanitabar, Rogaway and Vizár, 2015;
/// the "BE32" layout of RustCrypto's `aead-stream`) over
/// XChaCha20-Poly1305: chunk `i`'s nonce is `prefix (19) || i as u32
/// big-endian (4) || last (1)`. Chunks can't be reordered, dropped or
/// repeated without failing to open, nothing can follow the last one, and
/// a stream cut short is caught because its last chunk never arrives.
struct Stream {
    cipher: XChaCha20Poly1305,
    prefix: [u8; STREAM_PREFIX_LEN],
    aad: Vec<u8>,
    counter: u32,
    finished: bool,
}

impl Stream {
    fn new(cipher: XChaCha20Poly1305, prefix: [u8; STREAM_PREFIX_LEN], aad: &[u8]) -> Self {
        Self {
            cipher,
            prefix,
            aad: aad.to_vec(),
            counter: 0,
            finished: false,
        }
    }

    /// The next chunk's nonce; moves the counter on.
    fn next_nonce(&mut self, last: bool) -> Result<XNonce, CryptoError> {
        if self.finished {
            return Err(CryptoError::StreamEnded);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..STREAM_PREFIX_LEN].copy_from_slice(&self.prefix);
        nonce[STREAM_PREFIX_LEN..NONCE_LEN - 1].copy_from_slice(&self.counter.to_be_bytes());
        nonce[NONCE_LEN - 1] = u8::from(last);
        if last {
            self.finished = true;
        } else {
            self.counter = self
                .counter
                .checked_add(1)
                .ok_or(CryptoError::StreamTooLong)?;
        }
        Ok(XNonce::from(nonce))
    }
}

/// Seals a stream chunk by chunk. See [`EncryptionKey::stream_sealer`].
pub struct StreamSealer(Stream);

impl fmt::Debug for StreamSealer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StreamSealer([redacted])")
    }
}

impl StreamSealer {
    /// Stored with the stream: the opener needs it.
    pub fn prefix(&self) -> [u8; STREAM_PREFIX_LEN] {
        self.0.prefix
    }

    /// Seals the next chunk (at most [`STREAM_CHUNK`] bytes); `last` must
    /// be set on the final one, and nothing can follow it.
    pub fn seal(&mut self, chunk: &[u8], last: bool) -> Result<Vec<u8>, CryptoError> {
        if chunk.len() > STREAM_CHUNK {
            return Err(CryptoError::ChunkTooBig);
        }
        let nonce = self.0.next_nonce(last)?;
        self.0
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: chunk,
                    aad: &self.0.aad,
                },
            )
            .map_err(|_| CryptoError::Decrypt)
    }
}

/// Opens a sealed stream chunk by chunk. See
/// [`EncryptionKey::stream_opener`].
pub struct StreamOpener(Stream);

impl fmt::Debug for StreamOpener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StreamOpener([redacted])")
    }
}

impl StreamOpener {
    /// Opens the next chunk. `last` says whether the stream claims this is
    /// its final chunk; a false claim fails to open.
    pub fn open(&mut self, sealed: &[u8], last: bool) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() > STREAM_CHUNK + STREAM_TAG {
            return Err(CryptoError::ChunkTooBig);
        }
        let nonce = self.0.next_nonce(last)?;
        self.0
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: sealed,
                    aad: &self.0.aad,
                },
            )
            .map_err(|_| CryptoError::Decrypt)
    }

    /// Whether the last chunk has been opened: a stream that ends before
    /// then was cut short.
    pub fn finished(&self) -> bool {
        self.0.finished
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

    fn seal_all(key: &EncryptionKey, data: &[u8], aad: &[u8]) -> (StreamSealer, Vec<Vec<u8>>) {
        let mut sealer = key.stream_sealer(aad).unwrap();
        let chunks: Vec<&[u8]> = if data.is_empty() {
            vec![&[]]
        } else {
            data.chunks(STREAM_CHUNK).collect()
        };
        let last = chunks.len() - 1;
        let sealed = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| sealer.seal(c, i == last).unwrap())
            .collect();
        (sealer, sealed)
    }

    fn open_all(
        key: &EncryptionKey,
        prefix: [u8; STREAM_PREFIX_LEN],
        aad: &[u8],
        sealed: &[Vec<u8>],
    ) -> Result<Vec<u8>, CryptoError> {
        let mut opener = key.stream_opener(prefix, aad);
        let mut out = Vec::new();
        for (i, chunk) in sealed.iter().enumerate() {
            out.extend(opener.open(chunk, i == sealed.len() - 1)?);
        }
        if !opener.finished() {
            return Err(CryptoError::Decrypt);
        }
        Ok(out)
    }

    #[test]
    fn derived_keys_differ_by_label_and_from_the_instance_key() {
        let k = key(K1);
        let a = k.derive("tether snapshots v1").unwrap();
        let b = k.derive("something else").unwrap();
        let sealed = a.encrypt(b"x", "c").unwrap();
        assert_eq!(a.decrypt(&sealed, "c").unwrap(), b"x");
        assert_eq!(b.decrypt(&sealed, "c"), Err(CryptoError::Decrypt));
        assert_eq!(k.decrypt(&sealed, "c"), Err(CryptoError::Decrypt));
        // Deterministic: the same key and label give the same subkey.
        let again = key(K1).derive("tether snapshots v1").unwrap();
        assert_eq!(again.decrypt(&sealed, "c").unwrap(), b"x");
        assert_eq!(format!("{a:?}"), "EncryptionKey([redacted])");
    }

    #[test]
    fn derive_is_hkdf_sha256_expand() {
        // RFC 5869 test case 1: its PRK and info give this OKM (first 32
        // bytes of the 42).
        let k = key("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5");
        let info = [0xf0u8, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
        let okm = crate::scram::hmac(&k.bytes, &[&info, &[1]]);
        let hex: String = okm.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
        );
    }

    #[test]
    fn streams_round_trip_across_chunks() {
        let k = key(K1).derive("test").unwrap();
        for len in [0, 1, STREAM_CHUNK, STREAM_CHUNK + 1, 3 * STREAM_CHUNK - 7] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let (sealer, sealed) = seal_all(&k, &data, b"header");
            assert_eq!(
                open_all(&k, sealer.prefix(), b"header", &sealed).unwrap(),
                data,
                "{len}"
            );
        }
    }

    #[test]
    fn streams_refuse_tampering_reordering_and_truncation() {
        let k = key(K1).derive("test").unwrap();
        let data = vec![7u8; 3 * STREAM_CHUNK];
        let (sealer, sealed) = seal_all(&k, &data, b"header");
        let prefix = sealer.prefix();
        assert_eq!(sealed.len(), 3);

        // Cut off the last chunk: the rest opens, but the end never comes.
        assert_eq!(
            open_all(&k, prefix, b"header", &sealed[..2]),
            Err(CryptoError::Decrypt)
        );
        // Claiming an earlier chunk is the last one fails.
        let mut opener = k.stream_opener(prefix, b"header");
        assert_eq!(opener.open(&sealed[0], true), Err(CryptoError::Decrypt));
        // Swapped chunks.
        let swapped = vec![sealed[1].clone(), sealed[0].clone(), sealed[2].clone()];
        assert_eq!(
            open_all(&k, prefix, b"header", &swapped),
            Err(CryptoError::Decrypt)
        );
        // A flipped bit, another header, the instance key, another prefix.
        let mut flipped = sealed.clone();
        flipped[1][5] ^= 1;
        assert_eq!(
            open_all(&k, prefix, b"header", &flipped),
            Err(CryptoError::Decrypt)
        );
        assert_eq!(
            open_all(&k, prefix, b"other", &sealed),
            Err(CryptoError::Decrypt)
        );
        assert_eq!(
            open_all(&key(K1), prefix, b"header", &sealed),
            Err(CryptoError::Decrypt)
        );
        let mut other_prefix = prefix;
        other_prefix[0] ^= 1;
        assert_eq!(
            open_all(&k, other_prefix, b"header", &sealed),
            Err(CryptoError::Decrypt)
        );
        // Nothing opens after the last chunk, and nothing seals after it.
        let mut opener = k.stream_opener(prefix, b"header");
        for (i, chunk) in sealed.iter().enumerate() {
            opener.open(chunk, i == 2).unwrap();
        }
        assert_eq!(opener.open(&sealed[2], true), Err(CryptoError::StreamEnded));
        let (mut sealer, _) = seal_all(&k, b"x", b"header");
        assert_eq!(sealer.seal(b"y", true), Err(CryptoError::StreamEnded));
        // Oversized chunks are refused before any work.
        let mut sealer = k.stream_sealer(b"header").unwrap();
        assert_eq!(
            sealer.seal(&vec![0; STREAM_CHUNK + 1], true),
            Err(CryptoError::ChunkTooBig)
        );
    }

    #[test]
    fn stream_prefixes_are_random() {
        let k = key(K1);
        let a = k.stream_sealer(b"").unwrap().prefix();
        let b = k.stream_sealer(b"").unwrap().prefix();
        assert_ne!(a, b);
    }
}
