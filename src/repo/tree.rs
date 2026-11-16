//! Package trees: a catalog of every version a repository has published,
//! or an index of one version of each package for each architecture, as a
//! tree of small files named by the hash of their content.
//!
//! A package's node lists its versions, one per line: the version, the
//! architecture, its manifest's identity and its summary's,
//!
//! ```text
//! 8.21.0-1 x86_64 sha256:… sha256:… yanked
//! 8.22.0-1 x86_64 sha256:… sha256:…
//! ```
//!
//! with `yanked`, or `deleted` and a reason, after a version taken back. A
//! version's summary is its manifest's head, everything but its files, kept
//! as a node of its own, so that resolving reads it and not the manifest. A
//! bucket's node lists the packages whose names fall in it, each with its
//! node's hash; the top node lists the buckets, each with its node's hash. A
//! name falls in the bucket of the first two hex digits of its SHA-256
//! hash, which can never change, since it is how a name is found. Every
//! node's lines are sorted and end in a newline, so one content is always
//! one file. Looking up a name reads three small files however many the
//! tree holds, and changing one package changes three nodes and leaves
//! every other one, which a mirror can cache for ever, as it is.
//!
//! A catalog's **provides tree** is bucketed the same way, by capability:
//! a capability's node lists every version, of every package, that provides
//! it, and at what version, `-` for none:
//!
//! ```text
//! org.gnu.glibc 2.44-1 x86_64 2.44
//! se.curl.curl 8.22.0-1 x86_64 -
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::id::check_name;
use crate::metadata::check_capability;
use crate::remote::Remote;
use crate::store;
use crate::{ALGORITHM, Identity, Version};

/// Where a repository keeps its tree nodes, by hash.
const TREES: &str = "trees";

/// How a version stands in a catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Published, and chosen as any other.
    Current,
    /// Taken back: a new resolution doesn't choose it, but a request for
    /// exactly it still does.
    Yanked,
    /// Taken away, for the reason given: its manifest and files are gone.
    Deleted(String),
}

/// One version of a package in a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub arch: String,
    /// Its manifest's identity.
    pub identity: Identity,
    /// The identity of its summary, its manifest's head, a tree node of its
    /// own. Only a deleted version, whose manifest is gone, can lack one.
    pub summary: Option<Identity>,
    pub standing: Standing,
}

impl Release {
    fn render(&self) -> String {
        let mut line = format!("{} {} {}", self.version, self.arch, self.identity);
        if let Some(summary) = &self.summary {
            line.push(' ');
            line.push_str(&summary.to_string());
        }
        match &self.standing {
            Standing::Current => {}
            Standing::Yanked => line.push_str(" yanked"),
            Standing::Deleted(reason) => {
                line.push_str(" deleted ");
                line.push_str(reason);
            }
        }
        line
    }

    /// Parse a package node's line. With `legacy`, a version may lack its
    /// summary, as every version did before summaries existed.
    fn parse(line: &str, legacy: bool) -> Result<Self> {
        let mut words = line.splitn(3, ' ');
        let (Some(version), Some(arch), Some(mut rest)) =
            (words.next(), words.next(), words.next())
        else {
            bail!("expected a version, an architecture and an identity");
        };
        Version::parse(version)?;
        // The identities are told from the standing by their prefix.
        let mut identities = Vec::new();
        while identities.len() < 2 && rest.starts_with(ALGORITHM) {
            let (identity, after) = rest.split_once(' ').unwrap_or((rest, ""));
            identities.push(identity.parse::<Identity>()?);
            rest = after;
        }
        let mut words = rest.splitn(2, ' ');
        let standing = match (words.next().filter(|word| !word.is_empty()), words.next()) {
            (None, None) => Standing::Current,
            (Some("yanked"), None) => Standing::Yanked,
            (Some("deleted"), Some(reason)) if !reason.is_empty() => {
                Standing::Deleted(reason.to_owned())
            }
            _ => bail!("expected yanked, or deleted and a reason"),
        };
        let mut identities = identities.into_iter();
        let Some(identity) = identities.next() else {
            bail!("expected a manifest's identity");
        };
        let summary = identities.next();
        if summary.is_none() && !legacy && !matches!(standing, Standing::Deleted(_)) {
            bail!("expected its summary's identity after its manifest's");
        }
        Ok(Self {
            version: version.to_owned(),
            arch: arch.to_owned(),
            identity,
            summary,
            standing,
        })
    }
}

/// A version of a package that provides a capability, in a provides tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    /// The package's name.
    pub name: String,
    pub version: String,
    pub arch: String,
    /// The version it provides the capability at, if any.
    pub provided: Option<String>,
}

