//! The packages and identity of a composed `/usr`, readable without a store,
//! and why each package is there.
//!
//! The inventory is a line format. The first line names the closure; each
//! other line is one package, its identity, and a bracket for each thing
//! that wants it:
//!
//! ```text
//! closure sha256:9c41…
//! org.openssl.libssl@3.5.8-1?arch=x86_64 sha256:… [se.curl.curl libssl.so.3]
//! se.curl.curl@8.22.0-1?arch=x86_64 sha256:… [* >=8.22]
//! ```
//!
//! A bracket holds who wants the package: `*` for a request made of the
//! closure itself, or the name of a package in it. Then, if what was asked
//! for is a capability rather than the package by name, the capability.
//! Then, for a request from an index or channel, `#` and its name. Then the
//! constraint, one token per expression, such as `>=8.22`, none
//! for any version. A capability never starts with an operator and a
//! constraint token always does, so the two can't be confused.
//!
//! A package is wanted by every request and every dependency or requirement
//! of a package in the closure that it satisfies, so the brackets are a
//! function of the packages and the requests alone, however the closure was
//! reached. Lines are sorted by package ID and brackets by requester, `*`
//! first, then by capability, a package asked for by name first. The
//! closure's identity is the hash of every line after the first: the
//! closure is named by its requests as well as by its packages.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::id::check_name;
use crate::metadata::check_capability;
use crate::repo::check_ref_name;
use crate::resolve::{Request, Wanted};
use crate::{Constraint, Identity, Metadata, PackageId};

/// The path reserved for the inventory, relative to `/usr`.
pub(crate) const INVENTORY: &str = "inventory";

/// What the first line of an inventory starts with.
const CLOSURE_LINE: &str = "closure ";

/// The requester of a request made of the closure itself, rather than by a
/// package in it. No package can have this name.
pub const USER: &str = "*";

/// One thing that wants a package.
#[derive(Clone, Debug)]
pub struct Want {
    /// [`USER`], or the name of the package in the closure that wants it.
    pub requester: String,
    /// The capability asked for, if the package wasn't asked for by name.
    pub capability: Option<String>,
    /// The index or channel a request asked for it from.
    pub index: Option<String>,
    /// The versions that would do.
    pub constraint: Constraint,
}

impl Want {
    fn render(&self) -> String {
        let mut out = format!("[{}", self.requester);
        if let Some(capability) = &self.capability {
            out.push(' ');
            out.push_str(capability);
        }
        if let Some(index) = &self.index {
            out.push_str(" #");
            out.push_str(index);
        }
        for token in self.constraint.tokens() {
            out.push(' ');
            out.push_str(&token);
        }
        out.push(']');
        out
    }

    /// The order brackets are written in: by requester, `*` first, then by
    /// capability, none first, then by constraint.
    fn order(&self, other: &Self) -> Ordering {
        let key = |want: &Self| (want.requester != USER, want.requester.clone());
        key(self)
            .cmp(&key(other))
            .then_with(|| self.capability.cmp(&other.capability))
            .then_with(|| self.index.cmp(&other.index))
            .then_with(|| self.constraint.tokens().cmp(&other.constraint.tokens()))
    }
}

/// One package of an inventory.
#[derive(Clone, Debug)]
pub struct InventoryPackage {
    pub id: PackageId,
    /// Its manifest's identity.
    pub identity: Identity,
    /// What wants it, in the order the inventory writes them. Never empty.
    pub wanted_by: Vec<Want>,
}

/// The identity and packages of a composed `/usr`.
#[derive(Debug)]
pub struct Inventory {
    /// The hash of every line after the first.
    pub identity: Identity,
    /// Its packages, sorted by ID.
    pub packages: Vec<InventoryPackage>,
}

