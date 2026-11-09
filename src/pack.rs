//! Packing a directory into the artifact directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::access::{Security, Stamp};
use crate::derive;
use crate::manifest::toml_string;
use crate::security::{
    SIGNATURE_BYTES, SIGNATURE_SUFFIX, check_signature, is_elf, is_signature_path,
};
use crate::store::{self, ArtifactDir};
use crate::{Entry, Identity, Metadata, PackageId, SecurityDescriptor, render_manifest};

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
    /// Number of stored files that don't carry all of their security
    /// metadata, because this system isn't Peios and wouldn't let ppk give
    /// it. Their manifest records it.
    pub unstamped: usize,
    /// What deriving requirements and provides from the files skipped, such
    /// as a shared library without a soname, in payload order.
    pub warnings: Vec<String>,
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
/// A file named `<file>.peios.sig` is not packed as a file: it is the
/// signature of `<file>`, a regular file beside it, as PSPU 5.16 has it, and
/// becomes that file's `security.peios.sig` attribute wherever the file is
/// placed. It must be exactly a signature's 3310 bytes, starting with its
/// version, 1, and `<file>` must not be ELF, which carries its signature in
/// a section instead. Nothing else may have a name ending in `.peios.sig`.
///
/// The package's ELF objects add to what it requires and provides, as
/// [`pack_with`] describes.
///
/// A packed package is immutable: packing the same name, version and arch
/// again succeeds only if it produces the identical manifest. Blobs are
/// written first, then the manifest, then the ref, so a ref never names a
/// manifest that is missing or incomplete.
pub fn pack(id: &PackageId, input: &Path, artifact_dir: &Path) -> Result<Packed> {
    pack_with_options(id, &PackOptions::default(), input, artifact_dir)
}

/// Pack `input` into `artifact_dir` as `id`, as [`pack`] does, with
/// `metadata` in its manifest. The metadata must pass [`Metadata::check`],
/// and is part of the package's identity.
///
/// The requirements and provides that the files' ELF objects give join
/// `metadata`'s: each `DT_NEEDED` soname is required, and each `DT_SONAME`
/// provided, except that a soname the package provides isn't required. For
/// the libraries in [`SYMBOL_VERSION_FLOORS`](crate::SYMBOL_VERSION_FLOORS),
/// the highest symbol version needed is the requirement's floor, and a
/// provide is at the package's version. Each version node of a library in
/// [`SYMBOL_CAPABILITIES`](crate::SYMBOL_CAPABILITIES) is a capability,
/// `elfver(<soname>:<node>)`. Files under `src/`, signatures and symlinks
/// are not read.
///
/// Where `metadata` and the files both name a requirement or provide,
/// `metadata`'s wins, except for those floor libraries: a requirement on one
/// for anything but any version is refused, as is a provide of one at a
/// version other than the package's.
pub fn pack_with(
    id: &PackageId,
    metadata: &Metadata,
    input: &Path,
    artifact_dir: &Path,
) -> Result<Packed> {
    let options = PackOptions {
        metadata: metadata.clone(),
        ..PackOptions::default()
    };
    pack_with_options(id, &options, input, artifact_dir)
}

/// Pack `input` into `artifact_dir` as `id`, as [`pack`] does, but replace
/// whatever is packed as `id` already.
///
/// This breaks the rule that a packed version never changes, so it is for
/// development, when a version is rebuilt from changed source and nothing
/// else relies on what it was. Only the ref changes: the replaced manifest
/// and its blobs stay, as does anything composed from them.
pub fn pack_replacing(id: &PackageId, input: &Path, artifact_dir: &Path) -> Result<Packed> {
    let options = PackOptions {
        replace: true,
        ..PackOptions::default()
    };
    pack_with_options(id, &options, input, artifact_dir)
}

/// Pack `input` into `artifact_dir` as `id` with `metadata`, as
/// [`pack_with`] does, but replace whatever is packed as `id` already, as
/// [`pack_replacing`] does.
pub fn pack_replacing_with(
    id: &PackageId,
    metadata: &Metadata,
    input: &Path,
    artifact_dir: &Path,
) -> Result<Packed> {
    let options = PackOptions {
        metadata: metadata.clone(),
        replace: true,
        ..PackOptions::default()
    };
    pack_with_options(id, &options, input, artifact_dir)
}