impl Provider {
    fn render(&self) -> String {
        let provided = self.provided.as_deref().unwrap_or("-");
        format!("{} {} {} {provided}", self.name, self.version, self.arch)
    }

    fn parse(line: &str) -> Result<Self> {
        let words: Vec<&str> = line.split(' ').collect();
        let [name, version, arch, provided] = words.as_slice() else {
            bail!("expected a name, a version, an architecture and a version provided");
        };
        check_name(name)?;
        Version::parse(version)?;
        let provided = match *provided {
            "-" => None,
            // As a manifest's `provides` table gives it, which needs no
            // revision.
            provided => {
                Version::parse_relaxed(provided)?;
                Some(provided.to_owned())
            }
        };
        Ok(Self {
            name: (*name).to_owned(),
            version: (*version).to_owned(),
            arch: (*arch).to_owned(),
            provided,
        })
    }
}

/// Every capability in a provides tree, with the versions that provide it.
pub type Providers = BTreeMap<String, Vec<Provider>>;

/// Every package in a tree, by name, each with its versions.
pub type Packages = BTreeMap<String, Vec<Release>>;

/// The tree nodes of a repository.
pub struct Trees {
    dir: PathBuf,
    /// The repository, if it is served over HTTP, not a directory.
    remote: Option<Remote>,
}

impl Trees {
    /// The tree nodes of the repository at `repo`.
    pub fn new(repo: &Path) -> Self {
        Self {
            dir: repo.join(TREES).join(ALGORITHM),
            remote: None,
        }
    }

    /// The tree nodes of the repository `remote` serves, which can be read
    /// but not written.
    pub(crate) fn remote(remote: &Remote) -> Self {
        Self {
            dir: PathBuf::new(),
            remote: Some(remote.clone()),
        }
    }

    fn path(&self, identity: &Identity) -> PathBuf {
        self.dir.join(identity.hex())
    }

    /// The node `identity`, checked against its hash.
    pub(crate) fn read(&self, identity: &Identity) -> Result<String> {
        if let Some(remote) = &self.remote {
            let bytes = remote.object(TREES, identity)?;
            return String::from_utf8(bytes).with_context(|| format!("tree node {identity}"));
        }
        let path = self.path(identity);
        let bytes = fs::read(&path).with_context(|| format!("{}", path.display()))?;
        if Identity::of(&bytes) != *identity {
            bail!("{}: does not match its hash", path.display());
        }
        String::from_utf8(bytes).with_context(|| format!("{}", path.display()))
    }

    /// Write the node `text`, if it isn't there, and return its identity.
    pub(crate) fn write(&self, text: &str) -> Result<Identity> {
        if self.remote.is_some() {
            bail!("a repository served over HTTP can't be written");
        }
        let identity = Identity::of(text.as_bytes());
        let path = self.path(&identity);
        if !path.exists() {
            store::create_dirs(&self.dir)?;
            if !store::write_new(&self.dir, &path, text.as_bytes())? {
                bail!("{}: does not match its hash", path.display());
            }
        }
        Ok(identity)
    }

    /// Write the tree of `packages`, returning its top node's identity.
    /// Nodes already there are left as they are.
    pub fn write_tree(&self, packages: &Packages) -> Result<Identity> {
        let mut leaves = BTreeMap::new();
        for (name, releases) in packages {
            if releases.is_empty() {
                continue;
            }
            check_name(name)?;
            let lines: Vec<String> = releases.iter().map(Release::render).collect();
            leaves.insert(name.as_str(), self.write_leaf(lines)?);
        }
        self.write_buckets(leaves)
    }

    /// Write the provides tree of `providers`, returning its top node's
    /// identity.
    pub fn write_provides(&self, providers: &Providers) -> Result<Identity> {
        let mut leaves = BTreeMap::new();
        for (capability, provided) in providers {
            if provided.is_empty() {
                continue;
            }
            check_capability(capability)?;
            let lines: Vec<String> = provided.iter().map(Provider::render).collect();
            leaves.insert(capability.as_str(), self.write_leaf(lines)?);
        }
        self.write_buckets(leaves)
    }

    /// Write a leaf node of `lines`, sorted and unique.
    fn write_leaf(&self, mut lines: Vec<String>) -> Result<Identity> {
        lines.sort();
        lines.dedup();
        self.write(&join(&lines))
    }

