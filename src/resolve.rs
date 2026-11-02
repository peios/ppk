//! Resolving: choosing the exact packages an environment needs, from what it
//! asks for and what one or more artifact directories hold, so that a
//! profile can be written.
//!
//! The choice is greedy. Each package or capability asked for, and each that
//! a chosen package depends on or requires, is settled once, by the newest
//! version that satisfies it then. Nothing chosen is reconsidered, so a later
//! requirement that the choice doesn't satisfy is an error, which says what
//! conflicted with what, and whether another version would have satisfied
//! both. That is the evidence a resolver that backtracks would need.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::id::check_name;
use crate::metadata::check_capability;
use crate::store::{self, ArtifactDir};
use crate::{Constraint, Identity, Metadata, PackageId, Profile, Version, parse_manifest};

/// What a request or requirement asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wanted {
    /// A package, by name.
    Package(String),
    /// A capability, provided by whichever package provides it. A package
    /// provides its own name at its own version, as well as what its
    /// `provides` table lists at the versions it gives (PSPU 5.4: names
    /// share one namespace), and either satisfies a requirement.
    Capability(String),
}

/// Something asked for, at the versions that will do.
#[derive(Clone, Debug)]
pub struct Request {
    pub wanted: Wanted,
    pub constraint: Constraint,
}

impl Request {
    /// A request for the package `name`. The name must be a package name.
    pub fn package(name: &str, constraint: Constraint) -> Result<Self> {
        check_name(name)?;
        Ok(Self {
            wanted: Wanted::Package(name.to_owned()),
            constraint,
        })
    }

    /// A request for the capability `name`, which must be a capability's
    /// name.
    pub fn capability(name: &str, constraint: Constraint) -> Result<Self> {
        check_capability(name)?;
        Ok(Self {
            wanted: Wanted::Capability(name.to_owned()),
            constraint,
        })
    }

    /// A request for a package written as its name, then optionally a
    /// constraint, such as `org.gnu.gcc >= 16` or `org.gnu.make`.
    pub fn parse_package(s: &str) -> Result<Self> {
        let (name, constraint) = split(s)?;
        Self::package(name, constraint)
    }

    /// A request for a capability written as [`Request::parse_package`]
    /// reads a package's, such as `libc.so.6 >= 2.34` or `sh`.
    pub fn parse_capability(s: &str) -> Result<Self> {
        let (name, constraint) = split(s)?;
        Self::capability(name, constraint)
    }
}

/// The name and constraint in `s`. The name ends at whitespace or an
/// operator, neither of which a name can hold, and no constraint means any
/// version.
fn split(s: &str) -> Result<(&str, Constraint)> {
    let s = s.trim();
    let end = s
        .find(|c: char| c.is_whitespace() || "=<>!".contains(c))
        .unwrap_or(s.len());
    let (name, constraint) = s.split_at(end);
    if name.is_empty() {
        bail!("'{s}': no name");
    }
    let constraint =
        Constraint::from_manifest_string(constraint.trim()).with_context(|| format!("'{s}'"))?;
    Ok((name, constraint))
}

/// A package chosen.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub id: PackageId,
    /// Its manifest's identity.
    pub identity: Identity,
    /// The first artifact directory given that holds it.
    pub artifact_dir: PathBuf,
    /// Why it was chosen: what first asked for it.
    pub reason: String,
}

/// The packages chosen, one version of each name, sorted by name.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub packages: Vec<Resolved>,
}

impl Resolution {
    /// A profile of the packages chosen, and nothing else.
    pub fn profile(&self) -> Profile {
        Profile {
            packages: self.packages.iter().map(|p| p.id.clone()).collect(),
            ..Profile::default()
        }
    }
}

