//! Repositories: an artifact directory that publishes what it holds. Its
//! catalog lists every version it has published, its indexes each hold one
//! version of each package, its channels follow indexes, and its pointer
//! file, the one file that changes and the one that is signed, says what
//! each of them is.
//!
//! ```text
//! repository           the pointer file
//! repository.sig       its signature
//! trees/sha256/<hex>   the catalog's and the indexes' tree nodes
//! refs/ manifests/ blobs/
//! ```
//!
//! Everything below the pointer file is named by its hash, so a static web
//! server can serve a repository as it is and a mirror can cache all of it
//! but the two files at the top.

mod key;
mod pointer;
mod tree;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub use key::{PrivateKey, PublicKey};
pub use pointer::{Channel, LEADING, Pointer, check_ref_name};
pub use tree::{Packages, Release, Standing, Trees};

use crate::store::{self, ArtifactDir};
use crate::{Entry, Identity, Metadata, PackageId, Version, parse_manifest, stored_packages};

/// The pointer file, at the repository's top.
const POINTER: &str = "repository";

/// Its signature, beside it.
const SIGNATURE: &str = "repository.sig";

/// A package left out of `leading`, because what it needs isn't there.
#[derive(Debug)]
pub struct LeftOut {
    pub id: PackageId,
    pub reason: String,
}

/// What publishing did.
#[derive(Debug)]
pub struct Published {
    pub pointer: Pointer,
    /// Packages `leading` would have held, but couldn't.
    pub left_out: Vec<LeftOut>,
}

/// The pointer file of the repository at `repo`, or `None` if it has none.
/// With `key`, its signature must be that key's.
pub fn read_pointer(repo: &Path, key: Option<&PublicKey>) -> Result<Option<Pointer>> {
    let path = repo.join(POINTER);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("{}", path.display())),
    };
    if let Some(key) = key {
        let signature_path = repo.join(SIGNATURE);
        let signature =
            fs::read(&signature_path).with_context(|| format!("{}", signature_path.display()))?;
        if !key.verifies(&bytes, &signature) {
            bail!("{}: not signed by the repository's key", path.display());
        }
    }
    let text = String::from_utf8(bytes).with_context(|| format!("{}", path.display()))?;
    Pointer::parse(&text)
        .map(Some)
        .with_context(|| format!("{}: malformed pointer file", path.display()))
}

/// Sign and write `pointer`. The signature is written first, so a reader
/// that finds the two don't match has read between them and can read again.
fn write_pointer(repo: &Path, pointer: &Pointer, key: &PrivateKey) -> Result<()> {
    pointer.check()?;
    let text = pointer.render();
    let signature = key.sign(text.as_bytes())?;
    store::write_replacing(repo, &repo.join(SIGNATURE), &signature)?;
    store::write_replacing(repo, &repo.join(POINTER), text.as_bytes())
}

/// The repository's pointer file, or a new one, sequence 0, if it has none.
fn current(repo: &Path) -> Result<Pointer> {
    Ok(read_pointer(repo, None)?.unwrap_or_default())
}

/// Publish what the repository at `repo` holds: its catalog, from its refs
/// and the standing the catalog gave each version, and `leading`, the newest
/// current version of each package for each architecture. A package whose
/// dependencies or requirements `leading` can't satisfy is left out of it,
/// and so is anything left needing it, or with `strict`, is an error. The
/// pointer file goes up one in sequence and is signed with `key`.
pub fn update(repo: &Path, key: &PrivateKey, strict: bool) -> Result<Published> {
    update_with(repo, key, strict, |_| Ok(()))
}

fn update_with(
    repo: &Path,
    key: &PrivateKey,
    strict: bool,
    change: impl FnOnce(&mut Packages) -> Result<()>,
) -> Result<Published> {
    let old = current(repo)?;
    let trees = Trees::new(repo);
    let mut catalog = catalog(repo, &trees, &old)?;
    change(&mut catalog)?;
    let (leading, left_out) = leading(repo, &catalog)?;
    if strict && let Some(first) = left_out.first() {
        bail!("{}: {}", first.id, first.reason);
    }
    let mut pointer = old.next();
    pointer.catalog = Some(trees.write_tree(&catalog)?);
    pointer
        .indexes
        .insert(LEADING.to_owned(), trees.write_tree(&leading)?);
    write_pointer(repo, &pointer, key)?;
    Ok(Published { pointer, left_out })
}

