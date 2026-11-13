//! Fetching a package from another artifact directory.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::access::{Security, Stamp};
use crate::store::{self, ArtifactDir};
use crate::{Entry, Identity, Manifest, PackageId, parse_manifest};

/// The result of a successful fetch.
#[derive(Debug)]
pub struct Fetched {
    /// The package's identity: `sha256:<hex>` of the manifest's bytes.
    pub identity: String,
    /// Where the manifest was written.
    pub manifest_path: PathBuf,
    /// Where the ref naming the manifest was written.
    pub ref_path: PathBuf,
    /// Number of blobs copied; blobs already present are not counted.
    pub blobs_written: usize,
    /// Number of stored files that don't carry all of their security
    /// metadata, because this system isn't Peios and wouldn't let ppk give
    /// it. Their manifest records it.
    pub unstamped: usize,
}

/// Copy package `id` from the artifact directory `source` into
/// `artifact_dir`.
///
/// Nothing read from `source` is trusted. The manifest must hash to the
/// identity in the source's ref, be in canonical form, name `id`, and contain
/// only safe paths; every blob copied must hash to the name the manifest
/// gives it. Blobs already in `artifact_dir` are not copied. Nothing is taken
/// from a stored file but its content: it is given the security metadata its
/// entry records, as [`pack_with_options`](crate::pack_with_options) gives
/// it, and a file already in `artifact_dir` with the wrong metadata is an
/// error.
///
/// If `artifact_dir` already has a different package under `id`, the fetch is
/// refused before anything is written. Otherwise blobs are written first,
/// then the manifest, then the ref, as `pack` does. A blob that fails its
/// check stops the fetch after earlier blobs were copied; they are named by
/// their content, so they are correct and harmless.
pub fn fetch(id: &PackageId, source: &Path, artifact_dir: &Path) -> Result<Fetched> {
    let mut fetched = fetch_as(std::slice::from_ref(id), source, artifact_dir, false)?;
    Ok(fetched.remove(0))
}

/// Copy each of `ids` from `source` into `artifact_dir`, as [`fetch`] does,
/// together: every package's blobs, then every manifest, then every ref. It
/// syncs the store a few times in all, not a few times a package, so a set
/// of packages is much quicker to fetch this way than one by one. It checks
/// every package before it writes anything.
pub fn fetch_all(ids: &[PackageId], source: &Path, artifact_dir: &Path) -> Result<Vec<Fetched>> {
    fetch_as(ids, source, artifact_dir, false)
}

/// Copy package `id` from `source` into `artifact_dir`, as [`fetch`] does,
/// but replace whatever is stored as `id` there already.
///
/// This breaks the rule that a stored version never changes, as
/// [`pack_replacing`](crate::pack_replacing) does, so it is for an artifact
/// directory nothing else relies on yet, such as a bootstrap's, whose
/// packages are rebuilt at the same versions before it is published. Only
/// the ref changes: the replaced manifest and its blobs stay.
pub fn fetch_replacing(id: &PackageId, source: &Path, artifact_dir: &Path) -> Result<Fetched> {
    let mut fetched = fetch_as(std::slice::from_ref(id), source, artifact_dir, true)?;
    Ok(fetched.remove(0))
}

/// A package checked in the source, to be copied.
struct Source {
    id: PackageId,
    identity: Identity,
    bytes: Vec<u8>,
    manifest: Manifest,
}

