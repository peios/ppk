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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::id::check_name;
use crate::manifest::parse_summary;
use crate::metadata::check_capability;
use crate::repo::{self, Standing, check_ref_name};
use crate::store::{self, ArtifactDir};
use crate::{Constraint, Identity, Metadata, PackageId, Profile, Source, Version, parse_manifest};

/// What a request or requirement asks for.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Wanted {
    /// A package, by name.
    Package(String),
    /// A capability, provided by whichever package provides it. A package
    /// provides its own name at its own version, as well as what its
    /// `provides` table lists at the versions it gives (PSPU 5.4: names
    /// share one namespace), and either satisfies a requirement.
    Capability(String),
}

/// Something asked for, at the versions that will do, and from an index
/// or channel if it names one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub wanted: Wanted,
    pub constraint: Constraint,
    /// The index or channel of a repository it must come from: the version
    /// that holds, and what that version needs, from the same index.
    pub index: Option<String>,
}

impl Request {
    /// A request for the package `name`. The name must be a package name.
    pub fn package(name: &str, constraint: Constraint) -> Result<Self> {
        check_name(name)?;
        Ok(Self {
            wanted: Wanted::Package(name.to_owned()),
            constraint,
            index: None,
        })
    }

    /// A request for the capability `name`, which must be a capability's
    /// name.
    pub fn capability(name: &str, constraint: Constraint) -> Result<Self> {
        check_capability(name)?;
        Ok(Self {
            wanted: Wanted::Capability(name.to_owned()),
            constraint,
            index: None,
        })
    }

    /// The same request, from the index or channel `index`.
    pub fn from_index(self, index: Option<&str>) -> Result<Self> {
        if let Some(index) = index {
            check_ref_name(index)?;
        }
        Ok(Self {
            index: index.map(str::to_owned),
            ..self
        })
    }

    /// A request for a package written as its name, then optionally `#` and
    /// an index or channel, then optionally a constraint, such as
    /// `org.gnu.gcc >= 16`, `se.curl.curl#stable` or `org.gnu.make`.
    pub fn parse_package(s: &str) -> Result<Self> {
        let (name, index, constraint) = split(s)?;
        Self::package(name, constraint)?.from_index(index)
    }

    /// A request for a capability written as [`Request::parse_package`]
    /// reads a package's, such as `libc.so.6 >= 2.34` or `sh`.
    pub fn parse_capability(s: &str) -> Result<Self> {
        let (name, index, constraint) = split(s)?;
        Self::capability(name, constraint)?.from_index(index)
    }

    /// What the request's versions are written as, after its name: `#` and
    /// its index, if it names one, then its constraint, if it has one.
    pub fn versions(&self) -> String {
        let constraint = self.constraint.to_manifest_string();
        match (&self.index, constraint.is_empty()) {
            (Some(index), true) => format!("#{index}"),
            (Some(index), false) => format!("#{index} {constraint}"),
            (None, _) => constraint,
        }
    }

    /// The request's versions written as [`Request::versions`] writes them.
    pub fn parse_versions(s: &str) -> Result<(Option<String>, Constraint)> {
        let s = s.trim();
        let (index, constraint) = match s.strip_prefix('#') {
            Some(rest) => {
                let end = rest
                    .find(|c: char| c.is_whitespace() || "=<>!".contains(c))
                    .unwrap_or(rest.len());
                let (index, constraint) = rest.split_at(end);
                check_ref_name(index)?;
                (Some(index.to_owned()), constraint)
            }
            None => (None, s),
        };
        let constraint = Constraint::from_manifest_string(constraint.trim())
            .with_context(|| format!("'{s}'"))?;
        Ok((index, constraint))
    }
}

/// The name, index and constraint in `s`. The name ends at whitespace, an
/// operator or `#`, none of which a name can hold; no index means any, and
/// no constraint means any version.
fn split(s: &str) -> Result<(&str, Option<&str>, Constraint)> {
    let s = s.trim();
    let end = s
        .find(|c: char| c.is_whitespace() || "=<>!#".contains(c))
        .unwrap_or(s.len());
    let (name, rest) = s.split_at(end);
    if name.is_empty() {
        bail!("'{s}': no name");
    }
    let rest = rest.trim_start();
    let (index, constraint) = match rest.strip_prefix('#') {
        Some(rest) => {
            let end = rest
                .find(|c: char| c.is_whitespace() || "=<>!".contains(c))
                .unwrap_or(rest.len());
            let (index, constraint) = rest.split_at(end);
            (Some(index), constraint)
        }
        None => (None, rest),
    };
    let constraint =
        Constraint::from_manifest_string(constraint.trim()).with_context(|| format!("'{s}'"))?;
    Ok((name, index, constraint))
}

