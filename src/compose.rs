//! Composed environments: an environment's whole root laid out in a
//! directory of its own, for something such as an image builder to take.
//!
//! The packages' closure becomes the root's `usr`, as hardlinks to their
//! blobs or copies of them. What lies outside `/usr` belongs to the
//! environment, not to any package, so it is given separately: symlinks,
//! empty directories, and optionally a store holding the packages themselves.
//! The one exception is a link a package claims there, such as `/init`,
//! which the root holds because the package is in it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, Permissions};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};

use crate::access;
use crate::closure::{
    UsrFiles, build_entry, link_blob, merge, package_identities, render_closure_manifest,
};
use crate::environment::MOUNT_POINT;
use crate::fetch::fetch;
use crate::inventory::Inventory;
use crate::manifest::{check_path, toml_string};
use crate::store::ArtifactDir;
use crate::{DEFAULT_ARTIFACT_DIR, Entry, Identity, PackageId};

/// The mode of a composed root, so that anyone can read it; the temporary
/// directory it is built in would otherwise be private.
const ROOT_MODE: u32 = 0o755;

/// The result of a successful composition.
#[derive(Debug)]
pub struct Composed {
    /// The identity of the packages' closure, which is the root's `usr`.
    pub identity: String,
    /// The composed root.
    pub path: PathBuf,
    /// Number of files and directories of `usr` that don't carry all of
    /// their security metadata, because this system isn't Peios and wouldn't
    /// let ppk give it. Their manifests record it.
    pub unstamped: usize,
}

/// A symlink to create in a composed root, given as `<path>:<target>`.
///
/// The path is relative to the root. The target is written into the link
/// as given, unless it starts with a second `:`, which stands for `/usr`:
/// `init::sbin/prelude` links `init` to `usr/sbin/prelude`. That target is
/// relative, climbing back to the root from a link in a subdirectory, so
/// the link resolves both in the composed directory and once it is `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvSymlink {
    path: String,
    target: String,
}

impl EnvSymlink {
    /// Where the link goes, relative to the root.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the link holds.
    pub fn target(&self) -> &str {
        &self.target
    }
}

impl FromStr for EnvSymlink {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let Some((path, target)) = s.split_once(':') else {
            bail!("{s}: not <path>:<target>");
        };
        check_env_path(path)?;
        let target = match target.strip_prefix(':') {
            Some(in_usr) if !in_usr.is_empty() => {
                let climb = "../".repeat(path.matches('/').count());
                format!("{climb}{}/{in_usr}", usr())
            }
            Some(_) => bail!("{s}: not <path>:<target>"),
            None => target.to_owned(),
        };
        if target.is_empty() {
            bail!("{s}: not <path>:<target>");
        }
        Ok(Self {
            path: path.to_owned(),
            target,
        })
    }
}

/// An empty directory to create in a composed root, relative to the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvDir(String);

impl EnvDir {
    pub fn path(&self) -> &str {
        &self.0
    }
}

impl FromStr for EnvDir {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        check_env_path(s)?;
        Ok(Self(s.to_owned()))
    }
}

/// Whether a composed root gets an artifact directory of its own holding the
/// packages, where ppk inside it looks by default, and how it is filled.
/// With one, the root's `usr` is built from it; without, from the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RootArtifacts {
    /// No artifact directory of its own.
    #[default]
    None,
    /// Hardlink the packages' refs, manifests and blobs from the source,
    /// which must be on the root's mount.
    Include,
    /// Copy them from the source as `fetch` does, checking every manifest
    /// and blob, so the source may be on any mount.
    Copy,
}

/// What a composed root holds besides its `usr`.
#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub symlinks: Vec<EnvSymlink>,
    pub dirs: Vec<EnvDir>,
    pub artifacts: RootArtifacts,
    /// How `usr` holds the packages' files.
    pub files: UsrFiles,
}