/// The catalog of what `repo` holds: every ref, with the standing the
/// catalog `old` names gave it, and every version it records as deleted.
fn catalog(repo: &Path, trees: &Trees, old: &Pointer) -> Result<Packages> {
    let previous = match &old.catalog {
        Some(top) => trees.read_tree(top)?,
        None => Packages::new(),
    };
    let standing = |id: &PackageId| {
        previous.get(id.name()).and_then(|releases| {
            (releases.iter())
                .find(|r| r.version == id.version() && r.arch == id.arch())
                .map(|r| r.standing.clone())
        })
    };
    let mut catalog = Packages::new();
    for (id, identity) in stored_packages(repo)? {
        let standing = match standing(&id) {
            Some(Standing::Yanked) => Standing::Yanked,
            _ => Standing::Current,
        };
        catalog
            .entry(id.name().to_owned())
            .or_default()
            .push(Release {
                version: id.version().to_owned(),
                arch: id.arch().to_owned(),
                identity,
                standing,
            });
    }
    for (name, releases) in previous {
        for release in releases {
            if matches!(release.standing, Standing::Deleted(_)) {
                let held = catalog.entry(name.clone()).or_default();
                if !held
                    .iter()
                    .any(|r| r.version == release.version && r.arch == release.arch)
                {
                    held.push(release);
                }
            }
        }
    }
    Ok(catalog)
}

/// `leading` for `catalog`: the newest current version of each package for
/// each architecture, less any whose needs it can't meet, and the packages
/// left out for that.
fn leading(repo: &Path, catalog: &Packages) -> Result<(Packages, Vec<LeftOut>)> {
    let layout = ArtifactDir::new(repo);
    // Each package chosen, by name and architecture, with its metadata.
    let mut chosen: BTreeMap<(String, String), (Release, Version, Metadata)> = BTreeMap::new();
    for (name, releases) in catalog {
        for release in releases.iter().filter(|r| r.standing == Standing::Current) {
            let version = Version::parse(&release.version)?;
            let key = (name.clone(), release.arch.clone());
            if chosen
                .get(&key)
                .is_some_and(|(_, held, _)| *held >= version)
            {
                continue;
            }
            let metadata = metadata(&layout, &release.identity)?;
            chosen.insert(key, (release.clone(), version, metadata));
        }
    }
    let mut left_out = Vec::new();
    loop {
        let unmet = chosen.iter().find_map(|((name, arch), (_, _, metadata))| {
            unmet(&chosen, arch, metadata).map(|reason| ((name.clone(), arch.clone()), reason))
        });
        let Some((key, reason)) = unmet else { break };
        let (release, _, _) = chosen.remove(&key).expect("found among those chosen");
        left_out.push(LeftOut {
            id: PackageId::new(&key.0, &release.version, &key.1)?,
            reason,
        });
    }
    let mut leading = Packages::new();
    for ((name, _), (release, _, _)) in chosen {
        leading.entry(name).or_default().push(release);
    }
    Ok((leading, left_out))
}

/// Why a package for `arch` with `metadata` can't be installed from
/// `chosen`, if it can't: the first dependency or requirement nothing there
/// satisfies.
fn unmet(
    chosen: &BTreeMap<(String, String), (Release, Version, Metadata)>,
    arch: &str,
    metadata: &Metadata,
) -> Option<String> {
    for (name, constraint) in &metadata.dependencies {
        let met = chosen
            .get(&(name.clone(), arch.to_owned()))
            .is_some_and(|(_, version, _)| constraint.matches(version));
        if !met {
            return Some(format!(
                "depends on {name} {constraint}, which leading lacks"
            ));
        }
    }
    for (capability, constraint) in &metadata.requires {
        let met = chosen
            .iter()
            .any(|((name, held_arch), (_, version, held))| {
                held_arch == arch
                    && match held.provides.get(capability) {
                        Some(Some(provided)) => constraint.matches(provided),
                        Some(None) => true,
                        None => name == capability && constraint.matches(version),
                    }
            });
        if !met {
            return Some(format!(
                "requires {capability} {constraint}, which leading lacks"
            ));
        }
    }
    None
}

fn metadata(layout: &ArtifactDir, identity: &Identity) -> Result<Metadata> {
    let path = layout.manifest_path(identity);
    let bytes = fs::read(&path).with_context(|| format!("{}", path.display()))?;
    if Identity::of(&bytes) != *identity {
        bail!("{}: does not match its hash", path.display());
    }
    Ok(parse_manifest(&bytes)
        .with_context(|| format!("{}", path.display()))?
        .metadata)
}

