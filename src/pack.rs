//! Packing a directory into the artifact directory.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::store::{self, ArtifactDir};
use crate::{Entry, Identity, PackageId, render_manifest};

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
    let identity = Identity::of(manifest.as_bytes());

    let layout = ArtifactDir::new(artifact_dir);
    let ref_dir = layout.ref_dir(id);
    let ref_path = layout.ref_path(id);
    // The ref write at the end still refuses to replace a ref that appears
    // after this check.
    if let Some(existing) = store::read_ref(&ref_path)?.filter(|existing| *existing != identity) {
        bail!("{id}: already packed as {existing}; a packed version cannot be replaced");
    }

    let blob_dir = layout.blob_dir();
    fs::create_dir_all(&blob_dir).with_context(|| format!("{}", blob_dir.display()))?;
    let mut blobs_written = 0;
    for (hex, path) in &sources {
        let blob = blob_dir.join(hex);
        if !blob.exists() {
            if !store::copy_blob(path, hex, &blob_dir, &blob)? {
                bail!("{}: file changed while being packed", path.display());
            }
            blobs_written += 1;
        }
    }

    let manifest_dir = layout.manifest_dir();
    fs::create_dir_all(&manifest_dir).with_context(|| format!("{}", manifest_dir.display()))?;
    let manifest_path = layout.manifest_path(&identity);
    if !store::write_new(&manifest_dir, &manifest_path, manifest.as_bytes())? {
        bail!("{}: does not match its hash", manifest_path.display());
    }

    fs::create_dir_all(&ref_dir).with_context(|| format!("{}", ref_dir.display()))?;
    if !store::write_new(
        &ref_dir,
        &ref_path,
        store::ref_contents(&identity).as_bytes(),
    )? {
        let existing = store::read_ref(&ref_path)?.map(|existing| existing.to_string());
        bail!(
            "{id}: already packed as {}; a packed version cannot be replaced",
            existing.unwrap_or_default()
        );
    }

    Ok(Packed {
        identity: identity.to_string(),
        manifest_path,
        ref_path,
        entries: entries.len(),
        blobs_written,
    })
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
    Ok(Identity::from_digest(hasher.finalize()).hex().to_owned())
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