/// A package chosen.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub id: PackageId,
    /// Its manifest's identity.
    pub identity: Identity,
    /// The first artifact directory or repository given that holds it.
    pub source: Source,
    /// Why it was chosen: what first asked for it.
    pub reason: String,
}

impl Resolved {
    /// The artifact directory that holds it, if it came from one rather
    /// than from a repository served over HTTP.
    pub fn artifact_dir(&self) -> Option<&Path> {
        match &self.source {
            Source::Dir(dir) => Some(dir),
            Source::Remote(_) => None,
        }
    }
}

/// The packages chosen, one version of each name, sorted by name.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub packages: Vec<Resolved>,
}

impl Resolution {
    /// A profile asking for exactly the packages chosen, and nothing else.
    pub fn profile(&self) -> Result<Profile> {
        let ids: Vec<PackageId> = self.packages.iter().map(|p| p.id.clone()).collect();
        Profile::exact(&ids)
    }

    /// The packages chosen.
    pub fn ids(&self) -> Vec<PackageId> {
        self.packages.iter().map(|p| p.id.clone()).collect()
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
    resolve_pinned(requests, &[], artifact_dirs, arch)
}

/// Resolve as [`resolve`] does, where each repository `signed` names must
/// have signed its pointer file as its requirement says. A package of such a
/// repository is a candidate only if its signed catalog lists it, and only
/// at the identity the catalog gives: its ref and manifest are checked
/// against what the signature vouches for, not trusted for themselves.
pub fn resolve_signed(
    requests: &[Request],
    artifact_dirs: &[PathBuf],
    arch: &str,
    signed: &BTreeMap<PathBuf, repo::Trust>,
) -> Result<Resolution> {
    resolve_with(requests, &[], &sources(artifact_dirs), arch, signed)
}

/// Resolve as [`resolve_pinned`] does, from `sources`, each an artifact
/// directory or a repository served over HTTP.
///
/// A repository served over HTTP can't be listed, so its packages are
/// those its catalog lists, which it must have; with a key, the catalog is
/// checked as [`resolve_signed`] checks a directory's. Every manifest it
/// lists for `arch` is read, and checked against its hash.
pub fn resolve_sources(
    requests: &[Request],
    pins: &[PackageId],
    sources: &[Source],
    arch: &str,
) -> Result<Resolution> {
    resolve_with(requests, pins, sources, arch, &BTreeMap::new())
}

/// Resolve `requests` together with exact package IDs. Pins keep the version's
/// spelling as well as its value, and must all be for `arch`. They are chosen
/// before requests, so profiles and explicit IDs cannot be silently upgraded.
pub fn resolve_pinned(
    requests: &[Request],
    pins: &[PackageId],
    artifact_dirs: &[PathBuf],
    arch: &str,
) -> Result<Resolution> {
    resolve_with(
        requests,
        pins,
        &sources(artifact_dirs),
        arch,
        &BTreeMap::new(),
    )
}

fn sources(artifact_dirs: &[PathBuf]) -> Vec<Source> {
    artifact_dirs.iter().cloned().map(Source::Dir).collect()
}

/// Resolve from `sources`, where each directory `signed` names must have
/// signed its pointer file as its requirement says. A repository served
/// over HTTP carries its own requirement.
pub(crate) fn resolve_with(
    requests: &[Request],
    pins: &[PackageId],
    sources: &[Source],
    arch: &str,
    signed: &BTreeMap<PathBuf, repo::Trust>,
) -> Result<Resolution> {
    let wanted_indexes = requests.iter().filter_map(|r| r.index.clone()).collect();
    let roots = (pins.iter().map(|id| Wanted::Package(id.name().to_owned())))
        .chain(requests.iter().map(|request| request.wanted.clone()))
        .collect::<Vec<_>>();
    let mut index = Index::read(sources, arch, &wanted_indexes, signed, &roots)?;
    let mut pinned = BTreeMap::new();
    let mut pin_requests = Vec::new();
    for id in pins {
        if id.arch() != arch {
            bail!("{id}: resolving packages for {arch}");
        }
        if let Some(other) = pinned.insert(id.name(), id)
            && other != id
        {
            bail!("{id}: conflicts with exact pin {other}");
        }
        let Some(candidates) = index.names.get_mut(id.name()) else {
            bail!("{id}: no such package in the artifact directories");
        };
        candidates.retain(|candidate| candidate.id == *id);
        if candidates.is_empty() {
            bail!("{id}: no such package in the artifact directories");
        }
        pin_requests.push(Request::package(
            id.name(),
            Constraint::parse(&format!("= {}", id.version()))?,
        )?);
    }
    let mut resolver = Resolver {
        index: &index,
        sources,
        arch,
        chosen: BTreeMap::new(),
        packages: VecDeque::new(),
        capabilities: VecDeque::new(),
    };
    for request in pin_requests.iter().chain(requests) {
        resolver.push(Need {
            wanted: request.wanted.clone(),
            constraint: request.constraint.clone(),
            index: request.index.clone(),
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
                source: candidate.source.clone(),
                reason: choice.needs[0].to_string(),
            }
        })
        .collect();
    Ok(Resolution { packages })
}

/// A package one of the sources holds.
struct Candidate {
    id: PackageId,
    identity: Identity,
    source: Source,
    metadata: Metadata,
    /// Whether its repository's catalog has yanked it.
    yanked: bool,
    /// The indexes and channels asked for that hold it.
    indexes: BTreeSet<String>,
}

impl Candidate {
    /// Whether, and at which version, this provides `capability` at all, to
    /// find a conflict and describe it: `Some(None)` for no version, and
    /// `None` if it doesn't provide it. A `provides` entry is given before
    /// its own name.
    fn provides(&self, capability: &str) -> Option<Option<&Version>> {
        (self.metadata.capabilities(&self.id))
            .find(|(name, _)| *name == capability)
            .map(|(_, version)| version)
    }

