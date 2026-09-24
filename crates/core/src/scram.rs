//! SCRAM-SHA-256 password verifiers for Postgres roles Tether creates
//! (plugin storage roles). Handing Postgres the verifier instead of the
//! password keeps the password out of SQL text, and so out of server logs.

use sha2::{Digest, Sha256};

use crate::Secret;

const ITERATIONS: u32 = 4096;
const BLOCK: usize = 64;

fn hmac(key: &[u8], message: &[&[u8]]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    for part in message {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// PBKDF2-HMAC-SHA-256 with one output block (32 bytes), as SCRAM uses it.
fn salted_password(password: &[u8], salt: &[u8]) -> [u8; 32] {
    let mut u = hmac(password, &[salt, &1u32.to_be_bytes()]);
    let mut out = u;
    for _ in 1..ITERATIONS {
        u = hmac(password, &[&u]);
        for (o, b) in out.iter_mut().zip(u) {
            *o ^= b;
        }
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The verifier Postgres stores for `password` with this salt:
/// `SCRAM-SHA-256$4096:<salt>$<StoredKey>:<ServerKey>`.
pub fn verifier_with_salt(password: &Secret<String>, salt: &[u8]) -> String {
    let salted = salted_password(password.expose().as_bytes(), salt);
    let client_key = hmac(&salted, &[b"Client Key"]);
    let stored_key = Sha256::digest(client_key);
    let server_key = hmac(&salted, &[b"Server Key"]);
    format!(
        "SCRAM-SHA-256${ITERATIONS}:{}${}:{}",
        base64(salt),
        base64(&stored_key),
        base64(&server_key)
    )
}

/// A verifier with a fresh random salt.
pub fn verifier(password: &Secret<String>) -> Result<String, getrandom::Error> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt)?;
    Ok(verifier_with_salt(password, &salt))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // test code

    use super::*;

    #[test]
    fn matches_a_known_verifier() {
        // RFC 7677's password and salt; the expected keys were computed
        // independently (Python's hashlib and hmac).
        let salt = [
            0x5b, 0x6d, 0x99, 0x68, 0x9d, 0x12, 0x35, 0x8e, 0xec, 0xa0, 0x4b, 0x14, 0x12, 0x36,
            0xfa, 0x81,
        ];
        assert_eq!(
            verifier_with_salt(&Secret::new("pencil".to_owned()), &salt),
            "SCRAM-SHA-256$4096:W22ZaJ0SNY7soEsUEjb6gQ==$WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU="
        );
    }

    #[test]
    fn salts_are_random() {
        let password = Secret::new("x".to_owned());
        assert_ne!(verifier(&password).unwrap(), verifier(&password).unwrap());
    }
}
