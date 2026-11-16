//! Where packages are fetched and resolved from: an artifact directory, or
//! a repository served over HTTP.

use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::compression;
use crate::remote::{self, Remote};
use crate::repo::{Blobs, Pointer, PublicKey, Release, Standing, Trees};
use crate::store::{self, ArtifactDir};
use crate::{ALGORITHM, Identity, PackageId};

/// An artifact directory, or a repository served over HTTP.
#[derive(Clone, Debug)]
pub enum Source {
    Dir(PathBuf),
    Remote(Remote),
}

impl Source {
    /// The source `s` names: a repository served over HTTP if it is an
    /// `http` or `https` URL, and otherwise the artifact directory at that
    /// path.
    pub fn parse(s: &str) -> Result<Self> {
        match remote::is_url(s) {
            true => Ok(Self::Remote(Remote::new(&[s.to_owned()])?)),
            false => Ok(Self::Dir(PathBuf::from(s))),
        }
    }

    /// The same source, keeping what it reads over HTTP in `cache`.
    pub fn with_cache(self, cache: &Path) -> Self {
        match self {
            Self::Dir(dir) => Self::Dir(dir),
            Self::Remote(remote) => Self::Remote(remote.with_cache(cache)),
        }
    }

    /// Its pointer file, or `None` if it has none. A directory's is checked
    /// against `key`; a remote repository's against its own.
    pub(crate) fn pointer(&self, key: Option<&PublicKey>) -> Result<Option<Pointer>> {
        match self {
            Self::Dir(dir) => crate::repo::read_pointer(dir, key),
            Self::Remote(remote) => remote.pointer(),
        }
    }

    /// How its blobs are written, as its pointer file says: plain if it
    /// has none. A directory's pointer file is read without its key; a
    /// blob is checked against its hash however it is written.
    pub(crate) fn blobs(&self) -> Result<Blobs> {
        Ok(self
            .pointer(None)?
            .map(|pointer| pointer.blobs)
            .unwrap_or_default())
    }

    /// A reader of the content of the blob file at `path` below its top,
    /// decompressed if its blobs are `blobs`, and where it is read from.
    /// Nothing checks the content.
    pub(crate) fn open_blob(&self, path: &Path, blobs: Blobs) -> Result<(String, Box<dyn Read>)> {
        let (from, reader) = self.open(path)?;
        match blobs {
            Blobs::Plain => Ok((from, reader)),
            Blobs::Zstd => {
                let reader = compression::decoder(reader).with_context(|| from.clone())?;
                Ok((from, Box::new(reader)))
            }
        }
    }

    pub(crate) fn trees(&self) -> Trees {
        match self {
            Self::Dir(dir) => Trees::new(dir),
            Self::Remote(remote) => Trees::remote(remote),
        }
    }

    /// The identity it gives the package `id`, or `None` if it doesn't
    /// have it: a directory's ref, or a remote repository's catalog, which
    /// it must have.
    pub(crate) fn identity(&self, id: &PackageId) -> Result<Option<Identity>> {
        match self {
            Self::Dir(dir) => store::read_ref(&ArtifactDir::new(dir).ref_path(id)),
            Self::Remote(_) => Ok(self.listed(id)?.map(|release| release.identity)),
        }
    }

    /// The version `id` as a remote repository's catalog lists it, unless
    /// it is deleted; `None` for a directory, whose refs say instead.
    pub(crate) fn listed(&self, id: &PackageId) -> Result<Option<Release>> {
        let Self::Remote(remote) = self else {
            return Ok(None);
        };
        let Some(pointer) = remote.pointer()? else {
            bail!("{remote}: not published as a repository, which fetching over HTTP needs");
        };
        let Some(catalog) = &pointer.catalog else {
            return Ok(None);
        };
        Ok((Trees::remote(remote)
            .lookup(catalog, id.name())?
            .into_iter())
        .find(|release| {
            release.version == id.version()
                && release.arch == id.arch()
                && !matches!(release.standing, Standing::Deleted(_))
        }))
    }

    /// The manifest `identity`, checked against its hash.
    pub(crate) fn manifest(&self, identity: &Identity) -> Result<Vec<u8>> {
        match self {
            Self::Dir(dir) => {
                let path = ArtifactDir::new(dir).manifest_path(identity);
                let bytes = fs::read(&path).with_context(|| format!("{}", path.display()))?;
                if Identity::of(&bytes) != *identity {
                    bail!("{}: does not match its hash", path.display());
                }
                Ok(bytes)
            }
            Self::Remote(remote) => remote.object("manifests", identity),
        }
    }

    /// Where the manifest `identity` is, to name in an error: its path, or
    /// its URL below the first base URL.
    pub(crate) fn manifest_place(&self, identity: &Identity) -> String {
        match self {
            Self::Dir(dir) => (ArtifactDir::new(dir).manifest_path(identity))
                .display()
                .to_string(),
            Self::Remote(remote) => {
                format!(
                    "{}manifests/{ALGORITHM}/{}",
                    remote.urls()[0],
                    identity.hex()
                )
            }
        }
    }

    /// A reader of the file at `path` below its top, which is relative,
    /// and where it is read from, to name in an error. Nothing checks what
    /// it reads.
    pub(crate) fn open(&self, path: &Path) -> Result<(String, Box<dyn Read>)> {
        match self {
            Self::Dir(dir) => {
                let path = dir.join(path);
                let file = File::open(&path).with_context(|| format!("{}", path.display()))?;
                Ok((path.display().to_string(), Box::new(file)))
            }
            Self::Remote(remote) => {
                let path = path.to_str().expect("a store's paths are ASCII");
                let (url, reader) = remote.open(path)?;
                Ok((url, Box::new(reader)))
            }
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dir(dir) => write!(f, "{}", dir.display()),
            Self::Remote(remote) => write!(f, "{remote}"),
        }
    }
}

impl From<PathBuf> for Source {
    fn from(dir: PathBuf) -> Self {
        Self::Dir(dir)
    }
}

impl From<&Path> for Source {
    fn from(dir: &Path) -> Self {
        Self::Dir(dir.to_owned())
    }
}
