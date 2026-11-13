//! The repositories a machine fetches from: each one a file of its own in
//! the configuration directory, `/conf/ppk/repositories/<name>.toml` unless
//! told otherwise, saying where it is, the public key its pointer file must
//! be signed with, if any, and the index or channel the machine follows in it.
//!
//! ```toml
//! path = "/mnt/peios-repo"
//! key = "ml-dsa-65:…"
//! track = "stable"
//! ```
//!
//! `/conf` merges what the system ships with what the machine adds, which
//! is written under `/lcl/conf/ppk`. What the machine has seen of each
//! repository, the highest sequence, is state, not configuration: it is kept
//! in the state directory, `/var/state/ppk` unless told otherwise.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::repo::{self, Pointer, PublicKey, check_ref_name};

/// Where the repositories are read from unless told otherwise.
pub const DEFAULT_CONFIG_DIR: &str = "/conf/ppk";

/// Where a machine's own repositories are written unless told otherwise.
pub const DEFAULT_LOCAL_CONFIG_DIR: &str = "/lcl/conf/ppk";

/// Where what the machine has seen is kept unless told otherwise.
pub const DEFAULT_STATE_DIR: &str = "/var/state/ppk";

/// The keys a repository's file may have.
const KEYS: [&str; 3] = ["path", "key", "track"];

/// A repository the machine fetches from.
#[derive(Clone, Debug)]
pub struct Repository {
    /// Its name: its file's, without `.toml`.
    pub name: String,
    /// Where it is.
    pub path: PathBuf,
    /// The key its pointer file must be signed with, if it must be.
    pub key: Option<PublicKey>,
    /// The index or channel a request for a bare name follows.
    pub track: Option<String>,
}

impl Repository {
    fn render(&self) -> String {
        let quote = |s: &str| toml::Value::String(s.to_owned()).to_string();
        let mut out = format!("path = {}\n", quote(&self.path.display().to_string()));
        if let Some(key) = &self.key {
            out.push_str(&format!("key = {}\n", quote(&key.to_string())));
        }
        if let Some(track) = &self.track {
            out.push_str(&format!("track = {}\n", quote(track)));
        }
        out
    }

    fn parse(name: &str, text: &str) -> Result<Self> {
        let doc: toml::Table =
            toml::from_str(text).map_err(|err| anyhow!("not valid TOML: {}", err.message()))?;
        if let Some(key) = doc.keys().find(|key| !KEYS.contains(&key.as_str())) {
            bail!("unknown key {key:?}");
        }
        let string = |key: &str| -> Result<Option<String>> {
            match doc.get(key) {
                None => Ok(None),
                Some(value) => (value.as_str().map(str::to_owned))
                    .map(Some)
                    .ok_or_else(|| anyhow!("{key} is not a string")),
            }
        };
        let path = string("path")?.ok_or_else(|| anyhow!("no path"))?;
        let track = string("track")?;
        if let Some(track) = &track {
            check_ref_name(track)?;
        }
        Ok(Self {
            name: name.to_owned(),
            path: PathBuf::from(path),
            key: string("key")?.map(|key| key.parse()).transpose()?,
            track,
        })
    }

    /// The repository's pointer file, checked against its key if it has one,
    /// and against the highest sequence the machine has seen of it, which
    /// it then records in `state_dir`. `None` if it hasn't published one.
    pub fn pointer(&self, state_dir: &Path) -> Result<Option<Pointer>> {
        let Some(pointer) = repo::read_pointer(&self.path, self.key.as_ref())
            .with_context(|| format!("repository {}", self.name))?
        else {
            return Ok(None);
        };
        let seen_path = state_dir.join("sequences").join(&self.name);
        let seen = match fs::read_to_string(&seen_path) {
            Ok(text) => text
                .trim()
                .parse::<u64>()
                .with_context(|| format!("{}", seen_path.display()))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
            Err(err) => return Err(err).with_context(|| format!("{}", seen_path.display())),
        };
        if pointer.sequence < seen {
            bail!(
                "repository {}: offers sequence {}, older than {seen}, which this machine has \
                 seen already",
                self.name,
                pointer.sequence
            );
        }
        if pointer.sequence > seen {
            let dir = seen_path.parent().expect("a sequence is in a directory");
            fs::create_dir_all(dir).with_context(|| format!("{}", dir.display()))?;
            write_atomically(&seen_path, format!("{}\n", pointer.sequence).as_bytes())?;
        }
        Ok(Some(pointer))
    }
}

/// Check that `name` can name a repository: as an index or channel can be
/// named.
fn check_name(name: &str) -> Result<()> {
    check_ref_name(name).map_err(|_| {
        anyhow!(
            "invalid repository name '{name}': use 1 to 64 ASCII letters, digits, - . and _, \
             not starting with a dot"
        )
    })
}