    /// Whether this satisfies `need`. A capability is satisfied by its own
    /// name at its own version, or by a `provides` entry at that entry's
    /// version (PSPU 5.21). A need from an index is satisfied only by what
    /// that index holds, and a yanked version only by a need for exactly it.
    fn satisfies(&self, need: &Need) -> bool {
        if need
            .index
            .as_ref()
            .is_some_and(|index| !self.indexes.contains(index))
        {
            return false;
        }
        if self.yanked && need.constraint.exact() != Some(self.id.parsed_version()) {
            return false;
        }
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
    /// Each version deleted from a repository, by name: its version, the
    /// repository, and why.
    deleted: BTreeMap<String, Vec<(String, Source, String)>>,
}

/// What a repository's catalog and the indexes asked for say of each of
/// its packages for one architecture.
#[derive(Default)]
struct Published {
    /// The versions yanked, as `name@version`.
    yanked: BTreeSet<String>,
    /// For each index or channel asked for, the versions it holds, as
    /// `name@version`.
    indexes: BTreeMap<String, BTreeSet<String>>,
    deleted: Vec<(String, String, String)>,
    /// For a repository whose signature was checked, the identity its
    /// catalog gives each version published, by name and version; `None`
    /// where refs are trusted for themselves.
    catalog: Option<BTreeMap<(String, String), Identity>>,
}

impl Published {
    /// What the artifact directory `source` publishes, if it is a
    /// repository.
    fn read(
        source: &Source,
        arch: &str,
        wanted: &BTreeSet<String>,
        trust: Option<&repo::Trust>,
    ) -> Result<Self> {
        let mut published = Self::default();
        let Some(pointer) = source.pointer(trust)? else {
            if trust.is_some() {
                bail!("{source}: not published as a repository, which its identity needs");
            }
            return Ok(published);
        };
        if trust.is_some() {
            published.catalog = Some(BTreeMap::new());
        }
        // A directory's packages are its refs, so its trees are read for
        // what they say of them, whether or not they give summaries.
        let trees = source.trees();
        let held = |packages: &repo::Packages| {
            (packages.iter())
                .flat_map(|(name, releases)| {
                    (releases.iter())
                        .filter(|r| r.arch == arch)
                        .map(move |r| (name.clone(), r.clone()))
                })
                .collect::<Vec<_>>()
        };
        if let Some(catalog) = &pointer.catalog {
            for (name, release) in held(&trees.read_legacy_tree(catalog)?) {
                if let Some(identities) = &mut published.catalog
                    && !matches!(release.standing, Standing::Deleted(_))
                {
                    identities.insert(
                        (name.clone(), release.version.clone()),
                        release.identity.clone(),
                    );
                }
                match release.standing {
                    Standing::Current => {}
                    Standing::Yanked => {
                        published
                            .yanked
                            .insert(format!("{name}@{}", release.version));
                    }
                    Standing::Deleted(reason) => {
                        published.deleted.push((name, release.version, reason));
                    }
                }
            }
        }
        for index in wanted {
            let Ok((_, top)) = pointer.index_of(index) else {
                continue;
            };
            let versions = (held(&trees.read_legacy_tree(top)?).into_iter())
                .map(|(name, release)| format!("{name}@{}", release.version))
                .collect();
            published.indexes.insert(index.clone(), versions);
        }
        Ok(published)
    }
}

impl Index {
    /// The packages for `arch` in `sources`: every one a directory holds,
    /// and of a repository served over HTTP, those `roots` can reach, read
    /// a name at a time.
    fn read(
        sources: &[Source],
        arch: &str,
        wanted: &BTreeSet<String>,
        signed: &BTreeMap<PathBuf, repo::Trust>,
        roots: &[Wanted],
    ) -> Result<Self> {
        if sources.is_empty() {
            bail!("no artifact directory to choose packages from");
        }
        let mut names: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
        let mut deleted: BTreeMap<String, Vec<(String, Source, String)>> = BTreeMap::new();
        let mut found = BTreeSet::new();
        let mut remotes = Vec::new();
        for source in sources {
            let trust = match source {
                Source::Dir(dir) => signed.get(dir),
                Source::Remote(_) => {
                    remotes.push(Served::read(source, wanted, &mut found)?);
                    continue;
                }
            };
            let published = Published::read(source, arch, wanted, trust)?;
            found.extend(published.indexes.keys().cloned());
            for (name, version, reason) in &published.deleted {
                deleted.entry(name.clone()).or_default().push((
                    version.clone(),
                    source.clone(),
                    reason.clone(),
                ));
            }
            for (id, identity) in held(source, arch, &published)? {
                let name = id.name();
                let held = names.entry(name.to_owned()).or_default();
                if let Some(first) = held.iter().find(|c| c.id == id) {
                    if first.identity != identity {
                        bail!(
                            "{id} is {} in {}, but {identity} in {source}",
                            first.identity,
                            first.source,
                        );
                    }
                    continue;
                }
                let metadata = read_metadata(source, &id, &identity)?;
                let short = format!("{name}@{}", id.version());
                held.push(Candidate {
                    yanked: published.yanked.contains(&short),
                    indexes: (published.indexes.iter())
                        .filter(|(_, held)| held.contains(&short))
                        .map(|(index, _)| index.clone())
                        .collect(),
                    id,
                    identity,
                    source: source.clone(),
                    metadata,
                });
            }
        }
        if !remotes.is_empty() {
            reach(&remotes, arch, roots, &mut names, &mut deleted)?;
        }
        if let Some(missing) = wanted.difference(&found).next() {
            bail!("#{missing}: no repository among the artifact directories publishes it");
        }
        for candidates in names.values_mut() {
            candidates.sort_by(|a, b| {
                b.id.parsed_version()
                    .cmp(a.id.parsed_version())
                    .then_with(|| a.id.version().cmp(b.id.version()))
            });
        }
        Ok(Self { names, deleted })
    }