fn fetch_as(
    ids: &[PackageId],
    source: &Path,
    artifact_dir: &Path,
    replace: bool,
) -> Result<Vec<Fetched>> {
    if !fs::metadata(source)
        .with_context(|| format!("{}", source.display()))?
        .is_dir()
    {
        bail!("{}: not a directory", source.display());
    }
    let from = ArtifactDir::new(source);
    let to = ArtifactDir::new(artifact_dir);

    let mut packages = Vec::new();
    for id in ids {
        packages.push(check(id, &from, &to, source, replace)?);
    }

    let blob_dir = to.blob_dir();
    store::create_dirs(&blob_dir)?;
    let mut done = HashSet::new();
    let mut counts = vec![(0, 0); packages.len()];
    // Signatures first, so that a stored file's signature is in the store
    // before the file is, and its stamp can read it there. Everything is
    // named only once it's on disk, kind by kind; see `store::copy_pending`.
    let mut pending = Vec::new();
    for (package, (blobs_written, unstamped)) in packages.iter().zip(&mut counts) {
        for (_, entry) in &package.manifest.entries {
            let Entry::Blob {
                signature: Some(signature),
                ..
            } = entry
            else {
                continue;
            };
            let blob = to.blob_path(signature, false);
            if !done.insert(blob.clone()) || blob.exists() {
                continue;
            }
            let source_blob = from.blob_path(signature, false);
            let Some(copied) = store::copy_pending(
                &source_blob,
                &blob,
                (signature.as_str(), false),
                Security::default(),
                unstamped,
            )?
            else {
                bail!("{}: does not match its hash", source_blob.display());
            };
            pending.push(copied);
            *blobs_written += 1;
        }
    }
    store::place_pending(&blob_dir, std::mem::take(&mut pending))?;
    for (package, (blobs_written, unstamped)) in packages.iter().zip(&mut counts) {
        for (_, entry) in &package.manifest.entries {
            let Entry::Blob {
                hex,
                exec,
                descriptor,
                signature,
            } = entry
            else {
                continue;
            };
            let blob = to.file_path(entry);
            if !done.insert(blob.clone()) {
                continue;
            }
            let signature = signature
                .as_deref()
                .map(|signature| to.read_signature(signature))
                .transpose()?;
            let security = Security {
                descriptor: descriptor.as_ref(),
                signature: signature.as_deref(),
            };
            if blob.exists() {
                if entry.has_security() && security.ensure(&blob)? == Stamp::Skipped {
                    *unstamped += 1;
                }
                continue;
            }
            let source_blob = from.file_path(entry);
            let Some(copied) = store::copy_pending(
                &source_blob,
                &blob,
                (hex.as_str(), *exec),
                security,
                unstamped,
            )?
            else {
                bail!("{}: does not match its hash", source_blob.display());
            };
            pending.push(copied);
            *blobs_written += 1;
        }
    }
    store::place_pending(&blob_dir, std::mem::take(&mut pending))?;

    let manifest_dir = to.manifest_dir();
    store::create_dirs(&manifest_dir)?;
    for package in &packages {
        let manifest_path = to.manifest_path(&package.identity);
        pending.push(store::write_pending(
            &manifest_dir,
            &manifest_path,
            &package.bytes,
            false,
            || format!("{}: does not match its hash", manifest_path.display()),
        )?);
    }
    store::place_pending(&manifest_dir, std::mem::take(&mut pending))?;

    for package in &packages {
        let id = &package.id;
        let ref_dir = to.ref_dir(id);
        store::create_dirs(&ref_dir)?;
        pending.push(store::write_pending(
            &ref_dir,
            &to.ref_path(id),
            store::ref_contents(&package.identity).as_bytes(),
            replace,
            || format!("{id}: another package was stored under this name during the fetch"),
        )?);
    }
    store::place_pending(artifact_dir, pending)?;
    // The refs' names, which nothing syncs once they're placed.
    store::sync_filesystem(artifact_dir)?;

    Ok(packages
        .into_iter()
        .zip(counts)
        .map(|(package, (blobs_written, unstamped))| Fetched {
            identity: package.identity.to_string(),
            manifest_path: to.manifest_path(&package.identity),
            ref_path: to.ref_path(&package.id),
            blobs_written,
            unstamped,
        })
        .collect())
}

/// Check package `id` in the source: its ref, and a manifest that hashes to
/// it, names `id` and parses. Unless `replace`, `artifact_dir` must not hold
/// another package as `id`.
fn check(
    id: &PackageId,
    from: &ArtifactDir,
    to: &ArtifactDir,
    source: &Path,
    replace: bool,
) -> Result<Source> {
    let Some(identity) = store::read_ref(&from.ref_path(id))? else {
        bail!("{id}: no such package in {}", source.display());
    };
    // The ref write at the end still refuses to replace a ref that appears
    // after this check.
    if !replace
        && let Some(existing) =
            store::read_ref(&to.ref_path(id))?.filter(|existing| *existing != identity)
    {
        bail!(
            "{id}: already present as {existing}, but {} has {identity}",
            source.display()
        );
    }

    let source_manifest = from.manifest_path(&identity);
    let bytes =
        fs::read(&source_manifest).with_context(|| format!("{}", source_manifest.display()))?;
    if Identity::of(&bytes) != identity {
        bail!("{}: does not match its hash", source_manifest.display());
    }
    let manifest =
        parse_manifest(&bytes).with_context(|| format!("{}", source_manifest.display()))?;
    if manifest.id != *id {
        bail!(
            "{}: names {}, not {id}",
            source_manifest.display(),
            manifest.id
        );
    }
    Ok(Source {
        id: id.clone(),
        identity,
        bytes,
        manifest,
    })
}
