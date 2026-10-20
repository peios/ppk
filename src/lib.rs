//! Packing for the Peios package format.
//!
//! [`pack`] turns a directory into content-addressed blobs and a manifest in
//! an artifact directory:
//!
//! ```text
//! <artifact-dir>/blobs/sha256/<hex>
//!     file content, uncompressed, named by its SHA-256
//! <artifact-dir>/manifests/sha256/<hex>
//!     a manifest, named by the SHA-256 of its bytes
//! <artifact-dir>/refs/<name>/<version>/<arch>
//!     "sha256:<hex>\n": which manifest that package is
//! ```
//!
//! A manifest records name, version, arch, and a [files] table mapping each
//! path to an inline table whose first key says what kind of entry it is:
//!
//! ```text
//! "bin/hello" = { blob = "sha256:<hex>", exec = true }
//! "share/doc/hello/README" = { blob = "sha256:<hex>" }
//! "lib/libz.so" = { symlink = "libz.so.1" }
//! "var/lib/hello" = { dir = true }
//! ```
//!
//! The manifest is emitted by hand rather than through a TOML serializer so
//! that its bytes are canonical: the same input tree always produces the same
//! manifest, and so the same hash. That hash is the package's identity.
//!
//! The name, version and arch are written into the manifest as well as its
//! ref. The ref is only an index, rebuildable from the manifests; a signature
//! will cover the manifest's bytes, so those bytes must say which package
//! they are.

mod id;

use std::fmt::Write as _;
use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub use id::{HOST_ARCH, PackageId};

/// The hash algorithm, as it appears in artifact paths and manifest values.
pub const ALGORITHM: &str = "sha256";

/// Where packages are stored unless told otherwise.
pub const DEFAULT_ARTIFACT_DIR: &str = "/system/artifacts";

/// Everything in the artifact directory is immutable once written: content
/// is named by its hash, and a ref is never replaced.
const ARTIFACT_MODE: u32 = 0o444;

/// The result of a successful pack.
#[derive(Debug)]
pub struct Packed {
    /// The package's identity: `sha256:<hex>` of the manifest's bytes.
    pub identity: String,
    /// Where the manifest was written.
    pub manifest_path: PathBuf,
    /// Where the ref naming the manifest was written.
    pub ref_path: PathBuf,
    /// Number of entries recorded in the manifest.
    pub entries: usize,
    /// Number of blobs written; files whose blob already existed are not counted.
    pub blobs_written: usize,
}

/// One entry in a manifest's `[files]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A regular file whose content is the blob with this hex SHA-256.
    /// `exec` is true if any execute bit was set on the source file.
    Blob { hex: String, exec: bool },
    /// A symbolic link to this target, recorded exactly as `readlink` returns
    /// it. A relative target resolves from the link's directory; an absolute
    /// one from the root of the system the package is installed into.
    Symlink(String),
    /// An empty directory. Directories with anything in them are implied by
    /// their contents and never listed, so each tree has one manifest.
    Dir,
}

/// Something found while walking the content directory.
enum Found {
    File { path: PathBuf, exec: bool },
    Symlink(String),
    Dir,
}

/// Pack the content directory `input` into `artifact_dir` as `id`.
///
/// Regular files, symlinks and directories are supported. Symlinks are
/// recorded, never followed; empty directories are recorded; of a file's mode
/// only "any execute bit set" is kept. Special files, setuid or setgid files,
/// and paths or symlink targets that are not valid UTF-8 are errors rather
/// than being skipped.
///
/// A packed package is immutable: packing the same name, version and arch
/// again succeeds only if it produces the identical manifest. Blobs are
/// written first, then the manifest, then the ref, so a ref never names a
/// manifest that is missing or incomplete.
pub fn pack(id: &PackageId, input: &Path, artifact_dir: &Path) -> Result<Packed> {
    if !fs::metadata(input)
        .with_context(|| format!("{}", input.display()))?
        .is_dir()
    {
        bail!("{}: not a directory", input.display());
    }
    let input_real = fs::canonicalize(input).with_context(|| format!("{}", input.display()))?;
    if resolve(artifact_dir)?.starts_with(&input_real) {
        bail!(
            "{}: artifact directory is inside the input directory {}",
            artifact_dir.display(),
            input.display()
        );
    }

    let mut found = Vec::new();
    walk(input, input, &mut found)?;
    found.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    // Hash everything before writing anything, so that a refused pack leaves
    // the artifact directory untouched.
    let mut entries = Vec::with_capacity(found.len());
    let mut sources = Vec::new();
    for (rel, item) in found {
        let entry = match item {
            Found::File { path, exec } => {
                let hex = hash_file(&path)?;
                sources.push((hex.clone(), path));
                Entry::Blob { hex, exec }
            }
            Found::Symlink(target) => Entry::Symlink(target),
            Found::Dir => Entry::Dir,
        };
        entries.push((rel, entry));
    }

    let manifest = render_manifest(id, &entries);
    let hex = format!("{:x}", Sha256::digest(manifest.as_bytes()));
    let identity = format!("{ALGORITHM}:{hex}");

    let ref_dir = artifact_dir.join("refs").join(id.name()).join(id.version());
    let ref_path = ref_dir.join(id.arch());
    // The ref write at the end still refuses to replace a ref that appears
    // after this check.
    if let Some(existing) = read_ref(&ref_path)?.filter(|existing| *existing != identity) {
        bail!("{id}: already packed as {existing}; a packed version cannot be replaced");
    }

    let blob_dir = artifact_dir.join("blobs").join(ALGORITHM);
    fs::create_dir_all(&blob_dir).with_context(|| format!("{}", blob_dir.display()))?;
    let mut blobs_written = 0;
    for (hex, path) in &sources {
        let blob = blob_dir.join(hex);
        if !blob.exists() {
            write_blob(path, hex, &blob_dir, &blob)?;
            blobs_written += 1;
        }
    }

    let manifest_dir = artifact_dir.join("manifests").join(ALGORITHM);
    fs::create_dir_all(&manifest_dir).with_context(|| format!("{}", manifest_dir.display()))?;
    let manifest_path = manifest_dir.join(&hex);
    if !write_new(&manifest_dir, &manifest_path, manifest.as_bytes())? {
        bail!("{}: does not match its hash", manifest_path.display());
    }

    fs::create_dir_all(&ref_dir).with_context(|| format!("{}", ref_dir.display()))?;
    if !write_new(&ref_dir, &ref_path, format!("{identity}\n").as_bytes())? {
        let existing = read_ref(&ref_path)?.unwrap_or_default();
        bail!("{id}: already packed as {existing}; a packed version cannot be replaced");
    }

    Ok(Packed {
        identity,
        manifest_path,
        ref_path,
        entries: entries.len(),
        blobs_written,
    })
}

