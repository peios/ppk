//! Package trees: a catalog of every version a repository has published,
//! or an index of one version of each package for each architecture, as a
//! tree of small files named by the hash of their content.
//!
//! A package's node lists its versions, one per line:
//!
//! ```text
//! 8.21.0-1 x86_64 sha256:… yanked
//! 8.22.0-1 x86_64 sha256:…
//! ```
//!
//! with `yanked`, or `deleted` and a reason, after a version taken back. A
//! bucket's node lists the packages whose names fall in it, each with its
//! node's hash; the top node lists the buckets, each with its node's hash. A
//! name falls in the bucket of the first two hex digits of its SHA-256
//! hash, which can never change, since it is how a name is found. Every
//! node's lines are sorted and end in a newline, so one content is always
//! one file. Looking up a name reads three small files however many the
//! tree holds, and changing one package changes three nodes and leaves
//! every other one, which a mirror can cache for ever, as it is.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::id::check_name;
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
    pub standing: Standing,
}

impl Release {
    fn render(&self) -> String {
        let mut line = format!("{} {} {}", self.version, self.arch, self.identity);
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

    fn parse(line: &str) -> Result<Self> {
        let mut words = line.splitn(5, ' ');
        let (Some(version), Some(arch), Some(identity)) =
            (words.next(), words.next(), words.next())
        else {
            bail!("expected a version, an architecture and an identity");
        };
        Version::parse(version)?;
        let standing = match (words.next(), words.next()) {
            (None, _) => Standing::Current,
            (Some("yanked"), None) => Standing::Yanked,
            (Some("deleted"), Some(reason)) if !reason.is_empty() => {
                Standing::Deleted(reason.to_owned())
            }
            _ => bail!("expected yanked, or deleted and a reason"),
        };
        Ok(Self {
            version: version.to_owned(),
            arch: arch.to_owned(),
            identity: identity.parse()?,
            standing,
        })
    }
}

/// Every package in a tree, by name, each with its versions.
pub type Packages = BTreeMap<String, Vec<Release>>;

/// The tree nodes of the repository at `repo`.
pub struct Trees {
    dir: PathBuf,
}

impl Trees {
    pub fn new(repo: &Path) -> Self {
        Self {
            dir: repo.join(TREES).join(ALGORITHM),
        }
    }

    fn path(&self, identity: &Identity) -> PathBuf {
        self.dir.join(identity.hex())
    }

    /// The node `identity`, checked against its hash.
    fn read(&self, identity: &Identity) -> Result<String> {
        let path = self.path(identity);
        let bytes = fs::read(&path).with_context(|| format!("{}", path.display()))?;
        if Identity::of(&bytes) != *identity {
            bail!("{}: does not match its hash", path.display());
        }
        String::from_utf8(bytes).with_context(|| format!("{}", path.display()))
    }

    /// Write the node `text`, if it isn't there, and return its identity.
    fn write(&self, text: &str) -> Result<Identity> {
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
        let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, releases) in packages {
            if releases.is_empty() {
                continue;
            }
            check_name(name)?;
            let mut lines: Vec<String> = releases.iter().map(Release::render).collect();
            lines.sort();
            lines.dedup();
            let node = self.write(&join(&lines))?;
            buckets
                .entry(bucket(name))
                .or_default()
                .push(format!("{name} {node}"));
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
        let mut packages = Packages::new();
        for (bucket, node) in self.read_pairs(top)? {
            for (name, node) in self.read_pairs(&node.parse()?)? {
                if self::bucket(&name) != bucket {
                    bail!("{name}: in bucket {bucket}, not its own");
                }
                packages.insert(name, self.read_releases(&node.parse()?)?);
            }
        }
        Ok(packages)
    }

    /// The versions of the package `name` in the tree whose top node is
    /// `top`, none if it isn't there. Reads three nodes at most.
    pub fn lookup(&self, top: &Identity, name: &str) -> Result<Vec<Release>> {
        let wanted = bucket(name);
        let Some((_, node)) = self
            .read_pairs(top)?
            .into_iter()
            .find(|(b, _)| *b == wanted)
        else {
            return Ok(Vec::new());
        };
        let Some((_, node)) =
            (self.read_pairs(&node.parse()?)?.into_iter()).find(|(n, _)| n == name)
        else {
            return Ok(Vec::new());
        };
        self.read_releases(&node.parse()?)
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

    fn read_releases(&self, node: &Identity) -> Result<Vec<Release>> {
        let text = self.read(node)?;
        let lines = lines(&text).with_context(|| format!("tree node {node}"))?;
        lines
            .into_iter()
            .map(|line| Release::parse(line).with_context(|| format!("tree node {node}: {line:?}")))
            .collect()
    }
}

/// The bucket a package name falls in: the first two hex digits of the
/// SHA-256 hash of the name.
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