/// Choose a version of every package `requests` need, from the packages for
/// `arch` in `artifact_dirs`.
///
/// A dependency is on a package, by name. A requirement is on a capability,
/// which a package provides if its `provides` table lists it, or if it is the
/// package's own name; a provided capability with no version satisfies any
/// constraint. The packages chosen satisfy every request, dependency and
/// requirement, and hold one version of each name.
///
/// Packages asked for by name are settled before capabilities, at every
/// step: the requests, then the dependencies of what they chose, and only
/// when no dependency is left, the first capability waiting. So a package
/// named anywhere is chosen by its name, and a capability then finds it.
/// Within each kind, the order is the order of the requests, then of each
/// manifest's tables.
///
/// Each is settled once:
///
/// - A package not yet chosen is chosen at the newest version that satisfies
///   the constraint.
/// - A capability that a chosen package provides at a satisfying version
///   needs nothing more. One that chosen packages provide only at other
///   versions is a conflict. Otherwise, the one package name that provides it
///   is chosen, at the newest version that provides it at a satisfying
///   version. If several names provide it, that's an error: the request must
///   choose one by name.
/// - Anything that a package already chosen doesn't satisfy is a conflict,
///   and an error. It names the requirement, the choice and why it was made,
///   and says whether some version satisfies both, which only backtracking
///   would find.
///
/// Two spellings of one version, such as `1.0~a-1` and `1.0~alpha-1`
/// (PSPU 5.6), can't be told apart by a constraint, so choosing either is an
/// error, which asks for one to be removed. A package in several directories
/// must be the same package in each.
pub fn resolve(requests: &[Request], artifact_dirs: &[PathBuf], arch: &str) -> Result<Resolution> {
    let index = Index::read(artifact_dirs, arch)?;
    let mut resolver = Resolver {
        index: &index,
        artifact_dirs,
        arch,
        chosen: BTreeMap::new(),
        packages: VecDeque::new(),
        capabilities: VecDeque::new(),
    };
    for request in requests {
        resolver.push(Need {
            wanted: request.wanted.clone(),
            constraint: request.constraint.clone(),
            by: None,
        });
    }
    loop {
        if let Some(need) = resolver.packages.pop_front() {
            resolver.package(need)?;
        } else if let Some(need) = resolver.capabilities.pop_front() {
            resolver.capability(need)?;
        } else {
            break;
        }
    }
    let packages = resolver
        .chosen
        .into_values()
        .map(|choice| {
            let candidate = &index.names[choice.name][choice.index];
            Resolved {
                id: candidate.id.clone(),
                identity: candidate.identity.clone(),
                artifact_dir: candidate.artifact_dir.clone(),
                reason: choice.needs[0].to_string(),
            }
        })
        .collect();
    Ok(Resolution { packages })
}

/// A package one of the artifact directories holds.
struct Candidate {
    id: PackageId,
    identity: Identity,
    artifact_dir: PathBuf,
    metadata: Metadata,
}

impl Candidate {
    /// The version at which this provides `capability`, for an error:
    /// `Some(None)` for no version, and `None` if it doesn't provide it. A
    /// `provides` entry is given before its own name.
    fn provides(&self, capability: &str) -> Option<Option<&Version>> {
        match self.metadata.provides.get(capability) {
            Some(version) => Some(version.as_ref()),
            None if capability == self.id.name() => Some(Some(self.id.parsed_version())),
            None => None,
        }
    }

    /// Whether this satisfies `need`. A capability is satisfied by its own
    /// name at its own version, or by a `provides` entry at that entry's
    /// version (PSPU 5.21).
    fn satisfies(&self, need: &Need) -> bool {
        let by_name = |name: &str| {
            name == self.id.name() && need.constraint.matches(self.id.parsed_version())
        };
        match &need.wanted {
            Wanted::Package(name) => by_name(name),
            Wanted::Capability(capability) => {
                by_name(capability)
                    || match self.metadata.provides.get(capability) {
                        Some(Some(version)) => need.constraint.matches(version),
                        Some(None) => true,
                        None => false,
                    }
            }
        }
    }
}

/// Every package for one architecture in the artifact directories, by name,
/// newest first.
struct Index {
    names: BTreeMap<String, Vec<Candidate>>,
}

impl Index {
    fn read(artifact_dirs: &[PathBuf], arch: &str) -> Result<Self> {
        if artifact_dirs.is_empty() {
            bail!("no artifact directory to choose packages from");
        }
        let mut names: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
        for dir in artifact_dirs {
            let layout = ArtifactDir::new(dir);
            for name in package_names(dir)? {
                for version in store::versions(&name, arch, dir)? {
                    let id = PackageId::new(&name, &version, arch)?;
                    let Some(identity) = store::read_ref(&layout.ref_path(&id))? else {
                        continue;
                    };
                    let held = names.entry(name.clone()).or_default();
                    if let Some(first) = held.iter().find(|c| c.id == id) {
                        if first.identity != identity {
                            bail!(
                                "{id} is {} in {}, but {identity} in {}",
                                first.identity,
                                first.artifact_dir.display(),
                                dir.display()
                            );
                        }
                        continue;
                    }
                    let metadata = read_metadata(&layout, &id, &identity)?;
                    held.push(Candidate {
                        id,
                        identity,
                        artifact_dir: dir.clone(),
                        metadata,
                    });
                }
            }
        }
        for candidates in names.values_mut() {
            candidates.sort_by(|a, b| {
                b.id.parsed_version()
                    .cmp(a.id.parsed_version())
                    .then_with(|| a.id.version().cmp(b.id.version()))
            });
        }
        Ok(Self { names })
    }

    fn get(&self, name: &str) -> &[Candidate] {
        self.names.get(name).map_or(&[], Vec::as_slice)
    }
}