impl Inventory {
    /// The inventory of a closure of `packages`, each with its identity and
    /// metadata, made for `requests`.
    ///
    /// Every package must be wanted: by a request, or by a dependency or
    /// requirement of another package. A request that no package satisfies
    /// is an error, but a dependency or requirement need not be satisfied:
    /// a closure of exactly the packages given doesn't check what they need.
    pub(crate) fn of(
        packages: &[(Identity, PackageId, Metadata)],
        requests: &[Request],
    ) -> Result<Self> {
        let mut wanted: Vec<Vec<Want>> = vec![Vec::new(); packages.len()];
        for request in requests {
            let mut satisfied = false;
            for (index, (_, id, metadata)) in packages.iter().enumerate() {
                if let Some(mut want) =
                    wants(USER, &request.wanted, &request.constraint, id, metadata)
                {
                    want.index.clone_from(&request.index);
                    wanted[index].push(want);
                    satisfied = true;
                }
            }
            if !satisfied {
                bail!(
                    "{}: no package of the closure satisfies it",
                    describe(request)
                );
            }
        }
        for (_, requester, needs) in packages {
            let asked = (needs.dependencies.iter())
                .map(|(name, constraint)| (Wanted::Package(name.clone()), constraint))
                .chain(
                    (needs.requires.iter())
                        .map(|(name, constraint)| (Wanted::Capability(name.clone()), constraint)),
                );
            for (asked, constraint) in asked {
                for (index, (_, id, metadata)) in packages.iter().enumerate() {
                    if id.name() == requester.name() {
                        continue;
                    }
                    if let Some(want) = wants(requester.name(), &asked, constraint, id, metadata) {
                        wanted[index].push(want);
                    }
                }
            }
        }
        let mut entries = Vec::with_capacity(packages.len());
        for ((identity, id, _), mut wanted_by) in packages.iter().zip(wanted) {
            if wanted_by.is_empty() {
                bail!("{id}: in the closure, but nothing asks for it");
            }
            wanted_by.sort_by(Want::order);
            wanted_by.dedup_by(|a, b| a.order(b) == Ordering::Equal);
            entries.push(InventoryPackage {
                id: id.clone(),
                identity: identity.clone(),
                wanted_by,
            });
        }
        entries.sort_by_key(|entry| entry.id.to_string());
        if let Some(pair) = entries.windows(2).find(|pair| pair[0].id == pair[1].id) {
            bail!("{}: in the closure twice", pair[0].id);
        }
        Ok(Self::from_packages(entries))
    }

    fn from_packages(packages: Vec<InventoryPackage>) -> Self {
        Self {
            identity: Identity::of(render_body(&packages).as_bytes()),
            packages,
        }
    }

    /// Every line after the first, which the closure's identity is the hash
    /// of, and which its manifest beside its tree holds.
    pub(crate) fn body(&self) -> String {
        render_body(&self.packages)
    }

    fn render(&self) -> String {
        format!("{CLOSURE_LINE}{}\n{}", self.identity, self.body())
    }

    /// Write inside the unpublished tree. Unlike a stored blob, this file
    /// inherits the composed root's descriptor, as its directories do.
    pub(crate) fn write(&self, usr: &Path) -> Result<()> {
        let path = usr.join(INVENTORY);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o444)
            .open(&path)
            .with_context(|| format!("{}", path.display()))?;
        file.set_permissions(Permissions::from_mode(0o444))
            .and_then(|()| file.write_all(self.render().as_bytes()))
            .with_context(|| format!("{}", path.display()))
    }
}

fn render_body(packages: &[InventoryPackage]) -> String {
    let mut out = String::new();
    for package in packages {
        out.push_str(&format!("{} {}", package.id, package.identity));
        for want in &package.wanted_by {
            out.push(' ');
            out.push_str(&want.render());
        }
        out.push('\n');
    }
    out
}

/// How `requester` wants the package `id`, whose metadata is `metadata`, if
/// asking for `asked` at `constraint` is satisfied by it. A capability is
/// satisfied by the package's own name at its own version, or by a
/// `provides` entry at that entry's version; one provided with no version
/// satisfies any constraint (PSPU 5.21).
fn wants(
    requester: &str,
    asked: &Wanted,
    constraint: &Constraint,
    id: &PackageId,
    metadata: &Metadata,
) -> Option<Want> {
    let (capability, satisfied) = match asked {
        Wanted::Package(name) => (
            None,
            name == id.name() && constraint.matches(id.parsed_version()),
        ),
        Wanted::Capability(name) => (
            Some(name.clone()),
            match metadata.provides.get(name) {
                Some(Some(version)) => constraint.matches(version),
                Some(None) => true,
                None => name == id.name() && constraint.matches(id.parsed_version()),
            },
        ),
    };
    satisfied.then(|| Want {
        requester: requester.to_owned(),
        capability,
        index: None,
        constraint: constraint.clone(),
    })
}

