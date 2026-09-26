//! The snapshot file format.
//!
//! ```text
//! magic "TETHSNAP" (8) || format (1) || header length, u32 BE (4) || header (JSON)
//! || nonce prefix (19)
//! || frames: last flag (1: 0 more, 1 last) || length, u32 BE (4) || sealed chunk
//! ```
//!
//! The header is plaintext, so snapshots can be listed without the key,
//! but it can't be changed: every chunk is sealed with the SHA-256 of
//! everything before the prefix as associated data. Chunks are sealed with
//! [`tether_core::crypto::StreamSealer`] (STREAM over XChaCha20-Poly1305),
//! so nothing is ever held whole in memory.

use sha2::{Digest, Sha256};
use tether_core::crypto::{
    CryptoError, EncryptionKey, STREAM_CHUNK, STREAM_PREFIX_LEN, STREAM_TAG,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::SnapshotError;

const MAGIC: &[u8; 8] = b"TETHSNAP";
const FORMAT: u8 = 1;
/// Headers are small (a few KiB of migration records); anything bigger
/// isn't a snapshot.
const MAX_HEADER: u32 = 1 << 20;
const MORE: u8 = 0;
const LAST: u8 = 1;
/// Separates this use of the header hash from any other.
const AAD_CONTEXT: &[u8] = b"tether snapshot v1\0";

/// The bytes before the nonce prefix, for a header.
fn head(header_json: &[u8]) -> Result<Vec<u8>, SnapshotError> {
    let len = u32::try_from(header_json.len())
        .ok()
        .filter(|len| *len <= MAX_HEADER)
        .ok_or_else(|| SnapshotError::Corrupt("the header is too big".to_owned()))?;
    let mut head = Vec::with_capacity(MAGIC.len() + 5 + header_json.len());
    head.extend_from_slice(MAGIC);
    head.push(FORMAT);
    head.extend_from_slice(&len.to_be_bytes());
    head.extend_from_slice(header_json);
    Ok(head)
}

fn aad(head: &[u8]) -> Vec<u8> {
    let mut aad = AAD_CONTEXT.to_vec();
    aad.extend_from_slice(&Sha256::digest(head));
    aad
}

fn crypto(err: CryptoError) -> SnapshotError {
    match err {
        CryptoError::Decrypt => SnapshotError::Decrypt,
        other => SnapshotError::Crypto(other),
    }
}

/// Reads up to one chunk; shorter only at the end of `input`.
async fn read_chunk<R: AsyncRead + Unpin>(input: &mut R) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; STREAM_CHUNK];
    let mut filled = 0;
    while filled < STREAM_CHUNK {
        let n = input.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

/// Writes a snapshot: the header, then `input` sealed chunk by chunk.
/// Returns the plaintext bytes sealed.
pub(crate) async fn seal<R, W>(
    key: &EncryptionKey,
    header_json: &[u8],
    input: &mut R,
    out: &mut W,
) -> Result<u64, SnapshotError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let head = head(header_json)?;
    let mut sealer = key.stream_sealer(&aad(&head)).map_err(crypto)?;
    let io = |e| SnapshotError::io("writing the snapshot", e);
    out.write_all(&head).await.map_err(io)?;
    out.write_all(&sealer.prefix()).await.map_err(io)?;
    let read = |e| SnapshotError::io("reading pg_dump's output", e);
    let mut total = 0u64;
    let mut current = read_chunk(input).await.map_err(read)?;
    loop {
        // A short chunk means the input ended; a full one needs a look at
        // what follows to know whether it was the last.
        let next = if current.len() < STREAM_CHUNK {
            Vec::new()
        } else {
            read_chunk(input).await.map_err(read)?
        };
        let last = next.is_empty();
        let sealed = sealer.seal(&current, last).map_err(crypto)?;
        let len = u32::try_from(sealed.len())
            .map_err(|_| SnapshotError::Corrupt("a chunk is too big".to_owned()))?;
        out.write_all(&[if last { LAST } else { MORE }])
            .await
            .map_err(io)?;
        out.write_all(&len.to_be_bytes()).await.map_err(io)?;
        out.write_all(&sealed).await.map_err(io)?;
        total += current.len() as u64;
        if last {
            break;
        }
        current = next;
    }
    out.flush().await.map_err(io)?;
    Ok(total)
}

/// Reads a snapshot's header: `(the bytes it covers, the JSON)`.
pub(crate) async fn read_head<R: AsyncRead + Unpin>(
    input: &mut R,
) -> Result<(Vec<u8>, Vec<u8>), SnapshotError> {
    let not_snapshot = || SnapshotError::Corrupt("this isn't a Tether snapshot".to_owned());
    let mut fixed = [0u8; 13];
    input
        .read_exact(&mut fixed)
        .await
        .map_err(|_| not_snapshot())?;
    if &fixed[..8] != MAGIC {
        return Err(not_snapshot());
    }
    if fixed[8] != FORMAT {
        return Err(SnapshotError::Corrupt(format!(
            "snapshot format {} is newer than this version of Tether reads",
            fixed[8]
        )));
    }
    let len = u32::from_be_bytes([fixed[9], fixed[10], fixed[11], fixed[12]]);
    if len > MAX_HEADER {
        return Err(not_snapshot());
    }
    let mut json = vec![0u8; len as usize];
    input
        .read_exact(&mut json)
        .await
        .map_err(|_| not_snapshot())?;
    let head = head(&json)?;
    Ok((head, json))
}

