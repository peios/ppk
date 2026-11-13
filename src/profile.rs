//! Environment profiles: TOML files that say exactly what an environment
//! holds, so that a composed root or a `run` can be described once and
//! named, rather than spelt out in options each time.
//!
//! ```text
//! arch = "x86_64"
//! include = ["fsbase.toml"]
//! dirs = ["proc", "sys", "dev", "tmp"]
//!
//! [packages]
//! "net.busybox.busybox" = "1.37.0-1"
//!
//! [symlinks]
//! "bin" = ":bin"
//! ```
//!
//! `packages` maps each package asked for to the versions that will do, as
//! a constraint such as `">= 1.36"`, or `""` for any. A bare version asks for
//! exactly that version. `#` and an index or channel, as in `"#stable"` or
//! `"#stable >= 1.36"`, asks for the version that index holds. A profile holds requests, not their answer: what it
//! asks for is resolved when it is composed, with what that needs, and the
//! closure's inventory records the answer. Every package is for `arch`, the
//! host's by default. A symlink's value is what follows `<path>:` in `--symlink`, so
//! a leading `:` stands for `usr`. `dirs` are empty directories, and
//! `include_artifacts = true` gives a composed root its packages in an
//! artifact directory of its own, as `--include-artifacts` does. Every key
//! is optional, and any other is an error, so that a misspelt one cannot
//! silently leave something out.
//!
//! `include` names other profiles, fragments, whose contents the profile
//! holds as well, each relative to the directory of the file that names it.
//! A fragment can include others. Each file's `arch` is for its own
//! packages. Fragments are merged as [`Profile::merge`] merges, and a
//! disagreement is an error naming both files. A file that includes itself,
//! directly or not, is an error; one included twice by different routes is
//! merged once more, which changes nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::resolve::{Request, Wanted};
use crate::{Constraint, EnvDir, EnvSymlink, HOST_ARCH, PackageId};

/// The keys a profile may have.
const KEYS: [&str; 6] = [
    "arch",
    "include",
    "packages",
    "symlinks",
    "dirs",
    "include_artifacts",
];

/// What an environment holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    /// The architecture its packages are for, if it says.
    pub arch: Option<String>,
    /// The packages it asks for, by name, each with the versions that will
    /// do, at most one request for each name.
    pub packages: Vec<Request>,
    /// Symlinks a composed root holds besides its `usr`.
    pub symlinks: Vec<EnvSymlink>,
    /// Empty directories a composed root holds.
    pub dirs: Vec<EnvDir>,
    /// Whether a composed root holds its packages in an artifact directory
    /// of its own, as `--include-artifacts` gives it.
    pub include_artifacts: bool,
}

impl Profile {
    /// The profile in the file at `path`, with the fragments it includes.
    pub fn read(path: &Path) -> Result<Self> {
        Self::read_all(std::slice::from_ref(&path.to_path_buf()))
    }

    /// The profiles in the files at `paths`, with the fragments they
    /// include, merged into one as fragments are.
    pub fn read_all(paths: &[PathBuf]) -> Result<Self> {
        let mut merged = Merged::default();
        for path in paths {
            merged.read(path, &mut Vec::new())?;
        }
        Ok(merged.profile)
    }

    /// The profile `text` describes. It may not include fragments, since
    /// there is no file to find them from; [`Profile::parse_in`] can.
    pub fn parse(text: &str) -> Result<Self> {
        let mut merged = Merged::default();
        merged.parse(text, "the profile", None, &mut Vec::new())?;
        Ok(merged.profile)
    }

    /// The profile `text` describes, finding the fragments it includes
    /// relative to `dir`, as if it were a file there.
    pub fn parse_in(text: &str, dir: &Path) -> Result<Self> {
        let mut merged = Merged::default();
        merged.parse(text, "the profile", Some(dir), &mut Vec::new())?;
        Ok(merged.profile)
    }

    /// Adds everything `other` holds to this profile, as an included
    /// fragment is added. What both hold alike is held once. It is an error
    /// for the two to hold different versions of one package, packages for
    /// different architectures, different symlinks at one path, or a
    /// symlink where the other has a directory. `include_artifacts` is true
    /// if either says so, since a profile can't say false.
    pub fn merge(&mut self, other: &Profile) -> Result<()> {
        let mut merged = Merged::default();
        merged.add(self.clone(), "this profile")?;
        merged.add(other.clone(), "the other")?;
        *self = merged.profile;
        Ok(())
    }

