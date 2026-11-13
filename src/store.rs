//! The artifact directory's layout, and writing into it safely.

use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::access::{self, Security, Stamp};
use crate::manifest::render_entry;
use crate::security::check_signature;
use crate::{ALGORITHM, Entry, Identity, PackageId};

/// Everything in the artifact directory is immutable once written: content
/// is named by its hash, and a ref is never replaced.
#[cfg(not(test))]
const ARTIFACT_MODE: u32 = 0o444;

/// The mode of a blob's executable form: read-only like every artifact, and
/// executable, because closures hardlink it and a link shares its mode.
#[cfg(not(test))]
const EXEC_BLOB_MODE: u32 = 0o555;

/// Unit tests give files `user.` attributes in place of `security.` ones
/// (see `access`), which only a process that can write a file may set, so
/// what they store is writable by its owner.
#[cfg(test)]
const ARTIFACT_MODE: u32 = 0o644;

#[cfg(test)]
const EXEC_BLOB_MODE: u32 = 0o755;

/// What distinguishes a blob's executable form from its plain form, which has
/// no suffix. Hex digits never contain it, so the two cannot collide.
const EXEC_SUFFIX: &str = ".exec";

/// What separates a blob's hash from the hash of the entry it is stored for,
/// in the form of an entry with security metadata. The second hash is hex
/// digits, so it never spells `exec`.
const FORM_SEPARATOR: &str = ".";

/// Where each kind of artifact lives under an artifact directory's root.
pub(crate) struct ArtifactDir<'a> {
    root: &'a Path,
}

impl<'a> ArtifactDir<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self { root }
    }

    pub(crate) fn root(&self) -> &Path {
        self.root
    }

    pub(crate) fn blob_dir(&self) -> PathBuf {
        self.root.join("blobs").join(ALGORITHM)
    }

    /// The stored form of the blob `hex` that an entry with this `exec` and
    /// no security metadata needs. Content is stored once per form, not once
    /// per hash.
    pub(crate) fn blob_path(&self, hex: &str, exec: bool) -> PathBuf {
        let suffix = if exec { EXEC_SUFFIX } else { "" };
        self.blob_dir().join(format!("{hex}{suffix}"))
    }

    /// The stored form of the file `entry`: [`blob_path`]'s without security
    /// metadata, and with it `<hex>.<form>`, where `<form>` is the SHA-256
    /// of the entry as the manifest writes it. So each file with metadata is
    /// stored as a file of its own, carrying it, which its links share.
    ///
    /// [`blob_path`]: Self::blob_path
    pub(crate) fn file_path(&self, entry: &Entry) -> PathBuf {
        let Entry::Blob { hex, exec, .. } = entry else {
            panic!("only a file entry is stored as a file");
        };
        if !entry.has_security() {
            return self.blob_path(hex, *exec);
        }
        let form = Identity::of(render_entry(entry).as_bytes());
        self.blob_dir()
            .join(format!("{hex}{FORM_SEPARATOR}{}", form.hex()))
    }

    /// The signature in the blob `hex`, checked against its hash and as a
    /// signature.
    pub(crate) fn read_signature(&self, hex: &str) -> Result<Vec<u8>> {
        let path = self.blob_path(hex, false);
        let bytes = fs::read(&path).with_context(|| format!("{}", path.display()))?;
        if Identity::of(&bytes).hex() != hex {
            bail!("{}: does not match its hash", path.display());
        }
        check_signature(&bytes).with_context(|| format!("{}", path.display()))?;
        Ok(bytes)
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

/// The identity `id` is packed as in `artifact_dir`, or `None` if it is not
/// packed there.
pub fn packed(id: &PackageId, artifact_dir: &Path) -> Result<Option<Identity>> {
    read_ref(&ArtifactDir::new(artifact_dir).ref_path(id))
}

/// Every package indexed by a ref, sorted by its full ID. Manifests and
/// blobs are not read. A missing refs directory is an empty store.
pub fn stored_packages(artifact_dir: &Path) -> Result<Vec<(PackageId, Identity)>> {
    let mut packages = Vec::new();
    for name in ref_entries(&artifact_dir.join("refs"))? {
        let name = name.with_context(|| format!("{}: refs", artifact_dir.display()))?;
        if !name.file_type()?.is_dir() {
            continue;
        }
        for version in ref_entries(&name.path())? {
            let version = version?;
            if !version.file_type()?.is_dir() {
                continue;
            }
            for arch in ref_entries(&version.path())? {
                let arch = arch?;
                if !arch.file_type()?.is_file() {
                    continue;
                }
                let (name, version, arch_name) =
                    (name.file_name(), version.file_name(), arch.file_name());
                let (Some(name), Some(version), Some(arch_name)) =
                    (name.to_str(), version.to_str(), arch_name.to_str())
                else {
                    continue;
                };
                let Ok(id) = PackageId::new(name, version, arch_name) else {
                    continue;
                };
                if let Some(identity) = read_ref(&arch.path())? {
                    packages.push((id, identity));
                }
            }
        }
    }
    packages.sort_by_cached_key(|(id, _)| id.to_string());
    Ok(packages)
}

fn ref_entries(dir: &Path) -> Result<Vec<io::Result<fs::DirEntry>>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.collect()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err).with_context(|| format!("{}", dir.display())),
    }
}