/// What [`pack_with_options`] packs besides the files.
#[derive(Debug, Clone, Default)]
pub struct PackOptions {
    /// The manifest's metadata, as [`pack_with`] takes it.
    pub metadata: Metadata,
    /// Security descriptors for files and directories of the package, each
    /// replacing the one it would otherwise get, keyed by its path in the
    /// package, as the manifest lists it. A descriptor can't be given to a
    /// symlink, a signature or the package's root, and a directory given one
    /// is listed in the manifest even if it isn't empty.
    pub descriptors: BTreeMap<String, SecurityDescriptor>,
    /// Replace whatever is packed as the package already, as
    /// [`pack_replacing`] does.
    pub replace: bool,
}

/// Pack `input` into `artifact_dir` as `id`, as [`pack`] does, with what
/// `options` gives.
///
/// Each file with security metadata, a descriptor or a signature, is stored
/// as a file of its own carrying it, so that every link to it carries it
/// too. Where ppk can't give it that metadata, because this system isn't
/// Peios and doesn't let it, the file is stored without it, which
/// [`Packed::unstamped`] counts; its manifest still records it.
pub fn pack_with_options(
    id: &PackageId,
    options: &PackOptions,
    input: &Path,
    artifact_dir: &Path,
) -> Result<Packed> {
    let metadata = &options.metadata;
    metadata.check()?;
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
    let mut dirs = BTreeSet::new();
    walk(input, input, &mut found, &mut dirs)?;
    let signatures = take_signatures(&mut found)?;
    add_descriptor_dirs(&options.descriptors, &dirs, &mut found);
    found.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    check_descriptors(&options.descriptors, &found)?;

    // Hash everything before writing anything, so that a refused pack leaves
    // the artifact directory untouched.
    let derived = derive::derive(
        found.iter().filter_map(|(rel, item)| match item {
            Found::File { path, .. } => Some((rel.as_str(), path.as_path())),
            _ => None,
        }),
        id.parsed_version(),
    )
    .context("deriving requirements and provides")?;
    let warnings = derived.warnings.clone();
    let metadata = &derive::merge(metadata, derived)?;
    metadata.check()?;

    let mut entries = Vec::with_capacity(found.len());
    let mut sources = Vec::new();
    for (rel, item) in found {
        let descriptor = options.descriptors.get(&rel).cloned();
        let entry = match item {
            Found::File { path, exec } => {
                let signature = signatures.get(&rel);
                let entry = Entry::Blob {
                    hex: hash_file(&path)?,
                    exec,
                    descriptor,
                    signature: signature.map(|signature| signature.hex.clone()),
                };
                sources.push((entry.clone(), path, signature));
                entry
            }
            Found::Symlink(target) => Entry::Symlink(target),
            Found::Dir => Entry::Dir { descriptor },
        };
        entries.push((rel, entry));
    }

    let manifest = render_manifest(id, metadata, &entries);
    let identity = Identity::of(manifest.as_bytes());

    let layout = ArtifactDir::new(artifact_dir);
    let ref_dir = layout.ref_dir(id);
    let ref_path = layout.ref_path(id);
    // The ref write at the end still refuses to replace a ref that appears
    // after this check.
    if !options.replace
        && let Some(existing) = store::read_ref(&ref_path)?.filter(|existing| *existing != identity)
    {
        bail!("{id}: already packed as {existing}; a packed version cannot be replaced");
    }

    let blob_dir = layout.blob_dir();
    store::create_dirs(&blob_dir)?;
    let mut blobs_written = 0;
    let mut unstamped = 0;
    // Signatures first, so that a stored file's signature is in the store
    // before the file is.
    for signature in signatures.values() {
        let blob = layout.blob_path(&signature.hex, false);
        if !blob.exists() {
            if !store::write_new(&blob_dir, &blob, &signature.bytes)? {
                bail!("{}: does not match its hash", blob.display());
            }
            blobs_written += 1;
        }
    }
    for (entry, path, signature) in &sources {
        let Entry::Blob {
            hex,
            exec,
            descriptor,
            ..
        } = entry
        else {
            unreachable!("only files have sources");
        };
        let security = Security {
            descriptor: descriptor.as_ref(),
            signature: signature.map(|signature| signature.bytes.as_slice()),
        };
        let blob = layout.file_path(entry);
        if !blob.exists() {
            if !store::copy_blob(path, &blob, (hex.as_str(), *exec), security, &mut unstamped)? {
                bail!("{}: file changed while being packed", path.display());
            }
            blobs_written += 1;
        } else if security.ensure(&blob)? == Stamp::Skipped {
            unstamped += 1;
        }
    }

    let manifest_dir = layout.manifest_dir();
    store::create_dirs(&manifest_dir)?;
    let manifest_path = layout.manifest_path(&identity);
    if !store::write_new(&manifest_dir, &manifest_path, manifest.as_bytes())? {
        bail!("{}: does not match its hash", manifest_path.display());
    }

    store::create_dirs(&ref_dir)?;
    if options.replace {
        store::write_replacing(
            &ref_dir,
            &ref_path,
            store::ref_contents(&identity).as_bytes(),
        )?;
    } else if !store::write_new(
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
        unstamped,
        warnings,
    })
}