    /// Write the buckets and top node of a tree whose leaves are `leaves`,
    /// by key, returning the top node's identity.
    fn write_buckets(&self, leaves: BTreeMap<&str, Identity>) -> Result<Identity> {
        let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (key, node) in leaves {
            buckets
                .entry(bucket(key))
                .or_default()
                .push(format!("{key} {node}"));
        }
        let mut top = Vec::with_capacity(buckets.len());
        for (bucket, mut lines) in buckets {
            lines.sort();
            let node = self.write(&join(&lines))?;
            top.push(format!("{bucket} {node}"));
        }
        self.write(&join(&top))
    }

    /// Every package in the tree whose top node is `top`.
    pub fn read_tree(&self, top: &Identity) -> Result<Packages> {
        self.read_tree_as(top, false)
    }

    /// Every package in the tree whose top node is `top`, written before
    /// summaries existed or since, for what needs no summary: publishing
    /// again, and reading a directory, whose refs and manifests are there.
    pub(crate) fn read_legacy_tree(&self, top: &Identity) -> Result<Packages> {
        self.read_tree_as(top, true)
    }

    fn read_tree_as(&self, top: &Identity, legacy: bool) -> Result<Packages> {
        let mut packages = Packages::new();
        for (bucket, node) in self.read_pairs(top)? {
            for (name, node) in self.read_pairs(&node.parse()?)? {
                if self::bucket(&name) != bucket {
                    bail!("{name}: in bucket {bucket}, not its own");
                }
                packages.insert(name, self.read_releases(&node.parse()?, legacy)?);
            }
        }
        Ok(packages)
    }

    /// The versions of the package `name` in the tree whose top node is
    /// `top`, none if it isn't there. Reads three nodes at most.
    pub fn lookup(&self, top: &Identity, name: &str) -> Result<Vec<Release>> {
        match self.leaf(top, name)? {
            Some(node) => self.read_releases(&node, false),
            None => Ok(Vec::new()),
        }
    }

    /// The versions that provide `capability`, in the provides tree whose
    /// top node is `top`, none if none do. Reads three nodes at most.
    pub fn lookup_providers(&self, top: &Identity, capability: &str) -> Result<Vec<Provider>> {
        let Some(node) = self.leaf(top, capability)? else {
            return Ok(Vec::new());
        };
        let text = self.read(&node)?;
        let lines = lines(&text).with_context(|| format!("tree node {node}"))?;
        lines
            .into_iter()
            .map(|line| {
                Provider::parse(line).with_context(|| format!("tree node {node}: {line:?}"))
            })
            .collect()
    }

    /// The leaf node of `key` in the tree whose top node is `top`, if it
    /// has one.
    fn leaf(&self, top: &Identity, key: &str) -> Result<Option<Identity>> {
        let wanted = bucket(key);
        let Some((_, node)) = self
            .read_pairs(top)?
            .into_iter()
            .find(|(b, _)| *b == wanted)
        else {
            return Ok(None);
        };
        let Some((_, node)) =
            (self.read_pairs(&node.parse()?)?.into_iter()).find(|(k, _)| k == key)
        else {
            return Ok(None);
        };
        node.parse().map(Some)
    }

    fn read_pairs(&self, node: &Identity) -> Result<Vec<(String, String)>> {
        let text = self.read(node)?;
        let lines = lines(&text).with_context(|| format!("tree node {node}"))?;
        lines
            .into_iter()
            .map(|line| {
                let (key, value) = line
                    .split_once(' ')
                    .with_context(|| format!("tree node {node}: {line:?}"))?;
                Ok((key.to_owned(), value.to_owned()))
            })
            .collect()
    }

    fn read_releases(&self, node: &Identity, legacy: bool) -> Result<Vec<Release>> {
        let text = self.read(node)?;
        let lines = lines(&text).with_context(|| format!("tree node {node}"))?;
        lines
            .into_iter()
            .map(|line| {
                Release::parse(line, legacy).with_context(|| format!("tree node {node}: {line:?}"))
            })
            .collect()
    }
}

/// The bucket a package name or capability falls in: the first two hex
/// digits of the SHA-256 hash of the name.
fn bucket(name: &str) -> String {
    Identity::of(name.as_bytes()).hex()[..2].to_owned()
}