/// Create `artifact_dir` if it is missing, and check that files can be
/// created in it, so that a caller with costly work to do before packing
/// finds out first.
pub fn prepare_artifact_dir(artifact_dir: &Path) -> Result<()> {
    create_dirs(artifact_dir)?;
    // Removed again when dropped.
    new_temp(artifact_dir, ARTIFACT_MODE)?;
    Ok(())
}

/// The versions of the package `name` packed for `arch` in `artifact_dir`,
/// oldest first, in the order of versions (PSPU 5.6). Two spellings of one
/// version, such as `1.0~alpha-1` and `1.0~a-1`, are in byte order.
pub fn versions(name: &str, arch: &str, artifact_dir: &Path) -> Result<Vec<String>> {
    let dir = artifact_dir.join("refs").join(name);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("{}", dir.display())),
    };
    let mut versions: Vec<PackageId> = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("{}", dir.display()))?;
        let Some(version) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // A version is packed for `arch` when its directory holds that ref.
        let Ok(id) = PackageId::new(name, &version, arch) else {
            continue;
        };
        if packed(&id, artifact_dir)?.is_some() {
            versions.push(id);
        }
    }
    versions.sort_by(|a, b| {
        a.parsed_version()
            .cmp(b.parsed_version())
            .then_with(|| a.version().cmp(b.version()))
    });
    Ok(versions.iter().map(|id| id.version().to_owned()).collect())
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

/// Copy `path` to `dest`, as a blob's form is stored: read-only, executable
/// if `exec`, with the store's security descriptor, and carrying `security`,
/// given before `dest` exists. `dest` is a file of a tree that copies its
/// blobs; an artifact directory's blobs are copied with [`copy_pending`].
/// Returns false, writing nothing, if the content does not hash to `hex`;
/// the caller knows what that means. Adds one to `unstamped` if this system
/// would not let the copy carry all of `security`.
///
/// The copy isn't synced: a tree has thousands of files, and a sync for each
/// is most of the time it takes to write them. The caller calls
/// [`sync_filesystem`] once, before it gives the tree its name.
pub(crate) fn copy_blob(
    path: &Path,
    dest: &Path,
    (hex, exec): (&str, bool),
    security: Security,
    unstamped: &mut usize,
) -> Result<bool> {
    let Some(pending) = copy_pending(path, dest, (hex, exec), security, unstamped)? else {
        return Ok(false);
    };
    pending.place()?;
    Ok(true)
}

/// A file written under a temporary name beside `dest`, to be given its name
/// by [`place_pending`].
pub(crate) struct Pending {
    tmp: tempfile::TempPath,
    dest: PathBuf,
    /// For a file that must not replace another: what `dest` may already
    /// hold, and what to say if it holds something else.
    new: Option<(Vec<u8>, String)>,
}

