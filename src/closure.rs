//! Closures: packages composed into one tree of their blobs' files, as
//! hardlinks unless the caller asks for copies.

use std::collections::BTreeMap;
use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::access;
use crate::manifest::toml_string;
use crate::store::{self, ArtifactDir};
use crate::{Entry, Identity, PackageId, parse_manifest};

/// The mode of a closure's root, so that anyone can read the tree; the
/// temporary directory it is built in would otherwise be private.
const ROOT_MODE: u32 = 0o755;

/// The suffix of a closure's manifest, beside its tree.
const MANIFEST_SUFFIX: &str = ".manifest";

/// How a tree holds its packages' files.
///
/// A hardlink is the blob: the same file, with the blob's one security
/// descriptor, taking no space. A hardlink can't cross a mount, even between
/// two mounts of one filesystem, and the kernel is the only judge of that.
///
/// A copy can be made anywhere, and is checked against its hash as it is
/// made, as `fetch` checks what it copies, so its source need not be trusted.
/// It has the mode and security descriptor of the blob it came from, but as a
/// file of its own, so a change to the store's descriptors does not reach it.
/// That and the space are why copying is asked for, never fallen back on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsrFiles {
    /// Hardlink each file to its blob.
    #[default]
    Hardlink,
    /// Copy each file from its blob.
    Copy,
}

/// The result of a successful closure.
#[derive(Debug)]
pub struct Closed {
    /// The closure's identity: `sha256:<hex>` of its manifest's bytes.
    pub identity: String,
    /// The root of the closure's tree.
    pub tree_path: PathBuf,
    /// Where the closure's manifest was written.
    pub manifest_path: PathBuf,
    /// True if the closure already existed and nothing was built.
    pub reused: bool,
}

/// Compose `packages`, each already in `artifact_dir`, into a closure in
/// `closure_dir`.
///
/// A closure is named by the hash of its manifest, which lists the
/// identities of its packages sorted and without duplicates, so the order
/// and repetition of `packages` do not matter. An existing closure is reused.
///
/// Every file in the tree is a hardlink to its blob in the form its entry
/// needs; symlinks and directories are created afresh. All packages are
/// merged before anything is written, and the merge is refused if two
/// packages give one path different entries, or if a path lies under
/// another package's file or symlink. The tree is built in a temporary
/// directory and renamed into place, so a closure that exists is complete.
/// `closure_dir` must be on the same mount as `artifact_dir`.
pub fn closure(packages: &[PackageId], artifact_dir: &Path, closure_dir: &Path) -> Result<Closed> {
    closure_as(packages, artifact_dir, closure_dir, UsrFiles::Hardlink)
}

/// Compose a closure as [`closure`] does, but copy every file from its blob
/// instead of hardlinking it, so that `closure_dir` can be on any mount. See
/// [`UsrFiles`] for what that gives up. A closure is named by its packages
/// alone, so one that exists is reused however it was built.
pub fn closure_copying(
    packages: &[PackageId],
    artifact_dir: &Path,
    closure_dir: &Path,
) -> Result<Closed> {
    closure_as(packages, artifact_dir, closure_dir, UsrFiles::Copy)
}

fn closure_as(
    packages: &[PackageId],
    artifact_dir: &Path,
    closure_dir: &Path,
    files: UsrFiles,
) -> Result<Closed> {
    let layout = ArtifactDir::new(artifact_dir);
    let identities = package_identities(&layout, packages, artifact_dir)?;
    let manifest = render_closure_manifest(&identities);
    let identity = Identity::of(manifest.as_bytes());
    let tree_path = closure_dir.join(identity.hex());
    let manifest_path = closure_dir.join(format!("{}{MANIFEST_SUFFIX}", identity.hex()));
    let closed = |reused| Closed {
        identity: identity.to_string(),
        tree_path: tree_path.clone(),
        manifest_path: manifest_path.clone(),
        reused,
    };
    if tree_path.is_dir() {
        // Reusing a closure only reads, so that anyone who can read the store
        // can use the closures an administrator built.
        match fs::read(&manifest_path) {
            Ok(existing) if existing == manifest.as_bytes() => {}
            Ok(_) => bail!("{}: does not match its hash", manifest_path.display()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                write_manifest(closure_dir, &manifest_path, &manifest)?;
            }
            Err(err) => {
                return Err(err).with_context(|| format!("{}", manifest_path.display()));
            }
        }
        return Ok(closed(true));
    }

    let merged = merge(&layout, &identities)?;

    store::create_dirs(closure_dir)?;

    let tmp = tempfile::Builder::new()
        .prefix(".tmp-")
        .permissions(Permissions::from_mode(ROOT_MODE))
        .tempdir_in(closure_dir)
        .with_context(|| {
            format!(
                "{}: cannot create a temporary directory",
                closure_dir.display()
            )
        })?;
    // Directories and symlinks created in the tree inherit this; its files
    // have their blobs' descriptor, either way.
    access::protect_dir(tmp.path())?;
    for (path, (entry, _)) in &merged {
        build_entry(&layout, tmp.path(), path, entry, files, closure_dir)?;
    }
    // Last, so that a tree that can't be built leaves nothing behind.
    write_manifest(closure_dir, &manifest_path, &manifest)?;

    let built = tmp.keep();
    match fs::rename(&built, &tree_path) {
        Ok(()) => Ok(closed(false)),
        // Another run built the same closure first. Its tree is identical,
        // so this one is discarded.
        Err(_) if tree_path.is_dir() => {
            fs::remove_dir_all(&built).with_context(|| format!("{}", built.display()))?;
            Ok(closed(true))
        }
        Err(err) => Err(err).with_context(|| format!("{}", tree_path.display())),
    }
}

