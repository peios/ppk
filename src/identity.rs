//! Identities: the hash that names a manifest or a blob.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

use crate::ALGORITHM;

/// The length of a SHA-256 hash written in hexadecimal.
const HEX_LEN: usize = 64;

/// A SHA-256 hash, spelled `sha256:<hex>` with 64 lowercase hex digits.
///
/// The hex is the only part of an identity that becomes a path component,
/// and parsing admits nothing else, so an identity read from an untrusted
/// source can name only a file directly inside the directory it is joined
/// to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Identity {
    hex: String,
}

impl Identity {
    /// The identity of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self::from_digest(Sha256::digest(bytes))
    }

    pub(crate) fn from_digest(digest: impl fmt::LowerHex) -> Self {
        Self {
            hex: format!("{digest:x}"),
        }
    }

    /// The hash in lowercase hexadecimal, as used in artifact paths.
    pub fn hex(&self) -> &str {
        &self.hex
    }
}

impl FromStr for Identity {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let hex = s
            .strip_prefix(ALGORITHM)
            .and_then(|rest| rest.strip_prefix(':'));
        match hex {
            Some(hex)
                if hex.len() == HEX_LEN
                    && hex
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
            {
                Ok(Self {
                    hex: hex.to_owned(),
                })
            }
            _ => bail!("'{s}' is not a {ALGORITHM} identity"),
        }
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{ALGORITHM}:{}", self.hex)
    }
}