    /// Why a version of `name` that `need` asks for isn't there, if a
    /// repository deleted one it would have satisfied.
    fn tombstone(&self, name: &str, need: &Need) -> Option<String> {
        let deleted = self.deleted.get(name)?;
        let (version, source, reason) = deleted.iter().find(|(version, _, _)| {
            Version::parse(version).is_ok_and(|version| need.constraint.matches(&version))
        })?;
        Some(format!(
            "; {name}@{version} was deleted from {source}: {reason}"
        ))
    }

    fn get(&self, name: &str) -> &[Candidate] {
        self.names.get(name).map_or(&[], Vec::as_slice)
    }
}

/// A name's candidates, and each version of it deleted, with why.
type Versions = (Vec<Candidate>, Vec<(String, String)>);

/// A repository served over HTTP, from which only the names resolving can
/// reach are read: a name's versions from its node in the catalog, each
/// version's metadata from its summary, and a capability's providers from
/// the catalog's provides tree. No manifest is read.
struct Served<'a> {
    source: &'a Source,
    trees: repo::Trees,
    pointer: repo::Pointer,
    /// The index or channel asked for each that it publishes, and the top
    /// node of the index it is or follows.
    indexes: Vec<(String, Identity)>,
}

impl<'a> Served<'a> {
    fn read(
        source: &'a Source,
        wanted: &BTreeSet<String>,
        found: &mut BTreeSet<String>,
    ) -> Result<Self> {
        let Some(pointer) = source.pointer(None)? else {
            bail!("{source}: not published as a repository, which fetching over HTTP needs");
        };
        let mut indexes = Vec::new();
        for index in wanted {
            if let Ok((_, top)) = pointer.index_of(index) {
                indexes.push((index.clone(), top.clone()));
                found.insert(index.clone());
            }
        }
        Ok(Self {
            source,
            trees: source.trees(),
            pointer,
            indexes,
        })
    }

