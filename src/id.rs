//! Package identifiers: `<name>@<version>[?arch=<arch>]`.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};

/// The architecture a package is assumed to be for when its identifier does
/// not say: the one this build of ppk was compiled for, which for a native
/// build is the one it runs on.
pub const HOST_ARCH: &str = std::env::consts::ARCH;

/// Which package something is: its name, version and architecture.
///
/// Every field has passed [`check_component`], so each is safe to use as a
/// single path component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageId {
    name: String,
    version: String,
    arch: String,
}

impl PackageId {
    pub fn new(name: &str, version: &str, arch: &str) -> Result<Self> {
        check_component("package name", name)?;
        check_component("version", version)?;
        check_component("arch", arch)?;
        Ok(Self {
            name: name.to_owned(),
            version: version.to_owned(),
            arch: arch.to_owned(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn arch(&self) -> &str {
        &self.arch
    }
}

/// Parses `<name>@<version>`, optionally followed by `?arch=<arch>`. Without
/// a qualifier the arch is [`HOST_ARCH`]; arch-independent packages say
/// `?arch=noarch`. Unknown and repeated qualifiers are errors, so that a
/// misspelling is never silently ignored.
impl FromStr for PackageId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (package, query) = match s.split_once('?') {
            Some((package, query)) => (package, Some(query)),
            None => (s, None),
        };
        let Some((name, version)) = package.split_once('@') else {
            bail!("{s}: missing '@<version>'");
        };

        let mut arch = None;
        for qualifier in query.into_iter().flat_map(|query| query.split('&')) {
            let Some((key, value)) = qualifier.split_once('=') else {
                bail!("{s}: qualifier '{qualifier}' is not of the form <key>=<value>");
            };
            match key {
                "arch" if arch.is_some() => bail!("{s}: arch is given more than once"),
                "arch" => arch = Some(value),
                _ => bail!("{s}: unknown qualifier '{key}'"),
            }
        }

        Self::new(name, version, arch.unwrap_or(HOST_ARCH))
            .map_err(|err| anyhow::anyhow!("{s}: {err}"))
    }
}

/// The canonical spelling, which always includes the arch.
impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}?arch={}", self.name, self.version, self.arch)
    }
}

/// Names, versions and arches become path components, so until their syntax
/// is designed they are limited to a conservative set that cannot escape the
/// artifact directory: ASCII letters, digits and `. _ + - ~`, starting with a
/// letter or digit. This also keeps `@`, `?`, `&` and `=` out of them, so an
/// identifier always splits unambiguously.
fn check_component(what: &str, s: &str) -> Result<()> {
    let valid = s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-~".contains(c));
    if !valid {
        bail!(
            "invalid {what} '{s}': use ASCII letters, digits and . _ + - ~, \
             starting with a letter or digit"
        );
    }
    Ok(())
}