    /// A profile asking for exactly each of `packages`, which must all be
    /// for one architecture.
    pub fn exact(packages: &[PackageId]) -> Result<Self> {
        let mut profile = Self::default();
        profile.add_packages(packages.iter().cloned())?;
        Ok(profile)
    }

    /// The architecture the profile's packages are for: the one it names,
    /// or the host's.
    pub fn arch(&self) -> &str {
        self.arch.as_deref().unwrap_or(HOST_ARCH)
    }

    /// The exact package each request asks for, when every request asks
    /// for exactly one version, as an environment composed without
    /// resolving needs.
    pub fn exact_packages(&self) -> Result<Vec<PackageId>> {
        (self.packages.iter())
            .map(|request| {
                let Wanted::Package(name) = &request.wanted else {
                    bail!("a profile asks only for packages");
                };
                let Some(version) = request.constraint.exact() else {
                    bail!(
                        "{name}: asks for {}, not one exact version",
                        request.constraint
                    );
                };
                PackageId::new(name, &version.to_string(), self.arch())
            })
            .collect()
    }

    /// The profile as TOML, which [`Profile::parse`] reads back as it is.
    pub fn render(&self) -> Result<String> {
        let quote = |s: &str| toml::Value::String(s.to_owned()).to_string();
        let mut out = String::new();
        if let Some(arch) = &self.arch {
            out.push_str(&format!("arch = {}\n", quote(arch)));
        }
        if self.include_artifacts {
            out.push_str("include_artifacts = true\n");
        }
        if !self.dirs.is_empty() {
            let dirs: Vec<String> = self.dirs.iter().map(|dir| quote(dir.path())).collect();
            out.push_str(&format!("dirs = [{}]\n", dirs.join(", ")));
        }
        if !self.packages.is_empty() {
            out.push_str("\n[packages]\n");
            for request in &self.packages {
                let Wanted::Package(name) = &request.wanted else {
                    bail!("a profile asks only for packages");
                };
                let versions = match (&request.index, request.constraint.exact()) {
                    (None, Some(version)) => version.to_string(),
                    _ => request.versions(),
                };
                out.push_str(&format!("{} = {}\n", quote(name), quote(&versions)));
            }
        }
        if !self.symlinks.is_empty() {
            out.push_str("\n[symlinks]\n");
            // A target without a leading `:` is written as given, so the
            // link's own target reads back as itself.
            for link in &self.symlinks {
                out.push_str(&format!(
                    "{} = {}\n",
                    quote(link.path()),
                    quote(link.target())
                ));
            }
        }
        Ok(out)
    }

    /// Adds a request for exactly each of `packages` to the profile's. One
    /// it asks for already, exactly so, is added once; any other request for
    /// one of the names, or a package for another architecture, is an error.
    pub fn add_packages(&mut self, packages: impl IntoIterator<Item = PackageId>) -> Result<()> {
        for id in packages {
            match &self.arch {
                Some(arch) if arch != id.arch() => {
                    bail!("{id}: the environment's packages are for {arch}")
                }
                Some(_) => {}
                None => self.arch = Some(id.arch().to_owned()),
            }
            let request = Request::package(
                id.name(),
                Constraint::parse(&format!("= {}", id.version()))?,
            )?;
            match self
                .packages
                .iter()
                .find(|held| held.wanted == request.wanted)
            {
                Some(held) if *held == request => {}
                Some(held) => bail!(
                    "{id}: the environment asks for {} {} already",
                    id.name(),
                    held.constraint
                ),
                None => self.packages.push(request),
            }
        }
        Ok(())
    }

    /// Adds `symlinks` and `dirs` to the profile's. One it holds already
    /// is added once; a symlink at a path it links elsewhere is an error.
    pub fn add_extras(
        &mut self,
        symlinks: impl IntoIterator<Item = EnvSymlink>,
        dirs: impl IntoIterator<Item = EnvDir>,
    ) -> Result<()> {
        for link in symlinks {
            match self.symlinks.iter().find(|held| held.path() == link.path()) {
                Some(held) if *held == link => {}
                Some(held) => bail!(
                    "{}: links to {}, but the environment links it to {}",
                    link.path(),
                    link.target(),
                    held.target()
                ),
                None => self.symlinks.push(link),
            }
        }
        for dir in dirs {
            if !self.dirs.contains(&dir) {
                self.dirs.push(dir);
            }
        }
        Ok(())
    }
}