    /// The versions of `name` for `arch` it publishes, and those it
    /// deleted, with why.
    fn name(&self, name: &str, arch: &str) -> Result<Versions> {
        let (mut candidates, mut deleted) = (Vec::new(), Vec::new());
        let Some(catalog) = &self.pointer.catalog else {
            return Ok((candidates, deleted));
        };
        let mut in_indexes = Vec::new();
        for (index, top) in &self.indexes {
            let versions: BTreeSet<String> = (self.trees.lookup(top, name)?.into_iter())
                .filter(|release| release.arch == arch)
                .map(|release| release.version)
                .collect();
            in_indexes.push((index, versions));
        }
        for release in self.trees.lookup(catalog, name)? {
            if release.arch != arch {
                continue;
            }
            let yanked = match release.standing {
                Standing::Current => false,
                Standing::Yanked => true,
                Standing::Deleted(reason) => {
                    deleted.push((release.version, reason));
                    continue;
                }
            };
            let id = PackageId::new(name, &release.version, arch)?;
            let summary = release.summary.expect("a version published has a summary");
            let manifest = parse_summary(self.trees.read(&summary)?.as_bytes())
                .with_context(|| format!("{id}: summary {summary} in {}", self.source))?;
            if manifest.id != id {
                bail!(
                    "{id}: its summary {summary} in {} is {}'s",
                    self.source,
                    manifest.id
                );
            }
            candidates.push(Candidate {
                yanked,
                indexes: (in_indexes.iter())
                    .filter(|(_, versions)| versions.contains(&release.version))
                    .map(|(index, _)| (*index).clone())
                    .collect(),
                id,
                identity: release.identity,
                source: self.source.clone(),
                metadata: manifest.metadata,
            });
        }
        Ok((candidates, deleted))
    }

