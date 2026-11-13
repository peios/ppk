//! A repository's key: ML-DSA-65, as the rest of Peios signs with. The
//! private key is a PEM file of the kind OpenSSL writes (`openssl genpkey
//! -algorithm ML-DSA-65`), of which only the seed is needed; the public key
//! is written `ml-dsa-65:` and the base64 of its encoding, to go in a
//! repository's configuration.
//!
//! What is signed is the pointer file's bytes, in the context
//! `ppk repository`, so that a signature over a pointer file is never also a
//! signature over anything else this key signs.

use std::fs;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use ml_dsa::{EncodedVerifyingKey, MlDsa65, Signature, SigningKey, VerifyingKey};

/// The context every pointer file signature is made in.
const CONTEXT: &[u8] = b"ppk repository";

/// What a public key's string starts with.
const PUBLIC_PREFIX: &str = "ml-dsa-65:";

/// The DER encoding of the object identifier of ML-DSA-65.
const ML_DSA_65: &[u8] = &[
    0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x03, 0x12,
];

/// The DER tags read here.
const INTEGER: u8 = 0x02;
const OCTET_STRING: u8 = 0x04;
const SEQUENCE: u8 = 0x30;
/// A private key's seed alone, `[0] IMPLICIT OCTET STRING`.
const SEED: u8 = 0x80;

/// A repository's private key, which signs its pointer file.
pub struct PrivateKey {
    signing: SigningKey<MlDsa65>,
}

impl PrivateKey {
    /// Read the private key in the PEM file at `path`. No error tells of
    /// what the file holds.
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read(path).map_err(|_| anyhow!("{}: cannot read the key", path.display()))?;
        let text = String::from_utf8(text).map_err(|_| anyhow!("the key is not a PEM file"))?;
        let private = pem(&text, "PRIVATE KEY")?;
        let (seed, expanded) = seed(&private)?;
        let signing = SigningKey::<MlDsa65>::from_seed(&seed.into());
        // The expanded key, which OpenSSL writes beside the seed, is made
        // from it; one that differs is a damaged file.
        #[allow(deprecated)]
        if expanded
            .is_some_and(|expanded| signing.expanded_key().to_expanded().as_slice() != expanded)
        {
            bail!("the private key's expanded key is not its seed's");
        }
        Ok(Self { signing })
    }

    pub fn public(&self) -> PublicKey {
        PublicKey {
            verifying: self.signing.expanded_key().verifying_key(),
        }
    }

    /// The signature of `bytes`, checked before it is returned.
    pub fn sign(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let signature = self
            .signing
            .expanded_key()
            .sign_deterministic(bytes, CONTEXT)
            .map_err(|_| anyhow!("cannot sign"))?;
        if !self.public().verifies(bytes, &signature.encode()) {
            bail!("the signature made does not verify");
        }
        Ok(signature.encode().to_vec())
    }
}

/// A repository's public key, which checks its pointer file's signature.
#[derive(Clone, Debug, PartialEq)]
pub struct PublicKey {
    verifying: VerifyingKey<MlDsa65>,
}

impl PublicKey {
    /// Whether `signature` is this key's signature of `bytes`.
    pub fn verifies(&self, bytes: &[u8], signature: &[u8]) -> bool {
        Signature::<MlDsa65>::try_from(signature).is_ok_and(|signature| {
            self.verifying
                .verify_with_context(bytes, CONTEXT, &signature)
        })
    }
}

impl std::fmt::Display for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let encoded = base64::engine::general_purpose::STANDARD.encode(self.verifying.encode());
        write!(f, "{PUBLIC_PREFIX}{encoded}")
    }
}

impl std::str::FromStr for PublicKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let encoded = s
            .strip_prefix(PUBLIC_PREFIX)
            .ok_or_else(|| anyhow!("a public key starts with {PUBLIC_PREFIX}"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| anyhow!("the public key is not base64"))?;
        let encoded = EncodedVerifyingKey::<MlDsa65>::try_from(bytes.as_slice())
            .map_err(|_| anyhow!("the public key is not an ML-DSA-65 key"))?;
        Ok(Self {
            verifying: VerifyingKey::decode(&encoded),
        })
    }
}

/// The bytes of the PEM block labelled `label` in `text`.
fn pem(text: &str, label: &str) -> Result<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let body = text
        .split_once(&begin)
        .and_then(|(_, rest)| rest.split_once(&end))
        .map(|(body, _)| body)
        .ok_or_else(|| anyhow!("the key file has no {label}"))?;
    let body: String = body.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|_| anyhow!("the key file's {label} is not base64"))
}

/// The seed of the ML-DSA-65 private key `key`, a PKCS#8 `PrivateKeyInfo`
/// holding the seed alone or the seed and the expanded key, and the
/// expanded key if it holds it.
fn seed(key: &[u8]) -> Result<([u8; 32], Option<&[u8]>)> {
    let invalid = || anyhow!("the private key is not a PKCS#8 ML-DSA-65 key");
    let (info, _) = tlv(key, SEQUENCE).ok_or_else(invalid)?;
    let (_, rest) = tlv(info, INTEGER).ok_or_else(invalid)?;
    let (algorithm, rest) = tlv(rest, SEQUENCE).ok_or_else(invalid)?;
    if algorithm != ML_DSA_65 {
        bail!("the private key is not an ML-DSA-65 key");
    }
    let (private, _) = tlv(rest, OCTET_STRING).ok_or_else(invalid)?;
    let (seed, expanded) = match private.first() {
        Some(&SEED) => (tlv(private, SEED).map(|(seed, _)| seed), None),
        Some(&SEQUENCE) => {
            let both = tlv(private, SEQUENCE).map(|(both, _)| both);
            let seed = both.and_then(|both| tlv(both, OCTET_STRING));
            let expanded = seed.and_then(|(_, rest)| tlv(rest, OCTET_STRING));
            (
                seed.map(|(seed, _)| seed),
                Some(expanded.ok_or_else(invalid)?.0),
            )
        }
        _ => bail!("the private key holds no seed, which signing needs"),
    };
    let seed = seed
        .and_then(|seed| seed.try_into().ok())
        .ok_or_else(invalid)?;
    Ok((seed, expanded))
}

/// The content of the DER element with tag `tag` that `input` starts with,
/// and what follows it.
fn tlv(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    if *input.first()? != tag {
        return None;
    }
    let header = header_length(input)?;
    let length = match input[1] {
        short @ 0..=0x7f => short as usize,
        // At most four octets, which header_length checks.
        _ => input[2..header]
            .iter()
            .fold(0usize, |length, byte| length << 8 | *byte as usize),
    };
    let end = header.checked_add(length)?;
    Some((input.get(header..end)?, &input[end..]))
}

/// The length of the tag and length octets of the DER element `input`
/// starts with.
fn header_length(input: &[u8]) -> Option<usize> {
    match *input.get(1)? {
        0..=0x7f => Some(2),
        0x81..=0x84 => {
            let header = 2 + (input[1] & 0x7f) as usize;
            (input.len() >= header).then_some(header)
        }
        _ => None,
    }
}