impl Pending {
    fn place(self) -> Result<()> {
        let Some((bytes, conflict)) = self.new else {
            return self
                .tmp
                .persist(&self.dest)
                .map_err(|err| err.error)
                .with_context(|| format!("{}", self.dest.display()));
        };
        match self.tmp.persist_noclobber(&self.dest) {
            Ok(()) => Ok(()),
            Err(err) if err.error.kind() == io::ErrorKind::AlreadyExists => {
                if fs::read(&self.dest).with_context(|| format!("{}", self.dest.display()))?
                    != bytes
                {
                    bail!("{conflict}");
                }
                Ok(())
            }
            Err(err) => Err(err.error).with_context(|| format!("{}", self.dest.display())),
        }
    }
}

/// Write `bytes` under a temporary name in `dir`, to be named `dest` by
/// [`place_pending`]. Unless `replace`, it doesn't replace a `dest` that
/// exists already, which must then hold `bytes`, or placing it fails, saying
/// `conflict`.
pub(crate) fn write_pending(
    dir: &Path,
    dest: &Path,
    bytes: &[u8],
    replace: bool,
    conflict: impl FnOnce() -> String,
) -> Result<Pending> {
    let mut tmp = new_temp(dir, ARTIFACT_MODE)?;
    tmp.write_all(bytes)
        .with_context(|| format!("{}", dest.display()))?;
    Ok(Pending {
        tmp: tmp.into_temp_path(),
        dest: dest.to_owned(),
        new: (!replace).then(|| (bytes.to_vec(), conflict())),
    })
}

/// Copy `path` as [`copy_blob`] does, but under a temporary name beside
/// `dest`. A blob already named is trusted without being read again, so an
/// artifact directory's blobs are named only once their content is on disk:
/// [`place_pending`] syncs them all at once and then names them. Returns
/// `None`, writing nothing, if the content does not hash to `hex`.
pub(crate) fn copy_pending(
    path: &Path,
    dest: &Path,
    (hex, exec): (&str, bool),
    security: Security,
    unstamped: &mut usize,
) -> Result<Option<Pending>> {
    let file = File::open(path).with_context(|| format!("{}", path.display()))?;
    let mut reader = HashingReader {
        inner: file,
        hasher: Sha256::new(),
    };
    let dir = dest
        .parent()
        .expect("a blob's destination is in a directory");
    let mode = if exec { EXEC_BLOB_MODE } else { ARTIFACT_MODE };
    let mut tmp = new_temp(dir, mode)?;
    io::copy(&mut reader, &mut tmp).with_context(|| format!("{}", path.display()))?;
    if Identity::from_digest(reader.hasher.finalize()).hex() != hex {
        return Ok(None);
    }
    if security.stamp(tmp.path())? == Stamp::Skipped {
        *unstamped += 1;
    }
    // Closed, so that a package of thousands of files doesn't hold thousands
    // of descriptors open until they're named.
    Ok(Some(Pending {
        tmp: tmp.into_temp_path(),
        dest: dest.to_owned(),
        new: None,
    }))
}

/// Sync the filesystem holding `dir`, once, and then name each of `pending`,
/// in order.
pub(crate) fn place_pending(dir: &Path, pending: Vec<Pending>) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    sync_filesystem(dir)?;
    for each in pending {
        each.place()?;
    }
    Ok(())
}

/// Write everything written to the filesystem that holds `path` to its disk,
/// as one sync rather than one for each file.
pub(crate) fn sync_filesystem(path: &Path) -> Result<()> {
    let dir = File::open(path).with_context(|| format!("{}", path.display()))?;
    // SAFETY: syncfs reads only the descriptor, which `dir` holds open.
    if unsafe { libc::syncfs(dir.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("{}: cannot sync", path.display()));
    }
    Ok(())
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

/// Create or replace `dest` holding `bytes`, via a temporary file in `dir`
/// so readers never see a partial file.
pub(crate) fn write_replacing(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = new_temp(dir, ARTIFACT_MODE)?;
    tmp.write_all(bytes)
        .with_context(|| format!("{}", dest.display()))?;
    persist(tmp, dest)
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