/// A profile being merged from files, knowing which file gave each
/// package, symlink and directory, so that a disagreement can name both.
#[derive(Default)]
struct Merged {
    profile: Profile,
    packages: BTreeMap<String, String>,
    symlinks: BTreeMap<String, String>,
    dirs: BTreeMap<String, String>,
}

impl Merged {
    /// Merge the profile at `path`. `chain` is the files including it, each
    /// as found and as named, so that one including itself is found.
    fn read(&mut self, path: &Path, chain: &mut Vec<(PathBuf, String)>) -> Result<()> {
        let origin = path.display().to_string();
        let text = fs::read_to_string(path).with_context(|| origin.clone())?;
        let canonical = fs::canonicalize(path).with_context(|| origin.clone())?;
        if let Some(first) = chain.iter().position(|(held, _)| *held == canonical) {
            let names: Vec<&str> = chain[first..]
                .iter()
                .map(|(_, name)| name.as_str())
                .chain([origin.as_str()])
                .collect();
            bail!("include cycle: {}", names.join(" includes "));
        }
        let dir = path.parent().unwrap_or(Path::new(""));
        chain.push((canonical, origin.clone()));
        self.parse(&text, &origin, Some(dir), chain)?;
        chain.pop();
        Ok(())
    }

    /// Merge the profile `text`, named `origin`, finding what it includes
    /// relative to `dir`: fragments first, then its own contents. An error
    /// in the text is given with its origin; one in merging names the files
    /// itself.
    fn parse(
        &mut self,
        text: &str,
        origin: &str,
        dir: Option<&Path>,
        chain: &mut Vec<(PathBuf, String)>,
    ) -> Result<()> {
        // Text that is not a file is given no origin, as before fragments.
        let in_file = !chain.is_empty();
        let (doc, include) = within(parse_doc(text, dir), in_file, origin)?;
        for fragment in include {
            self.read(&fragment, chain)?;
        }
        let own = within(flat(&doc), in_file, origin)?;
        self.add(own, origin)
    }

    /// Add `profile`, from `origin`, refusing what disagrees with what is
    /// held already.
    fn add(&mut self, profile: Profile, origin: &str) -> Result<()> {
        if !profile.packages.is_empty() {
            let arch = profile.arch().to_owned();
            match &self.profile.arch {
                Some(held) if *held != arch => bail!(
                    "{origin} has packages for {arch}, but {} has packages for another architecture, {held}",
                    self.packages
                        .values()
                        .next()
                        .map_or("another", String::as_str)
                ),
                Some(_) => {}
                None => self.profile.arch = Some(arch),
            }
        }
        for request in profile.packages {
            let Wanted::Package(name) = &request.wanted else {
                bail!("{origin}: a profile asks only for packages");
            };
            if let Some(held) =
                (self.profile.packages.iter()).find(|held| held.wanted == request.wanted)
            {
                if *held != request {
                    bail!(
                        "{origin} asks for {name} {}, but {} asks for {name} {}",
                        request.constraint,
                        self.packages[name],
                        held.constraint
                    );
                }
                continue;
            }
            self.packages.insert(name.clone(), origin.to_owned());
            self.profile.packages.push(request);
        }
        for link in profile.symlinks {
            if let Some(dir_origin) = self.dirs.get(link.path()) {
                bail!(
                    "{origin} links {:?}, but {dir_origin} makes it a directory",
                    link.path()
                );
            }
            if let Some(held) = self
                .profile
                .symlinks
                .iter()
                .find(|held| held.path() == link.path())
            {
                if *held != link {
                    bail!(
                        "{origin} links {:?} to {:?}, but {} links it to {:?}",
                        link.path(),
                        link.target(),
                        self.symlinks[link.path()],
                        held.target()
                    );
                }
                continue;
            }
            self.symlinks
                .insert(link.path().to_owned(), origin.to_owned());
            self.profile.symlinks.push(link);
        }
        for dir in profile.dirs {
            if let Some(link_origin) = self.symlinks.get(dir.path()) {
                bail!(
                    "{origin} makes {:?} a directory, but {link_origin} links it",
                    dir.path()
                );
            }
            if !self.dirs.contains_key(dir.path()) {
                self.dirs.insert(dir.path().to_owned(), origin.to_owned());
                self.profile.dirs.push(dir);
            }
        }
        self.profile.include_artifacts |= profile.include_artifacts;
        Ok(())
    }
}