/// Every repository configured in `dir`, sorted by name.
pub fn repositories(dir: &Path) -> Result<Vec<Repository>> {
    let dir = dir.join("repositories");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("{}", dir.display())),
    };
    let mut found = BTreeMap::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("{}", dir.display()))?;
        let file = entry.file_name();
        let Some(name) = file.to_str().and_then(|file| file.strip_suffix(".toml")) else {
            continue;
        };
        let path = entry.path();
        check_name(name).with_context(|| format!("{}", path.display()))?;
        let text = fs::read_to_string(&path).with_context(|| format!("{}", path.display()))?;
        let repository =
            Repository::parse(name, &text).with_context(|| format!("{}", path.display()))?;
        found.insert(name.to_owned(), repository);
    }
    Ok(found.into_values().collect())
}

/// Add `repository` to `dir`, replacing one of its name.
pub fn add_repository(dir: &Path, repository: &Repository) -> Result<()> {
    check_name(&repository.name)?;
    let dir = dir.join("repositories");
    fs::create_dir_all(&dir).with_context(|| format!("{}", dir.display()))?;
    let path = dir.join(format!("{}.toml", repository.name));
    write_atomically(&path, repository.render().as_bytes())
}

/// Replace the file at `path` with `bytes`, through a temporary file beside
/// it, so a reader sees the old file or the new one. It takes the mode and
/// descriptor its directory gives it, as anything the machine writes there.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().expect("a file is in a directory");
    let tmp = tempfile::NamedTempFile::new_in(dir).with_context(|| format!("{}", dir.display()))?;
    fs::write(tmp.path(), bytes).with_context(|| format!("{}", path.display()))?;
    tmp.persist(path)
        .map_err(|err| err.error)
        .with_context(|| format!("{}", path.display()))?;
    Ok(())
}

/// Remove the repository `name` from `dir`.
pub fn remove_repository(dir: &Path, name: &str) -> Result<()> {
    check_name(name)?;
    let path = dir.join("repositories").join(format!("{name}.toml"));
    fs::remove_file(&path).with_context(|| format!("{}: no such repository", path.display()))
}

/// What resolving from configured repositories chose, and the requests as
/// they were settled: each bare request for a package following the index
/// or channel its repository tracks.
#[derive(Debug)]
pub struct Settled {
    pub resolution: crate::Resolution,
    pub requests: Vec<crate::Request>,
}

/// Resolve `requests` for `arch` from `repositories`, each checked as
/// [`Repository::pointer`] checks it, recording what was seen in
/// `state_dir`.
///
/// A request for a package that names no index and isn't for one exact
/// version follows the index or channel its repository tracks, if it
/// tracks one. Every package name belongs to one repository: a name two of
/// them publish is an error, for a request or anything it needs.
pub fn resolve(
    requests: &[crate::Request],
    repositories: &[Repository],
    state_dir: &Path,
    arch: &str,
) -> Result<Settled> {
    if repositories.is_empty() {
        bail!("no repository is configured to fetch from");
    }
    for repository in repositories {
        repository.pointer(state_dir)?;
    }
    let holders = |name: &str| -> Vec<&Repository> {
        (repositories.iter())
            .filter(|repository| repository.path.join("refs").join(name).is_dir())
            .collect()
    };
    let one = |name: &str| -> Result<&Repository> {
        match holders(name).as_slice() {
            [repository] => Ok(repository),
            [] => bail!("{name}: no configured repository publishes it"),
            several => {
                let names: Vec<&str> = several.iter().map(|r| r.name.as_str()).collect();
                bail!(
                    "{name}: published by the repositories {}; a name may come from only one",
                    names.join(" and ")
                )
            }
        }
    };
    let mut settled = Vec::with_capacity(requests.len());
    for request in requests {
        let mut request = request.clone();
        if let crate::Wanted::Package(name) = &request.wanted {
            let repository = one(name)?;
            if request.index.is_none() && request.constraint.exact().is_none() {
                request.index.clone_from(&repository.track);
            }
        }
        settled.push(request);
    }
    let paths: Vec<PathBuf> = repositories.iter().map(|r| r.path.clone()).collect();
    let signed = (repositories.iter())
        .filter_map(|r| r.key.clone().map(|key| (r.path.clone(), key)))
        .collect();
    let resolution = crate::resolve_signed(&settled, &paths, arch, &signed)?;
    for package in &resolution.packages {
        one(package.id.name())?;
    }
    Ok(Settled {
        resolution,
        requests: settled,
    })
}
