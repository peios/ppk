//! Exporting a repository: a copy of it to serve, whose blobs are
//! compressed. The repository itself stays plain, since what is built
//! from it links its blobs, and the copy is written again from it after
//! each publication, compressing only the blobs it doesn't hold yet.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};

use super::{Blobs, Pointer, Signer, read_pointer, write_pointer};
use crate::store::{self, ArtifactDir};
use crate::{ALGORITHM, Identity, compression, stored_packages};

/// The level blobs are compressed at unless told otherwise.
pub const DEFAULT_LEVEL: i32 = 12;

/// The mode of everything an export writes: read-only, and readable by the
/// server that serves it.
const MODE: u32 = 0o444;

/// What exporting did.
#[derive(Debug)]
pub struct Exported {
    /// The pointer file the copy was published with.
    pub pointer: Pointer,
    /// How many blobs were compressed; those the copy held already aren't.
    pub compressed: usize,
    /// How many files were removed from the copy, because the repository no
    /// longer holds them.
    pub removed: usize,
}

/// Write `to` as a copy of the repository `repo`, as it was last
/// published, with its blobs compressed with zstd at `level`, and sign its
/// pointer file as `signer`, saying `blobs zstd`.
///
/// Tree nodes, manifests and refs are copied as they are, and each blob is
/// compressed under its name, once for all its forms, which are hard links
/// of one file. What `to` holds already is kept, so exporting again
/// compresses only the blobs added since. Everything copied is checked
/// against its hash first. The pointer file is written once everything it
/// names is there, with the sequence `repo`'s has; what `repo` no longer
/// holds is removed from `to` after that.
///
/// `to` must be new, empty or an earlier export, and not a later one than
/// `repo`'s publication.
pub fn export(repo: &Path, to: &Path, signer: &Signer, level: i32) -> Result<Exported> {
    let Some(published) = read_pointer(repo, None)? else {
        bail!(
            "{}: not published as a repository; run ppk repo update first",
            repo.display()
        );
    };
    if published.blobs != Blobs::Plain {
        bail!("{}: its blobs are compressed already", repo.display());
    }
    check_destination(repo, to, &published)?;

    let mut kept = BTreeSet::new();
    for kind in ["trees", "manifests"] {
        copy_named(repo, to, kind, &mut kept)?;
    }
    let compressed = compress_blobs(repo, to, level, &mut kept)?;
    copy_refs(repo, to, &mut kept)?;
    store::sync_filesystem(to)?;

    let pointer = Pointer {
        signed: super::pointer::now(),
        blobs: Blobs::Zstd,
        ..published
    };
    write_pointer(to, &pointer, signer)?;
    let removed = remove_others(to, &kept)?;
    Ok(Exported {
        pointer,
        compressed,
        removed,
    })
}

/// Check that `to` can be written as an export of `repo`, whose pointer
/// file is `published`: it is new, empty or an earlier export.
fn check_destination(repo: &Path, to: &Path, published: &Pointer) -> Result<()> {
    let same = |a: &Path, b: &Path| matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b);
    if same(repo, to) {
        bail!("{}: can't export a repository over itself", to.display());
    }
    let empty = match fs::read_dir(to) {
        Ok(mut entries) => entries.next().is_none(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => true,
        Err(err) => return Err(err).with_context(|| format!("{}", to.display())),
    };
    if empty {
        return Ok(());
    }
    match read_pointer(to, None)? {
        Some(old) if old.blobs == Blobs::Zstd => {
            if old.sequence > published.sequence {
                bail!(
                    "{}: exported at sequence {}, later than {}, which {} has",
                    to.display(),
                    old.sequence,
                    published.sequence,
                    repo.display()
                );
            }
            Ok(())
        }
        _ => bail!(
            "{}: not an export of a repository; give a new or empty directory",
            to.display()
        ),
    }
}

/// The names of the files in `dir`, but temporary ones; none if it
/// doesn't exist.
fn names(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("{}", dir.display())),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("{}", dir.display()))?;
        if let Some(name) = entry.file_name().to_str()
            && !name.starts_with('.')
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Copy each file under `kind/sha256` of `repo` that `to` lacks, checked
/// against the hash that names it, recording each as kept.
fn copy_named(repo: &Path, to: &Path, kind: &str, kept: &mut BTreeSet<PathBuf>) -> Result<()> {
    let relative = Path::new(kind).join(ALGORITHM);
    let (from_dir, to_dir) = (repo.join(&relative), to.join(&relative));
    let names = names(&from_dir)?;
    if !names.is_empty() {
        fs::create_dir_all(&to_dir).with_context(|| format!("{}", to_dir.display()))?;
    }
    for name in names {
        kept.insert(relative.join(&name));
        let dest = to_dir.join(&name);
        if dest.exists() {
            continue;
        }
        let path = from_dir.join(&name);
        let bytes = fs::read(&path).with_context(|| format!("{}", path.display()))?;
        if Identity::of(&bytes).hex() != name {
            bail!("{}: does not match its hash", path.display());
        }
        write_new(&to_dir, &dest, |file| {
            file.write_all(&bytes).map_err(Into::into)
        })?;
    }
    Ok(())
}

