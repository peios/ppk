//! The packages and identity of a composed `/usr`, readable without a store.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::closure::render_closure_manifest;
use crate::manifest::toml_string;
use crate::{Identity, PackageId};

/// The path reserved for the inventory, relative to `/usr`.
pub(crate) const INVENTORY: &str = "inventory";

/// The identity and packages of a composed `/usr`.
#[derive(Debug)]
pub struct Inventory {
    /// The hash of the closure manifest, recomputed when reading.
    pub identity: Identity,
    /// Package identities and full IDs, sorted by identity without duplicates.
    pub packages: Vec<(Identity, PackageId)>,
}

impl Inventory {
    pub(crate) fn new(packages: Vec<(Identity, PackageId)>) -> Self {
        let identities: Vec<_> = packages
            .iter()
            .map(|(identity, _)| identity.clone())
            .collect();
        Self {
            identity: Identity::of(render_closure_manifest(&identities).as_bytes()),
            packages,
        }
    }

    fn render(&self) -> String {
        let mut out = format!("closure = \"{}\"\n\n[packages]\n", self.identity);
        for (identity, id) in &self.packages {
            out.push_str(&format!(
                "\"{identity}\" = {}\n",
                toml_string(&id.to_string())
            ));
        }
        out
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

fn parse(text: &str) -> Result<Inventory> {
    let value: toml::Table = toml::from_str(text)?;
    let recorded: Identity = value
        .get("closure")
        .and_then(toml::Value::as_str)
        .context("missing closure identity")?
        .parse()?;
    let packages = value
        .get("packages")
        .and_then(toml::Value::as_table)
        .context("missing packages table")?;
    if value.len() != 2 || packages.is_empty() {
        bail!("expected a closure identity and a nonempty packages table");
    }
    let mut ids = BTreeSet::new();
    let mut entries = Vec::with_capacity(packages.len());
    for (identity, id) in packages {
        let identity: Identity = identity.parse()?;
        let spelling = id.as_str().context("a package ID must be a string")?;
        let id: PackageId = spelling.parse()?;
        if id.to_string() != spelling || !ids.insert(spelling) {
            bail!("package IDs must be full and unique");
        }
        entries.push((identity, id));
    }
    entries.sort_by(|a, b| a.0.hex().cmp(b.0.hex()));
    let inventory = Inventory::new(entries);
    if inventory.identity != recorded {
        bail!("closure identity does not match its packages");
    }
    if inventory.render() != text {
        bail!("inventory is not in canonical identity order and format");
    }
    Ok(inventory)
}
