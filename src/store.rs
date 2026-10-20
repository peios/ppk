//! The artifact directory's layout, and writing into it safely.

use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::{ALGORITHM, PackageId};

/// Everything in the artifact directory is immutable once written: content
/// is named by its hash, and a ref is never replaced.
const ARTIFACT_MODE: u32 = 0o444;

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

    pub(crate) fn manifest_dir(&self) -> PathBuf {
        self.root.join("manifests").join(ALGORITHM)
    }

    pub(crate) fn ref_dir(&self, id: &PackageId) -> PathBuf {
        self.root.join("refs").join(id.name()).join(id.version())
    }

    pub(crate) fn ref_path(&self, id: &PackageId) -> PathBuf {
        self.ref_dir(id).join(id.arch())
    }
}

/// The identity a ref names, or `None` if there is no ref at `path`.
pub(crate) fn read_ref(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents.trim_end().to_owned())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("{}", path.display())),
    }
}

/// Copy `path` into `blob`, checking that the content still hashes to `hex`
/// so a file modified mid-pack cannot be stored under the wrong name.
pub(crate) fn write_blob(path: &Path, hex: &str, blob_dir: &Path, blob: &Path) -> Result<()> {
    let file = File::open(path).with_context(|| format!("{}", path.display()))?;
    let mut reader = HashingReader {
        inner: file,
        hasher: Sha256::new(),
    };
    let mut tmp = new_temp(blob_dir)?;
    io::copy(&mut reader, &mut tmp).with_context(|| format!("{}", path.display()))?;
    if format!("{:x}", reader.hasher.finalize()) != hex {
        bail!("{}: file changed while being packed", path.display());
    }
    persist(tmp, blob)
}

/// Create `dest` holding `bytes`, via a temporary file in `dir` so readers
/// never see a partial file. Never replaces an existing `dest`: returns true
/// if `dest` now holds `bytes`, or false if it already held something else.
pub(crate) fn write_new(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<bool> {
    let mut tmp = new_temp(dir)?;
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

/// A temporary file in `dir`, already read-only by mode. The handle returned
/// stays writable, because access is checked when the file is opened.
fn new_temp(dir: &Path) -> Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(".tmp-")
        .permissions(Permissions::from_mode(ARTIFACT_MODE))
        .tempfile_in(dir)
        .with_context(|| format!("{}: cannot create a temporary file", dir.display()))
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
