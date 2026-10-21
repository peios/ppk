//! The artifact directory's layout, and writing into it safely.

use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::access;
use crate::{ALGORITHM, Identity, PackageId};

/// Everything in the artifact directory is immutable once written: content
/// is named by its hash, and a ref is never replaced.
const ARTIFACT_MODE: u32 = 0o444;

/// The mode of a blob's executable form: read-only like every artifact, and
/// executable, because closures hardlink it and a link shares its mode.
const EXEC_BLOB_MODE: u32 = 0o555;

/// What distinguishes a blob's executable form from its plain form, which has
/// no suffix. Hex digits never contain it, so the two cannot collide.
const EXEC_SUFFIX: &str = ".exec";

/// Where each kind of artifact lives under an artifact directory's root.
pub(crate) struct ArtifactDir<'a> {
    root: &'a Path,
}

impl<'a> ArtifactDir<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self { root }
    }

    pub(crate) fn blob_dir(&self) -> PathBuf {
        self.root.join("blobs").join(ALGORITHM)
    }

    /// The stored form of the blob `hex` that an entry with this `exec` needs.
    /// Content is stored once per form, not once per hash.
    pub(crate) fn blob_path(&self, hex: &str, exec: bool) -> PathBuf {
        let suffix = if exec { EXEC_SUFFIX } else { "" };
        self.blob_dir().join(format!("{hex}{suffix}"))
    }

    pub(crate) fn manifest_dir(&self) -> PathBuf {
        self.root.join("manifests").join(ALGORITHM)
    }

    pub(crate) fn manifest_path(&self, identity: &Identity) -> PathBuf {
        self.manifest_dir().join(identity.hex())
    }

    pub(crate) fn ref_dir(&self, id: &PackageId) -> PathBuf {
        self.root.join("refs").join(id.name()).join(id.version())
    }

    pub(crate) fn ref_path(&self, id: &PackageId) -> PathBuf {
        self.ref_dir(id).join(id.arch())
    }
}

/// The identity a ref names, or `None` if there is no ref at `path`. A ref
/// holds exactly one identity and a newline; anything else is an error.
pub(crate) fn read_ref(path: &Path) -> Result<Option<Identity>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("{}", path.display())),
    };
    let Some(identity) = contents.strip_suffix('\n') else {
        bail!("{}: not a ref", path.display());
    };
    let identity = identity
        .parse()
        .with_context(|| format!("{}", path.display()))?;
    Ok(Some(identity))
}

/// The bytes of a ref naming `identity`.
pub(crate) fn ref_contents(identity: &Identity) -> String {
    format!("{identity}\n")
}

/// Copy `path` into `blob`, the form given by [`ArtifactDir::blob_path`], if
/// its content hashes to `hex`. Returns false, writing nothing, if it does
/// not; the caller knows what that means.
pub(crate) fn copy_blob(path: &Path, blob: &Path, hex: &str, exec: bool) -> Result<bool> {
    let file = File::open(path).with_context(|| format!("{}", path.display()))?;
    let mut reader = HashingReader {
        inner: file,
        hasher: Sha256::new(),
    };
    let blob_dir = blob
        .parent()
        .expect("a blob path is inside the blob directory");
    let mode = if exec { EXEC_BLOB_MODE } else { ARTIFACT_MODE };
    let mut tmp = new_temp(blob_dir, mode)?;
    io::copy(&mut reader, &mut tmp).with_context(|| format!("{}", path.display()))?;
    if Identity::from_digest(reader.hasher.finalize()).hex() != hex {
        return Ok(false);
    }
    persist(tmp, blob)?;
    Ok(true)
}

/// Create `dest` holding `bytes`, via a temporary file in `dir` so readers
/// never see a partial file. Never replaces an existing `dest`: returns true
/// if `dest` now holds `bytes`, or false if it already held something else.
pub(crate) fn write_new(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<bool> {
    let mut tmp = new_temp(dir, ARTIFACT_MODE)?;
    tmp.write_all(bytes)
        .and_then(|()| tmp.as_file().sync_all())
        .with_context(|| format!("{}", dest.display()))?;
    match tmp.persist_noclobber(dest) {
        Ok(_) => Ok(true),
        Err(err) if err.error.kind() == io::ErrorKind::AlreadyExists => {
            Ok(fs::read(dest).with_context(|| format!("{}", dest.display()))? == bytes)
        }
        Err(err) => Err(err.error).with_context(|| format!("{}", dest.display())),
    }
}

/// A temporary file in `dir`, already read-only by `mode` and carrying the
/// store's security descriptor. The handle returned stays writable, because
/// access is checked when the file is opened.
fn new_temp(dir: &Path, mode: u32) -> Result<tempfile::NamedTempFile> {
    let tmp = tempfile::Builder::new()
        .prefix(".tmp-")
        .permissions(Permissions::from_mode(mode))
        .tempfile_in(dir)
        .with_context(|| format!("{}: cannot create a temporary file", dir.display()))?;
    access::protect_file(tmp.as_file(), tmp.path())?;
    Ok(tmp)
}

/// Create `dir` and any missing parents. Each directory created gets the
/// store's security descriptor, which what is later created in it inherits.
pub(crate) fn create_dirs(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        create_dirs(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => access::protect_dir(dir),
        // Another run created it first.
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(err) => Err(err).with_context(|| format!("{}", dir.display())),
    }
}

fn persist(tmp: tempfile::NamedTempFile, dest: &Path) -> Result<()> {
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("{}", dest.display()))?;
    tmp.persist(dest)
        .map_err(|err| err.error)
        .with_context(|| format!("{}", dest.display()))?;
    Ok(())
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}