/// Compose `packages`, each already in `artifact_dir`, into a new root at
/// `out`: their closure as `usr`, the links they claim outside it, and
/// `extras` around it.
///
/// Nothing is written to the store; the closure is built in `out` itself.
/// Everything is checked before anything is written: the packages merge as
/// for a closure, a link claimed outside `usr` is not also an extra, nothing
/// lies under an extra symlink or a claimed link, and `out` must not exist.
/// The root is built in a temporary directory beside `out` and renamed into
/// place, so `out` is complete if it exists. `out` must be on
/// the same mount as `artifact_dir` wherever a file is hardlinked from it:
/// unless `extras` copies the packages into the root, or copies `usr`'s files
/// with no artifact directory in the root.
pub fn compose_env(
    packages: &[PackageId],
    artifact_dir: &Path,
    out: &Path,
    extras: &Extras,
) -> Result<Composed> {
    let layout = ArtifactDir::new(artifact_dir);
    let identities = package_identities(&layout, packages, artifact_dir)?;
    let identity = Identity::of(render_closure_manifest(&identities).as_bytes());
    let merged = merge(&layout, &identities)?;
    check_extras(extras, &merged.root_claims)?;
    if out.symlink_metadata().is_ok() {
        bail!("{}: already exists", out.display());
    }
    let parent = match out.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };

    let tmp = tempfile::Builder::new()
        .prefix(".tmp-")
        .permissions(Permissions::from_mode(ROOT_MODE))
        .tempdir_in(parent)
        .with_context(|| format!("{}: cannot create a temporary directory", parent.display()))?;
    // Directories and symlinks created in the root inherit this; its files
    // have their blobs' descriptor, either way.
    access::protect_dir(tmp.path())?;

    let root_store = tmp.path().join(artifacts());
    match extras.artifacts {
        RootArtifacts::None => {}
        RootArtifacts::Include => {
            include_artifacts(
                &layout,
                &root_store,
                packages,
                &identities,
                &merged.usr,
                out,
            )?;
        }
        RootArtifacts::Copy => {
            for id in packages {
                fetch(id, artifact_dir, &root_store)?;
            }
        }
    }
    let usr_layout = match extras.artifacts {
        RootArtifacts::None => layout,
        RootArtifacts::Include | RootArtifacts::Copy => ArtifactDir::new(&root_store),
    };
    let usr = tmp.path().join(usr());
    fs::create_dir(&usr).with_context(|| format!("{}", usr.display()))?;
    let mut unstamped = 0;
    for (path, (entry, _)) in &merged.usr {
        build_entry(
            &usr_layout,
            &usr,
            (path, entry),
            extras.files,
            out,
            &mut unstamped,
        )?;
    }
    Inventory::new(merged.packages).write(&usr)?;
    for dir in &extras.dirs {
        let dest = tmp.path().join(dir.path());
        fs::create_dir_all(&dest).with_context(|| format!("{}", dest.display()))?;
    }
    for link in &extras.symlinks {
        let dest = tmp.path().join(link.path());
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).with_context(|| format!("{}", parent.display()))?;
        }
        symlink(link.target(), &dest).with_context(|| format!("{}", dest.display()))?;
    }
    for (path, (target, _)) in &merged.root_claims {
        let dest = tmp.path().join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).with_context(|| format!("{}", parent.display()))?;
        }
        symlink(target, &dest).with_context(|| format!("{}", dest.display()))?;
    }

    fs::rename(tmp.path(), out).with_context(|| format!("{}", out.display()))?;
    // Renamed into place, so there is nothing left to clean up.
    let _ = tmp.keep();
    Ok(Composed {
        identity: identity.to_string(),
        path: out.to_owned(),
        unstamped,
    })
}

/// The root's `usr`: [`MOUNT_POINT`], relative to the root.
fn usr() -> &'static str {
    MOUNT_POINT.trim_start_matches('/')
}

/// The root's own artifact directory, where ppk inside it looks by default.
fn artifacts() -> &'static str {
    DEFAULT_ARTIFACT_DIR.trim_start_matches('/')
}

