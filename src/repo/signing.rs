//! How a repository is signed and how its signature is checked: with a PSO
//! bundle over its pointer file, as PSPU 5.33 specifies.
//!
//! A publisher signs with a [`Signer`]: a signing certificate, its key, and
//! the identity the repository signs as. A machine checks with a [`Trust`]:
//! the identity its configuration names, and whether a chain that no root
//! vouches for will do.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use pso::core::{
    Certificate, Crypto, Hash, HashForm, Invalid, Key, Outcome, Refusal, RustCrypto, Verifier,
    Warrant, encode_bundle,
};
use pso::keys::SecretKey;
use pso::store::{self, DirectoryLookup};

/// The warrant a certificate needs to sign a repository's pointer file, and
/// that the leaf over one holds.
const REPOSITORY: &str = "Repository";

/// The keys of a signer file.
const SIGNER_KEYS: [&str; 5] = ["identity", "certificate", "key", "passphrase-file", "carry"];

/// What publishes a repository: who it signs as, and what it signs with.
pub struct Signer {
    identity: String,
    certificate: Certificate,
    key: SecretKey,
    /// The certificates above the signing certificate that the signature
    /// carries, in order upward.
    carried: Vec<Vec<u8>>,
}

impl Signer {
    /// A signer that signs as `identity` with `certificate`, whose secret
    /// key is `key`, carrying `carried` above it.
    pub fn new(
        identity: &str,
        certificate: Certificate,
        key: SecretKey,
        carried: Vec<Vec<u8>>,
    ) -> Self {
        Self {
            identity: identity.into(),
            certificate,
            key,
            carried,
        }
    }

    /// Read the signer file at `path`, a TOML file naming the identity, the
    /// signing certificate, its secret key, and any certificates to carry:
    ///
    /// ```toml
    /// identity = "pkgs.example.com"
    /// certificate = "signer.cert"
    /// key = "signer.key"
    /// passphrase-file = "signer.passphrase"
    /// carry = ["intermediate.cert"]
    /// ```
    ///
    /// A path that isn't absolute is beside the signer file. If the key is
    /// sealed, its passphrase is the first line of `passphrase-file`, or is
    /// asked for on the terminal when there is none and standard input is
    /// one. The signer file names only paths, never a passphrase.
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .map_err(|_| anyhow!("{}: cannot read the signer file", path.display()))?;
        let doc: toml::Table = text
            .parse()
            .with_context(|| format!("{}: malformed signer file", path.display()))?;
        if let Some(key) = doc.keys().find(|key| !SIGNER_KEYS.contains(&key.as_str())) {
            bail!("{}: unknown key {key:?}", path.display());
        }
        let string = |key: &str| -> Result<&str> {
            doc.get(key)
                .and_then(toml::Value::as_str)
                .ok_or_else(|| anyhow!("{}: {key} must be a string", path.display()))
        };
        let beside = |file: &str| path.parent().unwrap_or(Path::new(".")).join(file);
        let certificate = |file: &str| -> Result<Certificate> {
            let file = beside(file);
            let bytes = fs::read(&file).with_context(|| format!("{}", file.display()))?;
            Certificate::decode(&bytes)
                .map_err(|err| anyhow!("{}: not a certificate: {err:?}", file.display()))
        };
        let carry = match doc.get("carry") {
            None => Vec::new(),
            Some(toml::Value::Array(files)) => files
                .iter()
                .map(|file| {
                    let file = file
                        .as_str()
                        .ok_or_else(|| anyhow!("{}: carry must hold strings", path.display()))?;
                    Ok(certificate(file)?.bytes().to_vec())
                })
                .collect::<Result<_>>()?,
            Some(_) => bail!("{}: carry must be an array", path.display()),
        };
        let passphrase_file = match doc.get("passphrase-file") {
            None => None,
            Some(file) => Some(beside(file.as_str().ok_or_else(|| {
                anyhow!("{}: passphrase-file must be a string", path.display())
            })?)),
        };
        let passphrase = pso::passphrase::Source::from_options(
            None,
            passphrase_file.as_deref(),
            "name a passphrase-file in the signer file",
        );
        Ok(Self {
            identity: string("identity")?.into(),
            certificate: certificate(string("certificate")?)?,
            key: SecretKey::load_with(&beside(string("key")?), &passphrase)?,
            carried: carry,
        })
    }

    /// The identity this signs as.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The signature of `bytes`: a bundle whose leaf is issued under the
    /// identity and holds `Repository`. It carries the signing certificate
    /// and every certificate above it that the signer was given, since a
    /// machine reading a repository for the first time has none of them.
    pub fn sign(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let repository = Warrant::set(REPOSITORY, &[], true).expect("a valid warrant");
        let leaf = pso::issue::leaf(
            &self.certificate,
            &self.key,
            HashForm::WholeFile,
            RustCrypto.sha512(&[bytes]),
            &self.identity,
            vec![repository],
        )
        .context("cannot sign the pointer file")?;
        let certificates: Vec<&[u8]> = std::iter::once(self.certificate.bytes())
            .chain(self.carried.iter().map(Vec::as_slice))
            .collect();
        encode_bundle(leaf.bytes(), &certificates)
            .map_err(|err| anyhow!("cannot sign the pointer file: {err:?}"))
    }
}

