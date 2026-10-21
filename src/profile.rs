//! Environment profiles: TOML files that say exactly what an environment
//! holds, so that a composed root or a `run` can be described once and
//! named, rather than spelt out in options each time.
//!
//! ```text
//! arch = "x86_64"
//! dirs = ["proc", "sys", "dev", "tmp"]
//!
//! [packages]
//! "net.busybox.busybox" = "1.37.0-1"
//!
//! [symlinks]
//! "bin" = ":bin"
//! ```
//!
//! `packages` maps each package's name to its exact version: a profile is
//! the resolved answer, so whatever chooses versions does so when writing
//! it, never when composing. Every package is for `arch`, the host's by
//! default. A symlink's value is what follows `<path>:` in `--symlink`, so
//! a leading `:` stands for `usr`. `dirs` are empty directories, and
//! `include_artifacts = true` gives a composed root its packages in an
//! artifact directory of its own, as `--include-artifacts` does. Every key
//! is optional, and any other is an error, so that a misspelt one cannot
//! silently leave something out.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use crate::{EnvDir, EnvSymlink, HOST_ARCH, PackageId};

/// What an environment holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    /// Its packages, at most one version of each name.
    pub packages: Vec<PackageId>,
    /// Symlinks a composed root holds besides its `usr`.
    pub symlinks: Vec<EnvSymlink>,
    /// Empty directories a composed root holds.
    pub dirs: Vec<EnvDir>,
    /// Whether a composed root holds its packages in an artifact directory
    /// of its own, as `--include-artifacts` gives it.
    pub include_artifacts: bool,
}

impl Profile {
    /// The profile in the file at `path`.
    pub fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
        Self::parse(&text).with_context(|| format!("{}", path.display()))
    }

    /// The profile `text` describes.
    pub fn parse(text: &str) -> Result<Self> {
        let doc: toml::Table =
            toml::from_str(text).map_err(|err| anyhow!("not valid TOML: {}", err.message()))?;
        for key in doc.keys() {
            if !["arch", "packages", "symlinks", "dirs", "include_artifacts"]
                .contains(&key.as_str())
            {
                bail!("unknown key {key:?}");
            }
        }
        let arch = match doc.get("arch") {
            Some(arch) => arch
                .as_str()
                .ok_or_else(|| anyhow!("arch is not a string"))?,
            None => HOST_ARCH,
        };

        let mut profile = Profile::default();
        for (name, version) in table(&doc, "packages")? {
            let version = version
                .as_str()
                .ok_or_else(|| anyhow!("packages: {name:?} has a version that is not a string"))?;
            let id = PackageId::new(name, version, arch).context("packages")?;
            profile.packages.push(id);
        }
        for (path, target) in table(&doc, "symlinks")? {
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

    /// Adds `packages` to the profile's. One it holds already is added
    /// once; another version of a package it holds is an error, since an
    /// environment holds one version of each.
    pub fn add_packages(&mut self, packages: impl IntoIterator<Item = PackageId>) -> Result<()> {
        for id in packages {
            match self.packages.iter().find(|held| held.name() == id.name()) {
                Some(held) if *held == id => {}
                Some(held) => bail!("{id}: the environment holds {held} already"),
                None => self.packages.push(id),
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
