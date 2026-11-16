//! Fetching a package from another artifact directory, or from a
//! repository served over HTTP.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::access::{Security, Stamp};
use crate::repo::Blobs;
use crate::store::{self, ArtifactDir};
use crate::{Entry, Identity, Manifest, PackageId, Resolved, Source, parse_manifest};

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
    fetch_from(id, &Source::from(source), artifact_dir)
}

/// Copy package `id` from `source` into `artifact_dir`, as [`fetch`] does.
/// From a repository served over HTTP, the package is the one its
/// catalog lists, checked against its key if it has one, and every file is
/// checked against its hash as it is read, before it is named in the store.
pub fn fetch_from(id: &PackageId, source: &Source, artifact_dir: &Path) -> Result<Fetched> {
    let mut fetched = fetch_as(&[Pick::new(id, source, None)], artifact_dir, false)?;
    Ok(fetched.remove(0))
}

/// Copy each package `packages` chose into `artifact_dir`, from the source
/// that holds it, as [`fetch_all`] does. Each is the package its resolution
/// chose: the manifest is the identity that chose it, not what the source
/// says now.
pub fn fetch_resolved(packages: &[Resolved], artifact_dir: &Path) -> Result<Vec<Fetched>> {
    let picks: Vec<Pick> = (packages.iter())
        .map(|package| Pick::new(&package.id, &package.source, Some(&package.identity)))
        .collect();
    fetch_as(&picks, artifact_dir, false)
}

/// Copy each of `ids` from `source` into `artifact_dir`, as [`fetch`] does,
/// together: every package's blobs, then every manifest, then every ref. It
/// syncs the store a few times in all, not a few times a package, so a set
/// of packages is much quicker to fetch this way than one by one. It checks
/// every package before it writes anything.
pub fn fetch_all(ids: &[PackageId], source: &Path, artifact_dir: &Path) -> Result<Vec<Fetched>> {
    let source = Source::from(source);
    let picks: Vec<Pick> = ids.iter().map(|id| Pick::new(id, &source, None)).collect();
    fetch_as(&picks, artifact_dir, false)
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
    fetch_replacing_from(id, &Source::from(source), artifact_dir)
}

/// Copy package `id` from `source` into `artifact_dir`, as [`fetch_from`]
/// does, but replace whatever is stored as `id` there already, as
/// [`fetch_replacing`] does.
pub fn fetch_replacing_from(
    id: &PackageId,
    source: &Source,
    artifact_dir: &Path,
) -> Result<Fetched> {
    let mut fetched = fetch_as(&[Pick::new(id, source, None)], artifact_dir, true)?;
    Ok(fetched.remove(0))
}

/// A package to fetch: where from, and its identity if it is known
/// already, rather than to be read from the source.
struct Pick<'a> {
    id: &'a PackageId,
    source: &'a Source,
    identity: Option<&'a Identity>,
}

impl<'a> Pick<'a> {
    fn new(id: &'a PackageId, source: &'a Source, identity: Option<&'a Identity>) -> Self {
        Self {
            id,
            source,
            identity,
        }
    }
}

/// A package checked in its source, to be copied.
struct Checked<'a> {
    id: PackageId,
    source: &'a Source,
    /// How the source writes its blobs.
    blobs: Blobs,
    identity: Identity,
    bytes: Vec<u8>,
    manifest: Manifest,
}

fn fetch_as(picks: &[Pick], artifact_dir: &Path, replace: bool) -> Result<Vec<Fetched>> {
    // Where each file is below a source's top.
    let from = ArtifactDir::new(Path::new(""));
    let to = ArtifactDir::new(artifact_dir);

    let mut packages = Vec::new();
    for pick in picks {
        packages.push(check(pick, &to, replace)?);
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
            let (source_blob, reader) =
                (package.source).open_blob(&from.blob_path(signature, false), package.blobs)?;
            let Some(copied) = store::copy_pending_from(
                reader,
                &source_blob,
                &blob,
                (signature.as_str(), false),
                Security::default(),
                unstamped,
            )?
            else {
                bail!("{source_blob}: does not match its hash");
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
            let (source_blob, reader) =
                (package.source).open_blob(&from.file_path(entry), package.blobs)?;
            let Some(copied) = store::copy_pending_from(
                reader,
                &source_blob,
                &blob,
                (hex.as_str(), *exec),
                security,
                unstamped,
            )?
            else {
                bail!("{source_blob}: does not match its hash");
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

/// Check the package `pick` names in its source: its identity, given or
/// read there, and a manifest that hashes to it, names the package and
/// parses. Unless `replace`, `artifact_dir` must not hold another package
/// under its ID.
fn check<'a>(pick: &Pick<'a>, to: &ArtifactDir, replace: bool) -> Result<Checked<'a>> {
    let (id, source) = (pick.id, pick.source);
    if let Source::Dir(dir) = source
        && !fs::metadata(dir)
            .with_context(|| format!("{}", dir.display()))?
            .is_dir()
    {
        bail!("{}: not a directory", dir.display());
    }
    let identity = match pick.identity {
        Some(identity) => identity.clone(),
        None => match source.identity(id)? {
            Some(identity) => identity,
            None => bail!("{id}: no such package in {source}"),
        },
    };
    // The ref write at the end still refuses to replace a ref that appears
    // after this check.
    if !replace
        && let Some(existing) =
            store::read_ref(&to.ref_path(id))?.filter(|existing| *existing != identity)
    {
        bail!("{id}: already present as {existing}, but {source} has {identity}");
    }

    let bytes = source.manifest(&identity)?;
    let place = source.manifest_place(&identity);
    let manifest = parse_manifest(&bytes).with_context(|| place.clone())?;
    if manifest.id != *id {
        bail!("{place}: names {}, not {id}", manifest.id);
    }
    Ok(Checked {
        id: id.clone(),
        source,
        blobs: source.blobs()?,
        identity,
        bytes,
        manifest,
    })
}