/// Render the manifest for `entries`, which must already be sorted by path.
///
/// Every entry is an inline table with single spaces inside the braces and
/// around `=`, keys in a fixed order, and `exec` written only when true, so
/// each entry has exactly one spelling.
pub fn render_manifest<P: AsRef<str>>(id: &PackageId, entries: &[(P, Entry)]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "name = {}", toml_string(id.name()));
    let _ = writeln!(out, "version = {}", toml_string(id.version()));
    let _ = writeln!(out, "arch = {}", toml_string(id.arch()));
    out.push_str("\n[files]\n");
    for (path, entry) in entries {
        let value = match entry {
            Entry::Blob { hex, exec: false } => format!("blob = \"{ALGORITHM}:{hex}\""),
            Entry::Blob { hex, exec: true } => {
                format!("blob = \"{ALGORITHM}:{hex}\", exec = true")
            }
            Entry::Symlink(target) => format!("symlink = {}", toml_string(target)),
            Entry::Dir => "dir = true".to_owned(),
        };
        let _ = writeln!(out, "{} = {{ {value} }}", toml_string(path.as_ref()));
    }
    out
}

/// Quote `s` as a TOML basic string in one fixed spelling: `"` and `\` are
/// backslash-escaped, every control character is written as `\uXXXX`, and
/// everything else is literal UTF-8.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() && (c as u32) < 0x80 => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The identity a ref names, or `None` if there is no ref at `path`.
fn read_ref(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents.trim_end().to_owned())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("{}", path.display())),
    }
}

/// Collect every regular file, symlink and empty directory under `dir`, keyed
/// by its path relative to `root`. Symlinks are recorded, never followed.
fn walk(root: &Path, dir: &Path, found: &mut Vec<(String, Found)>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("{}", dir.display()))? {
        let entry = entry.with_context(|| format!("{}", dir.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .with_context(|| format!("{}", path.display()))?;
        if kind.is_dir() {
            // Anything inside a directory adds at least one entry (or fails
            // the walk), so no new entries means it was empty.
            let before = found.len();
            walk(root, &path, found)?;
            if found.len() == before {
                found.push((relative(root, &path)?, Found::Dir));
            }
        } else if kind.is_file() {
            let mode = entry
                .metadata()
                .with_context(|| format!("{}", path.display()))?
                .permissions()
                .mode();
            if mode & 0o6000 != 0 {
                bail!(
                    "{}: setuid and setgid bits are not supported",
                    path.display()
                );
            }
            let exec = mode & 0o111 != 0;
            found.push((relative(root, &path)?, Found::File { path, exec }));
        } else if kind.is_symlink() {
            let target = fs::read_link(&path).with_context(|| format!("{}", path.display()))?;
            let Some(target) = target.to_str() else {
                bail!("{}: symlink target is not valid UTF-8", path.display());
            };
            found.push((relative(root, &path)?, Found::Symlink(target.to_owned())));
        } else {
            bail!(
                "{}: not a regular file, symlink or directory",
                path.display()
            );
        }
    }
    Ok(())
}

/// `path` relative to `root`, with `/` separators and no leading `/`.
fn relative(root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(root)
        .expect("walked path is under its root");
    let mut parts = Vec::new();
    for component in rel.components() {
        let Component::Normal(part) = component else {
            bail!("{}: unexpected path component", path.display());
        };
        let Some(part) = part.to_str() else {
            bail!("{}: path is not valid UTF-8", path.display());
        };
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("{}", path.display()))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).with_context(|| format!("{}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Copy `path` into `blob`, checking that the content still hashes to `hex`
/// so a file modified mid-pack cannot be stored under the wrong name.
fn write_blob(path: &Path, hex: &str, blob_dir: &Path, blob: &Path) -> Result<()> {
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
fn write_new(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<bool> {
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

/// The absolute, symlink-resolved form of `path`, which need not exist yet:
/// its longest existing ancestor is canonicalised and the rest appended.
fn resolve(path: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(path).with_context(|| format!("{}", path.display()))?;
    let mut existing = abs.as_path();
    let mut tail = Vec::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            break;
        };
        tail.push(name);
        existing = parent;
    }
    let mut resolved =
        fs::canonicalize(existing).with_context(|| format!("{}", existing.display()))?;
    resolved.extend(tail.iter().rev());
    Ok(resolved)
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