/// The roots a verifier holds and the system whose trust store it reads.
#[derive(Debug, PartialEq)]
pub struct Anchors {
    roots: Vec<Key>,
    /// The root directory of the system whose trust store is read.
    system: PathBuf,
}

impl Anchors {
    /// Anchors holding `roots`, reading the trust store of the system at
    /// `system`.
    pub fn new(roots: Vec<Key>, system: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            roots,
            system: system.into(),
        })
    }

    /// The roots this build of ppk was given, and this system's trust
    /// store.
    ///
    /// A build with debug assertions, which no release is, reads its roots
    /// and the system's root from the directory `$PPK_DEVELOPMENT_ANCHORS`
    /// names instead, if it is set: every `*.pub` file in its `roots`
    /// directory, and its `system` directory. ppk's own tests use it.
    pub fn system() -> Result<Arc<Self>> {
        #[cfg(debug_assertions)]
        if let Some(directory) = std::env::var_os("PPK_DEVELOPMENT_ANCHORS") {
            return Self::development(Path::new(&directory));
        }
        let roots = pso_roots::ROOTS
            .iter()
            .map(|(name, text)| pso::keys::parse_public(text).with_context(|| name.to_string()))
            .collect::<Result<_>>()
            .context("the roots ppk was built with")?;
        Ok(Self::new(roots, "/"))
    }

    #[cfg(debug_assertions)]
    fn development(directory: &Path) -> Result<Arc<Self>> {
        let roots_dir = directory.join("roots");
        let mut files: Vec<PathBuf> = fs::read_dir(&roots_dir)
            .with_context(|| format!("{}", roots_dir.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<_>>()?;
        files.sort();
        let roots = files
            .iter()
            .filter(|file| file.extension().is_some_and(|extension| extension == "pub"))
            .map(|file| pso::keys::load_public(file))
            .collect::<Result<_>>()?;
        Ok(Self::new(roots, directory.join("system")))
    }
}

/// What a machine requires of a repository's signature (PSPU 5.33): the
/// identity it is signed under, whether a chain no root vouches for will
/// do, and optionally the top-level its chain has to end at.
#[derive(Clone, Debug, PartialEq)]
pub struct Trust {
    pub identity: String,
    /// Accept an Unrooted chain as well as a Rooted one.
    pub unrooted: bool,
    /// The object hash of the top-level the chain has to end at.
    pub anchor: Option<Hash>,
    anchors: Arc<Anchors>,
}

impl Trust {
    /// A requirement that a repository is signed as `identity`, by a chain
    /// that `anchors` roots.
    pub fn new(identity: &str, anchors: Arc<Anchors>) -> Self {
        Self {
            identity: identity.into(),
            unrooted: false,
            anchor: None,
            anchors,
        }
    }

    /// Whether `signature` signs `bytes` as this requires.
    pub fn verifies(&self, bytes: &[u8], signature: &[u8]) -> bool {
        self.check(bytes, signature).is_ok()
    }

    /// Fails, saying why, unless `signature` signs `bytes` as this requires.
    pub fn check(&self, bytes: &[u8], signature: &[u8]) -> Result<()> {
        let anchors = &self.anchors;
        let loaded = store::load(&anchors.system, &anchors.roots)?;
        let verifier = Verifier {
            crypto: &RustCrypto,
            roots: &anchors.roots,
            store: &loaded.store,
            recognized: &[REPOSITORY],
        };
        let mut lookup = DirectoryLookup::new(anchors.system.join(store::PACKAGE_INTERMEDIARIES));
        let content_hash = RustCrypto.sha512(&[bytes]);
        let outcome = verifier
            .verify(signature, HashForm::WholeFile, &content_hash, &mut lookup)
            .map_err(|_| anyhow!("the signature could not be checked"))?;
        let verified = match &outcome {
            Outcome::Rooted(verified) => verified,
            Outcome::Unrooted(verified) if self.unrooted => verified,
            Outcome::Unrooted(_) => bail!("no root vouches for the chain that signed it"),
            Outcome::Consistent(_) => bail!("the chain that signed it is not trusted here"),
            Outcome::Refused(Refusal::Revoked) => bail!("its signature is revoked"),
            Outcome::Refused(Refusal::Distrusted) => bail!("its signer is distrusted here"),
            Outcome::Incomplete { .. } => bail!("a certificate of its chain is missing"),
            Outcome::Invalid(Invalid::ContentMismatch) => bail!("its signature is of other bytes"),
            Outcome::Invalid(_) => bail!("its signature is invalid"),
        };
        if verified.warrant(REPOSITORY).is_none() {
            bail!("its signer cannot sign a repository");
        }
        if verified.name != self.identity {
            bail!("it is signed as {}", verified.name);
        }
        if self
            .anchor
            .is_some_and(|anchor| anchor != verified.top_level)
        {
            bail!("its chain does not end at the configured anchor");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// A root that does not parse would stop ppk checking any repository.
    #[test]
    fn the_roots_ppk_is_built_with_are_keys() {
        assert!(!pso_roots::ROOTS.is_empty());
        for (name, text) in pso_roots::ROOTS {
            pso::keys::parse_public(text).unwrap_or_else(|err| panic!("{name}: {err:#}"));
        }
    }
}