fn join(lines: &[String]) -> String {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// The lines of a node, which must be sorted and unique, each ending in a
/// newline.
fn lines(text: &str) -> Result<Vec<&str>> {
    let Some(body) = text.strip_suffix('\n') else {
        bail!("does not end in a newline");
    };
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.windows(2).any(|pair| pair[0] >= pair[1]) || lines.contains(&"") {
        bail!("its lines are not sorted and unique");
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str, standing: Standing) -> Release {
        Release {
            version: version.to_owned(),
            arch: "x86_64".to_owned(),
            identity: Identity::of(version.as_bytes()),
            summary: Some(Identity::of(format!("summary {version}").as_bytes())),
            standing,
        }
    }

    fn packages(entries: &[(&str, Vec<Release>)]) -> Packages {
        entries
            .iter()
            .map(|(name, releases)| ((*name).to_owned(), releases.clone()))
            .collect()
    }

    #[test]
    fn a_line_has_its_summary_unless_its_version_was_deleted() {
        let (m, s) = (Identity::of(b"manifest"), Identity::of(b"summary"));
        for (line, legacy) in [
            (format!("1.0-1 x86_64 {m} {s}"), false),
            (format!("1.0-1 x86_64 {m} {s} yanked"), false),
            (
                format!("1.0-1 x86_64 {m} {s} deleted legal takedown"),
                false,
            ),
            (format!("1.0-1 x86_64 {m} deleted legal takedown"), false),
            (format!("1.0-1 x86_64 {m} yanked"), true),
        ] {
            let release = Release::parse(&line, legacy).unwrap();
            assert_eq!(release.render(), line);
        }
        for line in [
            format!("1.0-1 x86_64 {m}"),
            format!("1.0-1 x86_64 {m} yanked"),
            format!("1.0-1 x86_64 {m} {s} {m}"),
            format!("1.0-1 x86_64 {m} {s} retired"),
            format!("1.0-1 x86_64 {m} {s}  yanked"),
            "1.0-1 x86_64 yanked".to_owned(),
        ] {
            assert!(Release::parse(&line, false).is_err(), "{line}");
        }
    }

    #[test]
    fn a_provider_line_reads_back() {
        for line in [
            "org.gnu.glibc 2.44-1 x86_64 2.44-1",
            "org.git.kernel.dash 0.5.13.5-1 x86_64 1",
            "se.curl.curl 8.22.0-1 x86_64 -",
        ] {
            assert_eq!(Provider::parse(line).unwrap().render(), line);
        }
        for line in [
            "se.curl.curl 8.22.0-1 x86_64",
            "curl 8.22.0-1 x86_64 -",
            "se.curl.curl 8.22.0-1 x86_64 - extra",
        ] {
            assert!(Provider::parse(line).is_err(), "{line}");
        }
    }

    #[test]
    fn a_tree_reads_back_and_looks_up_a_name_in_three_nodes() {
        let repo = tempfile::tempdir().unwrap();
        let trees = Trees::new(repo.path());
        let tree = packages(&[
            (
                "se.curl.curl",
                vec![
                    release("8.22.0-1", Standing::Current),
                    release("8.21.0-1", Standing::Yanked),
                ],
            ),
            (
                "org.gnu.bash",
                vec![release("5.3-1", Standing::Deleted("legal takedown".into()))],
            ),
        ]);
        let top = trees.write_tree(&tree).unwrap();
        let mut sorted = tree.clone();
        for releases in sorted.values_mut() {
            releases.sort_by_key(Release::render);
        }
        assert_eq!(trees.read_tree(&top).unwrap(), sorted);
        assert_eq!(
            trees.lookup(&top, "org.gnu.bash").unwrap(),
            sorted["org.gnu.bash"]
        );
        assert!(trees.lookup(&top, "org.gnu.sed").unwrap().is_empty());
        // The same packages are the same tree.
        assert_eq!(trees.write_tree(&sorted).unwrap(), top);
    }

    #[test]
    fn changing_one_package_writes_three_nodes() {
        let repo = tempfile::tempdir().unwrap();
        let trees = Trees::new(repo.path());
        let mut tree: Packages = (0..200)
            .map(|n| {
                (
                    format!("dev.peios.p{n}"),
                    vec![release("1.0-1", Standing::Current)],
                )
            })
            .collect();
        trees.write_tree(&tree).unwrap();
        let before = fs::read_dir(&trees.dir).unwrap().count();
        tree.get_mut("dev.peios.p7")
            .unwrap()
            .push(release("2.0-1", Standing::Current));
        trees.write_tree(&tree).unwrap();
        assert_eq!(fs::read_dir(&trees.dir).unwrap().count(), before + 3);
    }

    #[test]
    fn a_node_that_was_changed_is_refused() {
        let repo = tempfile::tempdir().unwrap();
        let trees = Trees::new(repo.path());
        let top = trees
            .write_tree(&packages(&[(
                "se.curl.curl",
                vec![release("8.22.0-1", Standing::Current)],
            )]))
            .unwrap();
        let path = trees.path(&top);
        let mut perms = fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&path, perms).unwrap();
        fs::write(&path, "00 sha256:0\n").unwrap();
        let err = trees.read_tree(&top).unwrap_err();
        assert!(err.to_string().contains("does not match its hash"), "{err}");
    }
}