fn describe(request: &Request) -> String {
    let name = match &request.wanted {
        Wanted::Package(name) | Wanted::Capability(name) => name,
    };
    let versions = request.versions();
    match versions.is_empty() {
        true => name.clone(),
        false if versions.starts_with('#') => format!("{name}{versions}"),
        false => format!("{name} {versions}"),
    }
}

/// Read and verify `usr/inventory`. Only a missing file returns `None`.
/// No store is needed: the inventory records each package's full ID.
pub fn read_inventory(usr: &Path) -> Result<Option<Inventory>> {
    let path = usr.join(INVENTORY);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("{}", path.display())),
    };
    parse(&text)
        .map(Some)
        .with_context(|| format!("{}: malformed inventory", path.display()))
}

/// Parse an inventory, checking that it is in the one form it is written
/// in and that its identity is the hash of its body.
pub fn parse(text: &str) -> Result<Inventory> {
    let Some((first, body)) = text.split_once('\n') else {
        bail!("no closure line");
    };
    let recorded: Identity = first
        .strip_prefix(CLOSURE_LINE)
        .context("the first line must name the closure")?
        .parse()?;
    let mut packages = Vec::new();
    for (number, line) in body.lines().enumerate() {
        let package = parse_line(line).with_context(|| format!("line {}", number + 2))?;
        packages.push(package);
    }
    if packages.is_empty() {
        bail!("no packages");
    }
    let ids: BTreeSet<String> = packages.iter().map(|p| p.id.to_string()).collect();
    if ids.len() != packages.len() {
        bail!("package IDs must be unique");
    }
    let inventory = Inventory::from_packages(packages);
    if inventory.identity != recorded {
        bail!("closure identity does not match its packages");
    }
    if inventory.render() != text {
        bail!("inventory is not in its canonical order and form");
    }
    Ok(inventory)
}

fn parse_line(line: &str) -> Result<InventoryPackage> {
    let (head, brackets) = line
        .split_once(" [")
        .context("nothing wants this package")?;
    let (id, identity) = head
        .split_once(' ')
        .context("expected an ID and an identity")?;
    let spelling = id;
    let id: PackageId = spelling.parse()?;
    if id.to_string() != spelling {
        bail!("{spelling}: package IDs must be full");
    }
    let identity: Identity = identity.parse()?;
    let brackets = brackets.strip_suffix(']').context("unterminated bracket")?;
    let mut wanted_by = Vec::new();
    for bracket in brackets.split("] [") {
        wanted_by.push(parse_want(bracket).with_context(|| format!("[{bracket}]"))?);
    }
    Ok(InventoryPackage {
        id,
        identity,
        wanted_by,
    })
}

fn parse_want(bracket: &str) -> Result<Want> {
    let mut tokens = bracket.split(' ');
    let requester = tokens.next().unwrap_or_default();
    if requester != USER {
        check_name(requester)?;
    }
    let mut rest: Vec<&str> = tokens.collect();
    let capability = match rest.first() {
        Some(token) if !token.starts_with(['=', '<', '>', '!', '#']) => {
            check_capability(token)?;
            Some(rest.remove(0).to_owned())
        }
        _ => None,
    };
    let index = match rest.first().and_then(|token| token.strip_prefix('#')) {
        Some(index) => {
            check_ref_name(index)?;
            let index = index.to_owned();
            rest.remove(0);
            Some(index)
        }
        None => None,
    };
    if index.is_some() && requester != USER {
        bail!("only a request of the closure itself names an index");
    }
    let constraint = Constraint::from_tokens(&rest)?;
    if constraint.tokens() != rest {
        bail!("constraint not in canonical form");
    }
    Ok(Want {
        requester: requester.to_owned(),
        capability,
        index,
        constraint,
    })
}