/// Compress each blob of `repo` that `to` lacks, at `level`, on every CPU,
/// recording each as kept. Every form of a blob is a hard link of one
/// compressed file. Returns how many were compressed.
fn compress_blobs(
    repo: &Path,
    to: &Path,
    level: i32,
    kept: &mut BTreeSet<PathBuf>,
) -> Result<usize> {
    let from = ArtifactDir::new(repo).blob_dir();
    let dest = ArtifactDir::new(to).blob_dir();
    let relative = Path::new("blobs").join(ALGORITHM);
    // Each blob's forms, by the hash of its content.
    let mut forms: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in names(&from)? {
        let hex = name.split('.').next().expect("a name has a first part");
        forms.entry(hex.to_owned()).or_default().push(name);
    }
    if forms.is_empty() {
        return Ok(0);
    }
    fs::create_dir_all(&dest).with_context(|| format!("{}", dest.display()))?;
    let mut jobs = Vec::new();
    for (hex, names) in forms {
        kept.extend(names.iter().map(|name| relative.join(name)));
        let held = names.iter().find(|name| dest.join(name).exists()).cloned();
        match held {
            Some(held) => link_forms(&dest, &held, &names)?,
            None => jobs.push((hex, names)),
        }
    }

    let next = AtomicUsize::new(0);
    let failed = Mutex::new(None);
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(jobs.len());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((hex, names)) = jobs.get(i) else {
                        break;
                    };
                    if failed
                        .lock()
                        .expect("no worker panics holding it")
                        .is_some()
                    {
                        break;
                    }
                    if let Err(err) = compress_one(&from, &dest, hex, names, level) {
                        failed
                            .lock()
                            .expect("no worker panics holding it")
                            .get_or_insert(err);
                        break;
                    }
                }
            });
        }
    });
    if let Some(err) = failed.into_inner().expect("no worker panicked holding it") {
        return Err(err);
    }
    Ok(jobs.len())
}

/// Compress the blob `hex` of `from`, whose forms are `names`, into `dest`
/// under each of their names, checking its content against its hash.
fn compress_one(from: &Path, dest: &Path, hex: &str, names: &[String], level: i32) -> Result<()> {
    let path = from.join(&names[0]);
    let file = File::open(&path).with_context(|| format!("{}", path.display()))?;
    let first = dest.join(&names[0]);
    write_new(dest, &first, |out| {
        let identity = compression::compress(file, out, level)
            .with_context(|| format!("{}", path.display()))?;
        if identity.hex() != hex {
            bail!("{}: does not match its hash", path.display());
        }
        Ok(())
    })?;
    link_forms(dest, &names[0], names)
}

/// Make each of `names` in `dir` that is missing a hard link of `held`.
fn link_forms(dir: &Path, held: &str, names: &[String]) -> Result<()> {
    for name in names {
        let path = dir.join(name);
        match fs::hard_link(dir.join(held), &path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err).with_context(|| format!("{}", path.display())),
        }
    }
    Ok(())
}

/// Copy every ref of `repo` to `to`, replacing one that differs, recording
/// each as kept.
fn copy_refs(repo: &Path, to: &Path, kept: &mut BTreeSet<PathBuf>) -> Result<()> {
    let layout = ArtifactDir::new(Path::new(""));
    for (id, identity) in stored_packages(repo)? {
        let relative = layout.ref_path(&id);
        kept.insert(relative.clone());
        let dest = to.join(&relative);
        let contents = store::ref_contents(&identity);
        if fs::read(&dest).is_ok_and(|held| held == contents.as_bytes()) {
            continue;
        }
        let dir = dest.parent().expect("a ref is in a directory");
        fs::create_dir_all(dir).with_context(|| format!("{}", dir.display()))?;
        write_file(dir, &dest, true, |file| {
            file.write_all(contents.as_bytes()).map_err(Into::into)
        })?;
    }
    Ok(())
}

/// Remove every file below `trees`, `manifests`, `blobs` and `refs` of `to`
/// that isn't `kept`, and every directory of refs left empty. Returns how
/// many files were removed.
fn remove_others(to: &Path, kept: &BTreeSet<PathBuf>) -> Result<usize> {
    let mut removed = 0;
    for kind in ["trees", "manifests", "blobs"] {
        let relative = Path::new(kind).join(ALGORITHM);
        for name in names(&to.join(&relative))? {
            if !kept.contains(&relative.join(&name)) {
                remove(&to.join(&relative).join(name))?;
                removed += 1;
            }
        }
    }
    let refs = Path::new("refs");
    for name in names(&to.join(refs))? {
        for version in names(&to.join(refs).join(&name))? {
            let dir = refs.join(&name).join(&version);
            for arch in names(&to.join(&dir))? {
                if !kept.contains(&dir.join(&arch)) {
                    remove(&to.join(&dir).join(arch))?;
                    removed += 1;
                }
            }
            // Only an empty directory is removed.
            let _ = fs::remove_dir(to.join(&dir));
        }
        let _ = fs::remove_dir(to.join(refs).join(&name));
    }
    Ok(removed)
}

fn remove(path: &Path) -> Result<()> {
    fs::remove_file(path).with_context(|| format!("{}", path.display()))
}

/// Write `dest`, which must not exist, through a temporary file in `dir`
/// that `write` fills.
fn write_new(dir: &Path, dest: &Path, write: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    write_file(dir, dest, false, write)
}

/// Write `dest` through a temporary file in `dir` that `write` fills,
/// replacing what it holds if `replace`, and otherwise keeping it.
fn write_file(
    dir: &Path,
    dest: &Path,
    replace: bool,
    write: impl FnOnce(&mut File) -> Result<()>,
) -> Result<()> {
    let mut tmp = tempfile::Builder::new()
        .prefix(".tmp-")
        .permissions(fs::Permissions::from_mode(MODE))
        .tempfile_in(dir)
        .with_context(|| format!("{}: cannot create a temporary file", dir.display()))?;
    write(tmp.as_file_mut())?;
    let placed = match replace {
        true => tmp.persist(dest).map(drop).map_err(|err| err.error),
        false => tmp
            .persist_noclobber(dest)
            .map(drop)
            .map_err(|err| err.error),
    };
    match placed {
        Ok(()) => Ok(()),
        Err(err) if !replace && err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err).with_context(|| format!("{}", dest.display())),
    }
}