/// A path for an extra: safe, relative, and outside `usr`, which the
/// packages provide.
fn check_env_path(path: &str) -> Result<()> {
    check_path(path)?;
    let usr = usr();
    if path == usr || path.starts_with(&format!("{usr}/")) {
        bail!(
            "{}: inside {usr}, which the packages provide",
            toml_string(path)
        );
    }
    Ok(())
}

/// What lies at a path outside `usr`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outside<'a> {
    Symlink,
    Dir,
    /// A link claimed by this package.
    Claim(&'a PackageId),
}

/// Refuse extras and claimed links outside `usr` that clash, before anything
/// is written. Nothing may lie under a symlink or a claimed link, so creating
/// one never follows a link out of the root.
fn check_extras(extras: &Extras, claims: &BTreeMap<String, (String, PackageId)>) -> Result<()> {
    let mut paths: BTreeMap<&str, Outside> = BTreeMap::new();
    for link in &extras.symlinks {
        if paths.insert(link.path(), Outside::Symlink).is_some() {
            bail!("{}: given more than once", toml_string(link.path()));
        }
    }
    for dir in &extras.dirs {
        if paths.insert(dir.path(), Outside::Dir) == Some(Outside::Symlink) {
            bail!(
                "{}: given as both a directory and a symlink",
                toml_string(dir.path())
            );
        }
    }
    for (path, (_, claimant)) in claims {
        match paths.insert(path, Outside::Claim(claimant)) {
            Some(Outside::Symlink) => bail!(
                "{}: claimed by {claimant}, but the environment links it",
                toml_string(path)
            ),
            Some(Outside::Dir) => bail!(
                "{}: claimed by {claimant}, but the environment makes it a directory",
                toml_string(path)
            ),
            _ => {}
        }
    }

    let artifacts = artifacts();
    let mut checked: Vec<&str> = paths.keys().copied().collect();
    if extras.artifacts != RootArtifacts::None {
        for path in &checked {
            if path.starts_with(&format!("{artifacts}/"))
                || (*path == artifacts && paths[path] != Outside::Dir)
            {
                bail!(
                    "{}: inside {artifacts}, which holds the packages",
                    toml_string(path)
                );
            }
        }
        checked.push(artifacts);
    }
    for path in checked {
        let mut ancestor = path;
        while let Some(slash) = ancestor.rfind('/') {
            ancestor = &ancestor[..slash];
            match paths.get(ancestor) {
                Some(Outside::Symlink) => bail!(
                    "{}: under {}, which is a symlink",
                    toml_string(path),
                    toml_string(ancestor)
                ),
                Some(Outside::Claim(claimant)) => bail!(
                    "{}: under {}, which is a link claimed by {claimant}",
                    toml_string(path),
                    toml_string(ancestor)
                ),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Hardlink what ppk needs to find `packages` into the artifact directory
/// `dest`: their refs and manifests, each blob in the forms they use, and
/// their files' signatures. `out` is the root it is in, which an error names.
fn include_artifacts(
    layout: &ArtifactDir,
    dest: &Path,
    packages: &[PackageId],
    identities: &[Identity],
    merged: &BTreeMap<String, (Entry, PackageId)>,
    out: &Path,
) -> Result<()> {
    let dest_layout = ArtifactDir::new(dest);
    let mut links: BTreeSet<(PathBuf, PathBuf)> = BTreeSet::new();
    for id in packages {
        links.insert((layout.ref_path(id), dest_layout.ref_path(id)));
    }
    for identity in identities {
        links.insert((
            layout.manifest_path(identity),
            dest_layout.manifest_path(identity),
        ));
    }
    for (entry, _) in merged.values() {
        let Entry::Blob { signature, .. } = entry else {
            continue;
        };
        links.insert((layout.file_path(entry), dest_layout.file_path(entry)));
        if let Some(signature) = signature {
            links.insert((
                layout.blob_path(signature, false),
                dest_layout.blob_path(signature, false),
            ));
        }
    }
    for (from, to) in &links {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).with_context(|| format!("{}", parent.display()))?;
        }
        link_blob(layout, from, to, out, "--copy-artifacts")?;
    }
    Ok(())
}