/// The names of the packages `dir` has refs for. A name that isn't a
/// package name can't be a ref, and is passed over.
fn package_names(dir: &Path) -> Result<Vec<String>> {
    let refs = dir.join("refs");
    let entries = match fs::read_dir(&refs) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if !dir.is_dir() {
                bail!("{}: not an artifact directory", dir.display());
            }
            return Ok(Vec::new());
        }
        Err(err) => return Err(err).with_context(|| format!("{}", refs.display())),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("{}", refs.display()))?;
        if let Some(name) = entry.file_name().to_str()
            && check_name(name).is_ok()
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// The metadata of the package `id`, whose manifest is `identity`, checked
/// against its hash and against the package its ref says it is.
fn read_metadata(layout: &ArtifactDir, id: &PackageId, identity: &Identity) -> Result<Metadata> {
    let path = layout.manifest_path(identity);
    let bytes = fs::read(&path).with_context(|| format!("{}", path.display()))?;
    if Identity::of(&bytes) != *identity {
        bail!("{}: does not match its hash", path.display());
    }
    let manifest = parse_manifest(&bytes).with_context(|| format!("{}", path.display()))?;
    if manifest.id != *id {
        bail!(
            "{}: is {}, but its ref says {id}",
            path.display(),
            manifest.id
        );
    }
    Ok(manifest.metadata)
}

/// A request, dependency or requirement to settle.
#[derive(Clone, Debug)]
struct Need {
    wanted: Wanted,
    constraint: Constraint,
    /// The package that needs it, or none for a request.
    by: Option<PackageId>,
}

/// What is needed, as `org.gnu.gcc >= 16` or `sh`.
struct Wants<'a>(&'a Need);

impl fmt::Display for Wants<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match &self.0.wanted {
            Wanted::Package(name) | Wanted::Capability(name) => name,
        };
        match self.0.constraint.is_any() {
            true => write!(f, "{name}"),
            false => write!(f, "{name} {}", self.0.constraint),
        }
    }
}

/// Who needs what, as `org.gnu.make@4.4-1 depends on org.gnu.glibc >= 2.34`.
impl fmt::Display for Need {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verb = match self.wanted {
            Wanted::Package(_) => "depends on",
            Wanted::Capability(_) => "requires",
        };
        match &self.by {
            None => write!(f, "the request for {}", Wants(self)),
            Some(by) => write!(f, "{} {verb} {}", Short(by), Wants(self)),
        }
    }
}

/// A package as `name@version`: every package resolved is for one
/// architecture.
struct Short<'a>(&'a PackageId);

impl fmt::Display for Short<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.0.name(), self.0.version())
    }
}

/// A package chosen, and every need it has been checked against, the one
/// that chose it first.
struct Choice<'a> {
    name: &'a str,
    index: usize,
    needs: Vec<Need>,
}

struct Resolver<'a> {
    index: &'a Index,
    artifact_dirs: &'a [PathBuf],
    arch: &'a str,
    chosen: BTreeMap<&'a str, Choice<'a>>,
    packages: VecDeque<Need>,
    capabilities: VecDeque<Need>,
}

impl<'a> Resolver<'a> {
    fn push(&mut self, need: Need) {
        match need.wanted {
            Wanted::Package(_) => self.packages.push_back(need),
            Wanted::Capability(_) => self.capabilities.push_back(need),
        }
    }

