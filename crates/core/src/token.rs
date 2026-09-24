use sha2::{Digest, Sha256};

use crate::Secret;

/// A new random bearer token (256 bits, hex). Only its hash is stored.
pub fn new_token() -> Result<Secret<String>, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(Secret::new(hex(&bytes)))
}

/// What the database stores in place of a token.
pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_hex_and_hash_to_32_bytes() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.expose().len(), 64);
        assert!(a.expose().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.expose(), b.expose());
        assert_eq!(hash_token(a.expose()).len(), 32);
        assert_eq!(hash_token("x"), hash_token("x"));
    }
}