/// `result`, with `origin` as its context if it is from a file.
fn within<T>(result: Result<T>, in_file: bool, origin: &str) -> Result<T> {
    if in_file {
        result.with_context(|| origin.to_owned())
    } else {
        result
    }
}

/// The document `text` holds, with its keys checked, and the fragments it
/// includes, found relative to `dir`.
fn parse_doc(text: &str, dir: Option<&Path>) -> Result<(toml::Table, Vec<PathBuf>)> {
    let doc: toml::Table =
        toml::from_str(text).map_err(|err| anyhow!("not valid TOML: {}", err.message()))?;
    for key in doc.keys() {
        if !KEYS.contains(&key.as_str()) {
            bail!("unknown key {key:?}");
        }
    }
    let mut fragments = Vec::new();
    if let Some(include) = doc.get("include") {
        let include = include
            .as_array()
            .ok_or_else(|| anyhow!("include is not a list"))?;
        let Some(dir) = dir else {
            bail!("include: a profile given as text has no directory to find fragments in");
        };
        for fragment in include {
            let fragment = fragment
                .as_str()
                .ok_or_else(|| anyhow!("include: {fragment} is not a string"))?;
            if fragment.is_empty() {
                bail!("include: an empty path");
            }
            fragments.push(dir.join(fragment));
        }
    }
    Ok((doc, fragments))
}

/// The profile `doc` gives itself, without what it includes.
fn flat(doc: &toml::Table) -> Result<Profile> {
    let arch = match doc.get("arch") {
        Some(arch) => arch
            .as_str()
            .ok_or_else(|| anyhow!("arch is not a string"))?,
        None => HOST_ARCH,
    };

    let mut profile = Profile {
        arch: doc.get("arch").map(|_| arch.to_owned()),
        ..Profile::default()
    };
    for (name, versions) in table(doc, "packages")? {
        let versions = versions
            .as_str()
            .ok_or_else(|| anyhow!("packages: {name:?} has versions that are not a string"))?;
        let (index, constraint) =
            Request::parse_versions(versions).with_context(|| format!("packages: {name:?}"))?;
        let request = Request::package(name, constraint)
            .and_then(|request| request.from_index(index.as_deref()))
            .context("packages")?;
        profile.packages.push(request);
    }
    for (path, target) in table(doc, "symlinks")? {
        let target = target
            .as_str()
            .ok_or_else(|| anyhow!("symlinks: {path:?} has a target that is not a string"))?;
        if target.is_empty() || target == ":" {
            bail!("symlinks: {path:?} has no target");
        }
        let link = format!("{path}:{target}").parse().context("symlinks")?;
        profile.symlinks.push(link);
    }
    if let Some(dirs) = doc.get("dirs") {
        let dirs = dirs
            .as_array()
            .ok_or_else(|| anyhow!("dirs is not a list"))?;
        for dir in dirs {
            let dir = dir
                .as_str()
                .ok_or_else(|| anyhow!("dirs: {dir} is not a string"))?;
            profile.dirs.push(dir.parse().context("dirs")?);
        }
    }
    if let Some(include) = doc.get("include_artifacts") {
        profile.include_artifacts = include
            .as_bool()
            .ok_or_else(|| anyhow!("include_artifacts is not true or false"))?;
    }
    Ok(profile)
}

/// The table `key` of `doc`, empty if there is none.
fn table<'a>(doc: &'a toml::Table, key: &str) -> Result<Vec<(&'a str, &'a toml::Value)>> {
    match doc.get(key) {
        None => Ok(Vec::new()),
        Some(toml::Value::Table(table)) => Ok(table
            .iter()
            .map(|(name, value)| (name.as_str(), value))
            .collect()),
        Some(_) => bail!("{key} is not a table"),
    }
}