    fn candidate(&self, choice: &Choice) -> &'a Candidate {
        &self.index.names[choice.name][choice.index]
    }

    fn package(&mut self, need: Need) -> Result<()> {
        let Wanted::Package(name) = &need.wanted else {
            unreachable!("a package need is for a package");
        };
        if let Some(choice) = self.chosen.get(name.as_str()) {
            let candidate = self.candidate(choice);
            if !candidate.satisfies(&need) {
                bail!("{}", self.conflict(&need, choice));
            }
            self.chosen
                .get_mut(name.as_str())
                .expect("it was just found")
                .needs
                .push(need);
            return Ok(());
        }
        let candidates = self.index.get(name);
        if candidates.is_empty() {
            bail!(
                "{need}, but no artifact directory holds {name} for {}: {}",
                self.arch,
                self.dirs()
            );
        }
        let Some(index) = self.newest(candidates, &need)? else {
            let versions: Vec<&str> = candidates.iter().map(|c| c.id.version()).collect();
            bail!(
                "{need}, but no version satisfies it; there are {}",
                versions.join(", ")
            );
        };
        self.choose(&candidates[index], index, need);
        Ok(())
    }

    fn capability(&mut self, need: Need) -> Result<()> {
        let Wanted::Capability(capability) = &need.wanted else {
            unreachable!("a capability need is for a capability");
        };
        let mut providing = Vec::new();
        for (name, choice) in &self.chosen {
            let candidate = self.candidate(choice);
            if candidate.satisfies(&need) {
                let name = *name;
                self.chosen
                    .get_mut(name)
                    .expect("it was just found")
                    .needs
                    .push(need);
                return Ok(());
            }
            if candidate.provides(capability).is_some() {
                providing.push(choice);
            }
        }
        if let Some(choice) = providing.first() {
            bail!("{}", self.conflict(&need, choice));
        }

        // The names with a version that provides it. One already chosen,
        // at a version that doesn't, conflicts.
        let mut names: Vec<&'a str> = Vec::new();
        for (name, candidates) in &self.index.names {
            if !candidates.iter().any(|c| c.satisfies(&need)) {
                continue;
            }
            if let Some(choice) = self.chosen.get(name.as_str()) {
                bail!("{}", self.conflict(&need, choice));
            }
            names.push(name);
        }
        let name = match names.as_slice() {
            [name] => *name,
            [] => {
                let providers: Vec<String> = (self.index.names.values())
                    .flatten()
                    .filter_map(|c| match c.provides(capability)? {
                        Some(version) => Some(format!("{} provides {version}", Short(&c.id))),
                        None => Some(format!("{} provides it", Short(&c.id))),
                    })
                    .collect();
                match providers.is_empty() {
                    true => bail!("{need}, but no package provides {capability}"),
                    false => bail!(
                        "{need}, but no package provides a version that satisfies it; {}",
                        providers.join(", ")
                    ),
                }
            }
            _ => bail!(
                "{need}, but {} all provide it; request one of them by name",
                names.join(", ")
            ),
        };
        let candidates = self.index.get(name);
        let index = self
            .newest(candidates, &need)?
            .expect("a candidate of this name satisfies it");
        self.choose(&candidates[index], index, need);
        Ok(())
    }

    /// The newest of `candidates` that satisfies `need`, refusing two
    /// spellings of that version.
    fn newest(&self, candidates: &[Candidate], need: &Need) -> Result<Option<usize>> {
        let Some(index) = candidates.iter().position(|c| c.satisfies(need)) else {
            return Ok(None);
        };
        let chosen = &candidates[index].id;
        if let Some(other) = candidates[index + 1..]
            .iter()
            .find(|c| c.id.parsed_version() == chosen.parsed_version() && c.satisfies(need))
        {
            bail!(
                "{need}, and {} and {} are one version spelt two ways, which no constraint \
                 can tell apart; remove one of them from {}",
                Short(chosen),
                Short(&other.id),
                other.artifact_dir.display()
            );
        }
        Ok(Some(index))
    }

    fn choose(&mut self, candidate: &'a Candidate, index: usize, need: Need) {
        let by = Some(candidate.id.clone());
        for (name, constraint) in &candidate.metadata.dependencies {
            self.push(Need {
                wanted: Wanted::Package(name.clone()),
                constraint: constraint.clone(),
                by: by.clone(),
            });
        }
        for (capability, constraint) in &candidate.metadata.requires {
            self.push(Need {
                wanted: Wanted::Capability(capability.clone()),
                constraint: constraint.clone(),
                by: by.clone(),
            });
        }
        let name = candidate.id.name();
        self.chosen.insert(
            name,
            Choice {
                name,
                index,
                needs: vec![need],
            },
        );
    }

    /// The error for `need`, which `choice` doesn't satisfy: what was chosen
    /// and why, and whether another version would satisfy everything the
    /// choice was checked against as well as `need`.
    fn conflict(&self, need: &Need, choice: &Choice) -> String {
        let chosen = self.candidate(choice);
        let mut message = format!(
            "{need}, but {} was chosen for {}",
            Short(&chosen.id),
            choice.needs[0]
        );
        let all: Vec<&Need> = choice.needs.iter().chain([need]).collect();
        let both = match all.len() {
            2 => "both".to_owned(),
            n => format!("all {n} requirements on it"),
        };
        match self
            .index
            .get(choice.name)
            .iter()
            .find(|c| all.iter().all(|need| c.satisfies(need)))
        {
            Some(other) => message.push_str(&format!(
                "; {} would satisfy {both}, but finding it needs backtracking, which ppk \
                 doesn't do",
                Short(&other.id)
            )),
            None => message.push_str(&format!("; no version of {} satisfies {both}", choice.name)),
        }
        message
    }

    /// The artifact directories, for an error.
    fn dirs(&self) -> String {
        let dirs: Vec<String> = (self.artifact_dirs.iter())
            .map(|dir| dir.display().to_string())
            .collect();
        dirs.join(", ")
    }
}
