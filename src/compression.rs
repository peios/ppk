//! Compressed blobs: a repository whose pointer file says `blobs zstd`
//! holds each blob as a zstd frame of its content, under the name the
//! content's hash gives it. A store's blobs are never compressed, since a
//! closure links them, so a blob is decompressed as it is fetched and
//! checked against its hash as it is.

use std::io::{self, Read, Write};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::Identity;
use crate::store::HashingReader;

/// The most a compressed blob may decompress to. A blob that would be more
/// is refused, so that a small file can't fill the store.
pub(crate) const LIMIT: u64 = 64 << 30;

/// A reader of what the zstd frames `reader` reads decompress to, failing
/// once that is more than [`LIMIT`].
pub(crate) fn decoder<'a>(reader: impl Read + 'a) -> Result<impl Read + 'a> {
    Ok(Capped {
        inner: zstd::stream::read::Decoder::new(reader).context("cannot start decompressing")?,
        left: LIMIT,
        limit: LIMIT,
    })
}

/// A reader that fails once it has read more than `limit` bytes.
struct Capped<R> {
    inner: R,
    left: u64,
    limit: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let read = u64::try_from(n).expect("a read fits in 64 bits");
        if read > self.left {
            return Err(io::Error::other(format!(
                "decompresses to more than {} bytes, the most a blob may be",
                self.limit
            )));
        }
        self.left -= read;
        Ok(n)
    }
}

/// Compress what `reader` reads to `writer` at `level`, returning the
/// identity of what was read, so that the caller can check it.
pub(crate) fn compress(reader: impl Read, writer: impl Write, level: i32) -> Result<Identity> {
    let mut reader = HashingReader {
        inner: reader,
        hasher: Sha256::new(),
    };
    let mut encoder = zstd::stream::write::Encoder::new(writer, level)?;
    io::copy(&mut reader, &mut encoder)?;
    encoder.finish()?.flush()?;
    Ok(Identity::from_digest(reader.hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blob_compresses_and_decompresses_to_its_content() {
        let content = b"hello, hello, hello, hello".repeat(100);
        let mut compressed = Vec::new();
        let identity = compress(&content[..], &mut compressed, 12).unwrap();
        assert_eq!(identity, Identity::of(&content));
        assert!(compressed.len() < content.len());
        let mut out = Vec::new();
        decoder(&compressed[..])
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, content);
    }

    #[test]
    fn more_than_the_limit_is_refused() {
        let content = vec![0u8; 1 << 20];
        let mut compressed = Vec::new();
        compress(&content[..], &mut compressed, 3).unwrap();
        let mut capped = Capped {
            inner: zstd::stream::read::Decoder::new(&compressed[..]).unwrap(),
            left: 1000,
            limit: 1000,
        };
        let err = io::copy(&mut capped, &mut io::sink()).unwrap_err();
        assert!(err.to_string().contains("more than 1000 bytes"), "{err}");
    }

    #[test]
    fn what_isnt_a_zstd_frame_is_refused() {
        let mut out = Vec::new();
        assert!(
            decoder(&b"not zstd at all"[..])
                .unwrap()
                .read_to_end(&mut out)
                .is_err()
        );
    }
}