/// A file's signature, read from the file beside it.
struct Signature {
    bytes: Vec<u8>,
    /// The hex SHA-256 of `bytes`, which name its blob.
    hex: String,
}

/// Take the signatures out of `found`, each checked, keyed by the path of
/// the file it signs. Whatever else has a signature's name is refused.
fn take_signatures(found: &mut Vec<(String, Found)>) -> Result<BTreeMap<String, Signature>> {
    let (sidecars, rest): (Vec<_>, Vec<_>) = std::mem::take(found)
        .into_iter()
        .partition(|(rel, _)| rel.ends_with(SIGNATURE_SUFFIX));
    *found = rest;
    let files: BTreeMap<&str, &Path> = found
        .iter()
        .filter_map(|(rel, item)| match item {
            Found::File { path, .. } => Some((rel.as_str(), path.as_path())),
            _ => None,
        })
        .collect();

    let mut signatures = BTreeMap::new();
    for (rel, item) in sidecars {
        let quoted = toml_string(&rel);
        let Found::File { path, .. } = item else {
            bail!("{quoted}: a name ending in {SIGNATURE_SUFFIX} is for a file's signature");
        };
        if !is_signature_path(&rel) {
            bail!("{quoted}: a name ending in {SIGNATURE_SUFFIX} is for a file's signature");
        }
        let target = &rel[..rel.len() - SIGNATURE_SUFFIX.len()];
        let Some(target_path) = files.get(target) else {
            bail!(
                "{quoted}: signs {}, which is not a regular file of the package",
                toml_string(target)
            );
        };
        let bytes = read_limited(&path, SIGNATURE_BYTES + 1)?;
        check_signature(&bytes).with_context(|| quoted.clone())?;
        if is_elf(&read_limited(target_path, 4)?) {
            bail!(
                "{quoted}: signs {}, an ELF file, which carries its signature in its \
                 .peios.sig section",
                toml_string(target)
            );
        }
        let hex = Identity::of(&bytes).hex().to_owned();
        signatures.insert(target.to_owned(), Signature { bytes, hex });
    }
    Ok(signatures)
}

/// Add each directory in `descriptors` that is in the input, `dirs`, and not
/// yet in `found`, because it isn't empty. A directory with a descriptor is
/// listed even then.
fn add_descriptor_dirs(
    descriptors: &BTreeMap<String, SecurityDescriptor>,
    dirs: &BTreeSet<String>,
    found: &mut Vec<(String, Found)>,
) {
    let listed: BTreeSet<String> = found.iter().map(|(rel, _)| rel.clone()).collect();
    for path in descriptors.keys() {
        if dirs.contains(path) && !listed.contains(path) {
            found.push((path.clone(), Found::Dir));
        }
    }
}

/// Refuse a descriptor for anything but a file or directory in `found`.
fn check_descriptors(
    descriptors: &BTreeMap<String, SecurityDescriptor>,
    found: &[(String, Found)],
) -> Result<()> {
    for path in descriptors.keys() {
        let quoted = toml_string(path);
        match found.binary_search_by(|(rel, _)| rel.as_bytes().cmp(path.as_bytes())) {
            Ok(i) if matches!(found[i].1, Found::Symlink(_)) => {
                bail!("{quoted}: a symlink has no security descriptor of its own")
            }
            Ok(_) => {}
            Err(_) => bail!("{quoted}: given a security descriptor, but not in the package"),
        }
    }
    Ok(())
}

/// Up to `limit` bytes from the start of `path`.
fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("{}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(limit as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("{}", path.display()))?;
    Ok(bytes)
}

/// Collect every regular file, symlink and empty directory under `dir`, keyed
/// by its path relative to `root`, and every directory's path in `dirs`.
/// Symlinks are recorded, never followed.
fn walk(
    root: &Path,
    dir: &Path,
    found: &mut Vec<(String, Found)>,
    dirs: &mut BTreeSet<String>,
) -> Result<()> {
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
            walk(root, &path, found, dirs)?;
            let rel = relative(root, &path)?;
            if found.len() == before {
                found.push((rel.clone(), Found::Dir));
            }
            dirs.insert(rel);
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