/// The identities of `packages`, sorted and without duplicates, as a closure
/// of them lists them.
pub(crate) fn package_identities(
    layout: &ArtifactDir,
    packages: &[PackageId],
    artifact_dir: &Path,
) -> Result<Vec<Identity>> {
    if packages.is_empty() {
        bail!("a closure needs at least one package");
    }
    let mut identities = Vec::with_capacity(packages.len());
    for id in packages {
        let Some(identity) = store::read_ref(&layout.ref_path(id))? else {
            bail!("{id}: no such package in {}", artifact_dir.display());
        };
        identities.push(identity);
    }
    identities.sort_by(|a, b| a.hex().cmp(b.hex()));
    identities.dedup();
    Ok(identities)
}

/// The manifest naming a closure of `identities`, which must be sorted and
/// without duplicates. Each identity is on its own line, so that the closure
/// manifest has exactly one spelling.
pub(crate) fn render_closure_manifest(identities: &[Identity]) -> String {
    let mut out = String::from("packages = [\n");
    for identity in identities {
        out.push_str(&format!("    \"{identity}\",\n"));
    }
    out.push_str("]\n");
    out
}

fn write_manifest(closure_dir: &Path, manifest_path: &Path, manifest: &str) -> Result<()> {
    if !store::write_new(closure_dir, manifest_path, manifest.as_bytes())? {
        bail!("{}: does not match its hash", manifest_path.display());
    }
    Ok(())
}

/// Every path in the packages `identities`, with its entry and the package
/// that provides it, checked for conflicts before anything is written.
pub(crate) fn merge(
    layout: &ArtifactDir,
    identities: &[Identity],
) -> Result<BTreeMap<String, (Entry, PackageId)>> {
    let mut merged: BTreeMap<String, (Entry, PackageId)> = BTreeMap::new();
    for identity in identities {
        let path = layout.manifest_path(identity);
        let bytes = fs::read(&path).with_context(|| format!("{}", path.display()))?;
        if Identity::of(&bytes) != *identity {
            bail!("{}: does not match its hash", path.display());
        }
        let manifest = parse_manifest(&bytes).with_context(|| format!("{}", path.display()))?;
        for (file, entry) in manifest.entries {
            match merged.get(&file) {
                Some((existing, owner)) if *existing != entry => bail!(
                    "{}: {owner} and {} provide different entries",
                    toml_string(&file),
                    manifest.id
                ),
                Some(_) => {}
                None => {
                    merged.insert(file, (entry, manifest.id.clone()));
                }
            }
        }
    }

    // Checked on the whole merge, so that the result does not depend on the
    // order of packages. It also means no symlink is ever followed when the
    // tree is built, because nothing is created beneath one.
    for (file, (_, owner)) in &merged {
        let mut ancestor = file.as_str();
        while let Some(slash) = ancestor.rfind('/') {
            ancestor = &ancestor[..slash];
            if let Some((entry, ancestor_owner)) = merged.get(ancestor)
                && *entry != Entry::Dir
            {
                bail!(
                    "{}: in {owner}, but {} is a file or symlink in {ancestor_owner}",
                    toml_string(file),
                    toml_string(ancestor)
                );
            }
        }
    }
    Ok(merged)
}

/// Place `entry` at `path` under `root`, a directory of the tree `dest_name`,
/// which an error names rather than a temporary directory.
pub(crate) fn build_entry(
    layout: &ArtifactDir,
    root: &Path,
    path: &str,
    entry: &Entry,
    files: UsrFiles,
    dest_name: &Path,
) -> Result<()> {
    let dest = root.join(path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("{}", parent.display()))?;
    }
    match entry {
        Entry::Blob { hex, exec } => {
            let blob = layout.blob_path(hex, *exec);
            match files {
                UsrFiles::Hardlink => link_blob(layout, &blob, &dest, dest_name, "--copy-files"),
                UsrFiles::Copy if store::copy_blob(&blob, &dest, hex, *exec)? => Ok(()),
                UsrFiles::Copy => bail!("{}: does not match its hash", blob.display()),
            }
        }
        Entry::Symlink(target) => {
            symlink(target, &dest).with_context(|| format!("{}", dest.display()))
        }
        Entry::Dir => fs::create_dir_all(&dest).with_context(|| format!("{}", dest.display())),
    }
}

/// Hardlink `blob`, a file of the artifact directory `layout`, to `dest`, in
/// the tree `dest_name`. The error for a link across mounts points to
/// `copy_option`, which avoids it.
pub(crate) fn link_blob(
    layout: &ArtifactDir,
    blob: &Path,
    dest: &Path,
    dest_name: &Path,
    copy_option: &str,
) -> Result<()> {
    match fs::hard_link(blob, dest) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::CrossesDevices => bail!(
            "{}: not on the same mount as the artifact directory {}; use {copy_option}",
            dest_name.display(),
            layout.root().display()
        ),
        Err(err) if err.kind() == io::ErrorKind::TooManyLinks => bail!(
            "{}: has reached the filesystem's limit on hard links",
            blob.display()
        ),
        Err(err) => Err(err).with_context(|| format!("{}", blob.display())),
    }
}
