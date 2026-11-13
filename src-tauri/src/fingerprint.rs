//! Content fingerprinting: chunked BLAKE3.
//!
//! Two levels of identity come out of this module:
//!
//! * **chunk digests** — BLAKE3 of each 1 MiB slice. Storing them lets the
//!   product answer "are these two files *partly* the same?" and cluster files
//!   that belong to one project, which a whole-file hash cannot do;
//! * **root digest** — BLAKE3 over the concatenated chunk digests. It is the
//!   logical identity of the file's contents and replaces V1's whole-file hash.
//!
//! ## The incremental fast path
//!
//! Persisting a BLAKE3 hasher state is not possible, so "incremental" here
//! means something narrower and honest: when a file has only **grown** and the
//! previously stored digests are still valid for its prefix, only the new tail
//! is hashed. The prefix is not trusted blindly — the last reused chunk is
//! re-read and re-hashed, and a mismatch falls back to a full digest. That
//! makes the common cases (downloads finishing, logs appending, a document
//! being saved with more pages) cheap without ever producing a wrong answer.
//! Files that were rewritten in place fail the probe and are hashed fully.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const CHUNK_SIZE: u64 = 1024 * 1024;

/// Hard ceiling for content hashing. Bigger files are marked `too_large`
/// instead of blocking the collector — the back-pressure valve.
pub const MAX_HASH_BYTES: u64 = 512 * 1024 * 1024;

/// Never reuse more than this many stored chunks (guards against a corrupt or
/// absurd prefix making us allocate wildly).
const MAX_REUSE_CHUNKS: u64 = 100_000;

#[derive(Debug)]
pub enum HashError {
    TooLarge,
    Io(std::io::Error),
}

impl From<std::io::Error> for HashError {
    fn from(error: std::io::Error) -> Self {
        HashError::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDigest {
    pub index: u32,
    pub hash: String,
}

#[derive(Debug, Clone)]
pub struct FileDigest {
    pub chunks: Vec<ChunkDigest>,
    /// Logical content identity: BLAKE3 over the chunk digests.
    pub root: String,
    /// True when a stored prefix was revalidated and reused.
    pub incremental: bool,
    pub size: u64,
}

fn hex_to_bytes32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = hex.as_bytes();
    for i in 0..32 {
        let hi = (bytes[i * 2] as char).to_digit(16)?;
        let lo = (bytes[i * 2 + 1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

fn read_chunk(file: &mut File, index: u64, buffer: &mut [u8]) -> std::io::Result<usize> {
    file.seek(SeekFrom::Start(index * CHUNK_SIZE))?;
    let mut filled = 0usize;
    while filled < buffer.len() {
        let n = file.read(&mut buffer[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

fn root_of(chunks: &[ChunkDigest]) -> Option<String> {
    let mut hasher = blake3::Hasher::new();
    for chunk in chunks {
        let bytes = hex_to_bytes32(&chunk.hash)?;
        hasher.update(&bytes);
    }
    Some(hasher.finalize().to_hex().to_string())
}

/// Digest a file. `reuse` is the previously stored chunk list for this file,
/// if any; pass `None` to force a full digest.
pub fn digest_file(path: &Path, reuse: Option<&[ChunkDigest]>) -> Result<FileDigest, HashError> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_HASH_BYTES {
        return Err(HashError::TooLarge);
    }
    let total_chunks = size.div_ceil(CHUNK_SIZE);

    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; CHUNK_SIZE as usize];
    let mut chunks: Vec<ChunkDigest> = Vec::new();
    let mut start_index: u64 = 0;
    let mut incremental = false;

    if let Some(previous) = reuse {
        let count = previous.len() as u64;
        if count > 0 && count < total_chunks && count <= MAX_REUSE_CHUNKS {
            if let Some(last) = previous.last() {
                if last.index as u64 == count - 1 {
                    if let Some(expected) = hex_to_bytes32(&last.hash) {
                        let filled = read_chunk(&mut file, count - 1, &mut buffer)?;
                        let probe = blake3::hash(&buffer[..filled]);
                        if probe.as_bytes() == &expected {
                            chunks.extend_from_slice(previous);
                            start_index = count;
                            incremental = true;
                        }
                    }
                }
            }
        }
    }

    if !incremental {
        chunks.clear();
        start_index = 0;
    }

    for index in start_index..total_chunks {
        let filled = read_chunk(&mut file, index, &mut buffer)?;
        let digest = blake3::hash(&buffer[..filled]);
        chunks.push(ChunkDigest {
            index: index as u32,
            hash: digest.to_hex().to_string(),
        });
    }

    let root = root_of(&chunks).ok_or_else(|| {
        HashError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "chunk digest was not valid hex",
        ))
    })?;

    Ok(FileDigest {
        chunks,
        root,
        incremental,
        size,
    })
}