/// Opens the sealed body that follows `head` in `input`, writing the
/// plaintext to `out` as it goes. Fails if anything was changed, cut short
/// or added after the end. What was written before a failure must be
/// thrown away.
pub(crate) async fn open<R, W>(
    key: &EncryptionKey,
    head: &[u8],
    input: &mut R,
    out: &mut W,
) -> Result<u64, SnapshotError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let truncated = || SnapshotError::Corrupt("the snapshot is cut short".to_owned());
    let mut prefix = [0u8; STREAM_PREFIX_LEN];
    input
        .read_exact(&mut prefix)
        .await
        .map_err(|_| truncated())?;
    let mut opener = key.stream_opener(prefix, &aad(head));
    let write = |e| SnapshotError::io("passing the snapshot on", e);
    let mut total = 0u64;
    let mut sealed = Vec::with_capacity(STREAM_CHUNK + STREAM_TAG);
    loop {
        let mut frame = [0u8; 5];
        input
            .read_exact(&mut frame)
            .await
            .map_err(|_| truncated())?;
        let last = match frame[0] {
            MORE => false,
            LAST => true,
            _ => return Err(SnapshotError::Corrupt("a chunk is malformed".to_owned())),
        };
        let len = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
        if len > STREAM_CHUNK + STREAM_TAG {
            return Err(SnapshotError::Corrupt("a chunk is malformed".to_owned()));
        }
        sealed.resize(len, 0);
        input
            .read_exact(&mut sealed)
            .await
            .map_err(|_| truncated())?;
        let plain = opener.open(&sealed, last).map_err(crypto)?;
        out.write_all(&plain).await.map_err(write)?;
        total += plain.len() as u64;
        if last {
            break;
        }
    }
    let mut extra = [0u8; 1];
    let n = input
        .read(&mut extra)
        .await
        .map_err(|e| SnapshotError::io("reading the snapshot", e))?;
    if n != 0 {
        return Err(SnapshotError::Corrupt(
            "the snapshot has data after its end".to_owned(),
        ));
    }
    out.flush().await.map_err(write)?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tether_core::Secret;

    fn key() -> EncryptionKey {
        EncryptionKey::from_hex(&Secret::new("ab".repeat(32)))
            .unwrap()
            .derive(crate::KEY_LABEL)
            .unwrap()
    }

    async fn sealed(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let n = seal(&key(), br#"{"a":1}"#, &mut &data[..], &mut out)
            .await
            .unwrap();
        assert_eq!(n, data.len() as u64);
        out
    }

    async fn opened(file: &[u8]) -> Result<Vec<u8>, SnapshotError> {
        let mut input = file;
        let (head, json) = read_head(&mut input).await?;
        assert_eq!(json, br#"{"a":1}"#);
        let mut out = Vec::new();
        open(&key(), &head, &mut input, &mut out).await?;
        Ok(out)
    }

    #[tokio::test]
    async fn round_trips_any_size() {
        for len in [0, 5, STREAM_CHUNK, 2 * STREAM_CHUNK + 3] {
            let data: Vec<u8> = (0..len).map(|i| (i % 253) as u8).collect();
            let file = sealed(&data).await;
            assert_eq!(opened(&file).await.unwrap(), data, "{len}");
        }
    }

    #[tokio::test]
    async fn refuses_changes_truncation_and_trailing_data() {
        let data = vec![1u8; 2 * STREAM_CHUNK + 10];
        let file = sealed(&data).await;

        // The header is authenticated: change a byte of it.
        let mut header_changed = file.clone();
        header_changed[15] = b'b';
        let mut input = &header_changed[..];
        let (head, _) = read_head(&mut input).await.unwrap();
        let err = open(&key(), &head, &mut input, &mut tokio::io::sink())
            .await
            .unwrap_err();
        assert!(matches!(err, SnapshotError::Decrypt), "{err}");

        for cut in [file.len() - 1, file.len() - 20, 40] {
            let err = opened(&file[..cut]).await.unwrap_err();
            assert!(matches!(err, SnapshotError::Corrupt(_)), "{cut}: {err}");
        }
        let mut trailing = file.clone();
        trailing.push(0);
        assert!(matches!(
            opened(&trailing).await.unwrap_err(),
            SnapshotError::Corrupt(_)
        ));
        // The last flag on the first frame: claims the stream ends there.
        let mut flag = file.clone();
        let first_frame = 13 + 7 + STREAM_PREFIX_LEN;
        assert_eq!(flag[first_frame], MORE);
        flag[first_frame] = LAST;
        assert!(matches!(
            opened(&flag).await.unwrap_err(),
            SnapshotError::Decrypt
        ));
        // Another key.
        let mut input = &file[..];
        let (head, _) = read_head(&mut input).await.unwrap();
        let other = EncryptionKey::from_hex(&Secret::new("cd".repeat(32))).unwrap();
        assert!(matches!(
            open(&other, &head, &mut input, &mut tokio::io::sink())
                .await
                .unwrap_err(),
            SnapshotError::Decrypt
        ));
        assert!(matches!(
            opened(b"PGDMP not ours").await.unwrap_err(),
            SnapshotError::Corrupt(_)
        ));
    }
}
