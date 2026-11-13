use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// Hard ceiling for content hashing. Bigger files are marked `too_large`
/// instead of blocking the collector — this is the back-pressure valve
/// described in the design notes.
pub const MAX_HASH_BYTES: u64 = 512 * 1024 * 1024;

const CHUNK: usize = 1024 * 1024;

#[derive(Debug)]
pub enum HashError {
    TooLarge,
    Io(std::io::Error),
}

impl From<std::io::Error> for HashError {
    fn from(e: std::io::Error) -> Self {
        HashError::Io(e)
    }
}

/// BLAKE3 over the whole file, read in 1 MiB chunks.
///
/// Chunked reading keeps peak memory flat regardless of file size; the chunk
/// boundaries are also what a future incremental hasher will key off, so files
/// that change by a single block do not need a full re-read.
pub fn hash_file(path: &Path) -> Result<String, HashError> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_HASH_BYTES {
        return Err(HashError::TooLarge);
    }

    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(CHUNK, file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}