/// Make the index `name` a snapshot of the index or channel `from`. A
/// snapshot never changes, so `name` must be new.
pub fn snapshot(repo: &Path, key: &PrivateKey, name: &str, from: &str) -> Result<Pointer> {
    check_ref_name(name)?;
    let mut pointer = current(repo)?.next();
    if pointer.indexes.contains_key(name) || pointer.channels.contains_key(name) {
        bail!("{name}: already an index or channel");
    }
    let (_, top) = pointer.index_of(from)?;
    let top = top.clone();
    pointer.indexes.insert(name.to_owned(), top);
    write_pointer(repo, &pointer, key)?;
    Ok(pointer)
}

/// Make the channel `name` follow the index or channel `target`, and mark
/// it ended if `ended`.
pub fn channel(
    repo: &Path,
    key: &PrivateKey,
    name: &str,
    target: &str,
    ended: bool,
) -> Result<Pointer> {
    check_ref_name(name)?;
    let mut pointer = current(repo)?.next();
    if pointer.indexes.contains_key(name) {
        bail!("{name}: an index, not a channel");
    }
    pointer.channels.insert(
        name.to_owned(),
        Channel {
            target: target.to_owned(),
            ended,
        },
    );
    pointer.check()?;
    write_pointer(repo, &pointer, key)?;
    Ok(pointer)
}

/// Yank the version `id`, or with `undo`, unyank it, and publish again.
pub fn yank(repo: &Path, key: &PrivateKey, id: &PackageId, undo: bool) -> Result<Published> {
    update_with(repo, key, false, |catalog| {
        let release = find(catalog, id)?;
        release.standing = match (&release.standing, undo) {
            (Standing::Deleted(_), _) => bail!("{id}: deleted, not current"),
            (_, false) => Standing::Yanked,
            (_, true) => Standing::Current,
        };
        Ok(())
    })
}

/// Delete the version `id` for `reason`: its ref, its manifest, and every
/// file of it that no other manifest names, leaving a tombstone in the
/// catalog. Then publish again.
pub fn delete(repo: &Path, key: &PrivateKey, id: &PackageId, reason: &str) -> Result<Published> {
    if reason.is_empty() || reason.contains('\n') {
        bail!("a deletion's reason is one line, not empty");
    }
    let layout = ArtifactDir::new(repo);
    let Some(identity) = store::read_ref(&layout.ref_path(id))? else {
        bail!("{id}: no such package in {}", repo.display());
    };
    // Nothing is removed until the files to keep are known.
    let doomed = files_of(&layout, &identity)?;
    let kept = files_of_others(&layout, &identity)?;
    remove(&layout.ref_path(id))?;
    remove(&layout.manifest_path(&identity))?;
    for file in doomed.difference(&kept) {
        remove(file)?;
    }
    let reason = reason.to_owned();
    update_with(repo, key, false, move |catalog| {
        catalog
            .entry(id.name().to_owned())
            .or_default()
            .push(Release {
                version: id.version().to_owned(),
                arch: id.arch().to_owned(),
                identity,
                standing: Standing::Deleted(reason),
            });
        Ok(())
    })
}

fn find<'a>(catalog: &'a mut Packages, id: &PackageId) -> Result<&'a mut Release> {
    (catalog.get_mut(id.name()))
        .and_then(|releases| {
            (releases.iter_mut()).find(|r| r.version == id.version() && r.arch == id.arch())
        })
        .with_context(|| format!("{id}: not in the catalog"))
}

/// The files of the manifest `identity`: its blobs and their signatures.
fn files_of(layout: &ArtifactDir, identity: &Identity) -> Result<BTreeSet<PathBuf>> {
    let path = layout.manifest_path(identity);
    let bytes = fs::read(&path).with_context(|| format!("{}", path.display()))?;
    let manifest = parse_manifest(&bytes).with_context(|| format!("{}", path.display()))?;
    let mut files = BTreeSet::new();
    for (_, entry) in &manifest.entries {
        if let Entry::Blob { signature, .. } = entry {
            files.insert(layout.file_path(entry));
            if let Some(signature) = signature {
                files.insert(layout.blob_path(signature, false));
            }
        }
    }
    Ok(files)
}

/// The files every manifest but `identity` names.
fn files_of_others(layout: &ArtifactDir, identity: &Identity) -> Result<BTreeSet<PathBuf>> {
    let mut files = BTreeSet::new();
    let dir = layout.manifest_dir();
    for entry in fs::read_dir(&dir).with_context(|| format!("{}", dir.display()))? {
        let entry = entry.with_context(|| format!("{}", dir.display()))?;
        let Some(hex) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(other) = format!("{}:{hex}", crate::ALGORITHM).parse::<Identity>() else {
            continue;
        };
        if other != *identity {
            files.extend(files_of(layout, &other)?);
        }
    }
    Ok(files)
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("{}", path.display())),
    }
}
