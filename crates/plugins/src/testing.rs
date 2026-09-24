//! Helpers for tests that need real packages (the `testing` feature):
//! building zips and signing them the way minisign does, with throwaway
//! keys, so tests need no minisign binary. Signing only; nothing here
//! loosens verification.

// Test helpers: failing loudly is the point.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Cursor, Write};

use blake2::{Blake2b512, Digest};
use ed25519_dalek::{Signer, SigningKey};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

/// A minisign key pair, derived from `seed` (so tests are repeatable).
pub struct Key {
    id: [u8; 8],
    signing: SigningKey,
}

impl Key {
    pub fn new(seed: u8) -> Self {
        Self {
            id: [seed; 8],
            signing: SigningKey::from_bytes(&[seed; 32]),
        }
    }

    /// The base64 line of a minisign `.pub` file.
    pub fn public(&self) -> String {
        let mut bytes = b"Ed".to_vec();
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(&self.signing.verifying_key().to_bytes());
        base64(&bytes)
    }

    /// A `.minisig` file: a prehashed (`ED`) signature over BLAKE2b-512 of
    /// the data, and a global signature over it plus the trusted comment.
    pub fn sign(&self, data: &[u8]) -> String {
        let hash = Blake2b512::digest(data);
        let signature = self.signing.sign(&hash).to_bytes();
        let mut line = b"ED".to_vec();
        line.extend_from_slice(&self.id);
        line.extend_from_slice(&signature);
        let trusted = "timestamp:1790000000\tfile:plugin.zip";
        let mut global = signature.to_vec();
        global.extend_from_slice(trusted.as_bytes());
        let global = self.signing.sign(&global).to_bytes();
        format!(
            "untrusted comment: signature from a tether test key\n{}\ntrusted comment: {trusted}\n{}\n",
            base64(&line),
            base64(&global)
        )
    }
}

pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A zip of these files, deflated; names ending in `/` are directories.
pub fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in files {
        if name.ends_with('/') {
            writer.add_directory(*name, options).unwrap();
        } else {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}

/// A minimal `plugin.toml` for `id`, published with `key`.
pub fn manifest(id: &str, key: &Key) -> String {
    format!(
        "[plugin]\nid = \"{id}\"\nname = \"Test plugin\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\nstorage = true\n",
        key.public()
    )
}

/// A package for `id` signed by `key`, with `extra` files, and its
/// signature.
pub fn package(id: &str, key: &Key, extra: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let manifest = manifest(id, key);
    let mut files: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", COMPONENT),
    ];
    files.extend_from_slice(extra);
    let bytes = zip(&files);
    let signature = key.sign(&bytes);
    (bytes, signature)
}

/// Stands in for `plugin.wasm` where nothing compiles it.
pub const COMPONENT: &[u8] = b"\0asm\x0d\0\x01\0 not really a component";