    /// The names of the packages with a version for `arch` that provides
    /// `capability`.
    fn providers(&self, capability: &str, arch: &str) -> Result<Vec<String>> {
        if self.pointer.catalog.is_none() {
            return Ok(Vec::new());
        }
        let Some(top) = &self.pointer.provides else {
            bail!(
                "{}: published without a provides tree, which resolving a capability needs; \
                 publish it again with ppk repo update",
                self.source
            );
        };
        Ok((self.trees.lookup_providers(top, capability)?.into_iter())
            .filter(|provider| provider.arch == arch)
            .map(|provider| provider.name)
            .collect())
    }
}

/// Read into `names` and `deleted` every name of `remotes` that `roots`
/// reach: each package asked for by name, each package that provides a
/// capability asked for, and, from any candidate of a name read, each
/// package it depends on and each capability it requires. Every version
/// of a name is read, so the index holds what resolving would choose
/// among, whatever it chooses.
fn reach(
    remotes: &[Served],
    arch: &str,
    roots: &[Wanted],
    names: &mut BTreeMap<String, Vec<Candidate>>,
    deleted: &mut BTreeMap<String, Vec<(String, Source, String)>>,
) -> Result<()> {
    let mut queue: VecDeque<Wanted> = roots.iter().cloned().collect();
    let mut seen = BTreeSet::new();
    while let Some(wanted) = queue.pop_front() {
        if !seen.insert(wanted.clone()) {
            continue;
        }
        let name = match wanted {
            Wanted::Package(name) => name,
            Wanted::Capability(capability) => {
                for remote in remotes {
                    for name in remote.providers(&capability, arch)? {
                        queue.push_back(Wanted::Package(name));
                    }
                }
                // A directory's providers have their needs followed too.
                for (name, candidates) in names.iter() {
                    if candidates.iter().any(|c| c.provides(&capability).is_some()) {
                        queue.push_back(Wanted::Package(name.clone()));
                    }
                }
                continue;
            }
        };
        for remote in remotes {
            let (candidates, gone) = remote.name(&name, arch)?;
            for (version, reason) in gone {
                deleted.entry(name.clone()).or_default().push((
                    version,
                    remote.source.clone(),
                    reason,
                ));
            }
            let held = names.entry(name.clone()).or_default();
            for candidate in candidates {
                if let Some(first) = held.iter().find(|c| c.id == candidate.id) {
                    if first.identity != candidate.identity {
                        bail!(
                            "{} is {} in {}, but {} in {}",
                            candidate.id,
                            first.identity,
                            first.source,
                            candidate.identity,
                            candidate.source,
                        );
                    }
                    continue;
                }
                held.push(candidate);
            }
        }
        for candidate in names.get(&name).into_iter().flatten() {
            let needs = (candidate.metadata.dependencies.keys())
                .map(|name| Wanted::Package(name.clone()))
                .chain(
                    (candidate.metadata.requires.keys())
                        .map(|capability| Wanted::Capability(capability.clone())),
                );
            queue.extend(needs.filter(|need| !seen.contains(need)));
        }
    }
    Ok(())
}

/// The packages for `arch` that the artifact directory `source` holds, and
/// the identity of each: its refs, those its signed catalog lists if it is
/// signed.
fn held(source: &Source, arch: &str, published: &Published) -> Result<Vec<(PackageId, Identity)>> {
    let Source::Dir(dir) = source else {
        unreachable!("only a directory's packages are listed");
    };
    let layout = ArtifactDir::new(dir);
    let mut held = Vec::new();
    for name in package_names(dir)? {
        for version in store::versions(&name, arch, dir)? {
            let id = PackageId::new(&name, &version, arch)?;
            let Some(identity) = store::read_ref(&layout.ref_path(&id))? else {
                continue;
            };
            if let Some(catalog) = &published.catalog {
                match catalog.get(&(name.clone(), version.clone())) {
                    // Not published: a ref the signature doesn't vouch for.
                    None => continue,
                    Some(listed) if *listed != identity => bail!(
                        "{id}: its ref in {} names {identity}, but the repository's signed \
                         catalog lists {listed}",
                        dir.display()
                    ),
                    Some(_) => {}
                }
            }
            held.push((id, identity));
        }
    }
    Ok(held)
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
fn read_metadata(source: &Source, id: &PackageId, identity: &Identity) -> Result<Metadata> {
    let bytes = source.manifest(identity)?;
    let place = source.manifest_place(identity);
    let manifest = parse_manifest(&bytes).with_context(|| place.clone())?;
    if manifest.id != *id {
        bail!("{place}: is {}, but its ref says {id}", manifest.id);
    }
    Ok(manifest.metadata)
}

/// A request, dependency or requirement to settle.
#[derive(Clone, Debug)]
struct Need {
    wanted: Wanted,
    constraint: Constraint,
    /// The index it must be settled from: the request's, or for a
    /// dependency or requirement, the index of the request that chose the
    /// package that needs it.
    index: Option<String>,
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
        write!(f, "{name}")?;
        if let Some(index) = &self.0.index {
            write!(f, "#{index}")?;
        }
        match self.0.constraint.is_any() {
            true => Ok(()),
            false => write!(f, " {}", self.0.constraint),
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
    sources: &'a [Source],
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
        let tombstone = self.index.tombstone(name, &need).unwrap_or_default();
        if candidates.is_empty() {
            bail!(
                "{need}, but no artifact directory holds {name} for {}: {}{tombstone}",
                self.arch,
                self.dirs()
            );
        }
        let Some(index) = self.newest(candidates, &need)? else {
            let versions: Vec<&str> = candidates.iter().map(|c| c.id.version()).collect();
            let place = match &need.index {
                Some(index) => format!(" in #{index}"),
                None => String::new(),
            };
            bail!(
                "{need}, but no version{place} satisfies it; there are {}{tombstone}",
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
                other.source
            );
        }
        Ok(Some(index))
    }

    fn choose(&mut self, candidate: &'a Candidate, index: usize, need: Need) {
        let by = Some(candidate.id.clone());
        // What a package chosen from an index needs comes from that index.
        for (name, constraint) in &candidate.metadata.dependencies {
            self.push(Need {
                wanted: Wanted::Package(name.clone()),
                constraint: constraint.clone(),
                index: need.index.clone(),
                by: by.clone(),
            });
        }
        for (capability, constraint) in &candidate.metadata.requires {
            self.push(Need {
                wanted: Wanted::Capability(capability.clone()),
                constraint: constraint.clone(),
                index: need.index.clone(),
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

    /// The sources, for an error.
    fn dirs(&self) -> String {
        let dirs: Vec<String> = self.sources.iter().map(ToString::to_string).collect();
        dirs.join(", ")
    }
}
