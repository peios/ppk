//! Package identifiers: `<name>@<version>[?arch=<arch>]`.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};

use crate::Version;

/// The architecture a package is assumed to be for when its identifier does
/// not say: the one this build of ppk was compiled for, which for a native
/// build is the one it runs on.
pub const HOST_ARCH: &str = std::env::consts::ARCH;

/// Which package something is: its name, version and architecture.
///
/// The name has passed [`check_name`], the version is a complete
/// [`Version`], and the arch has passed [`check_component`], so each is safe
/// to use as a single path component.
///
/// Two identifiers are the same only if they are spelled the same: packages
/// are stored by their spelling, so `1.0~alpha-1` and `1.0~a-1` are two
/// packages, though as versions they are equal.
#[derive(Debug, Clone)]
pub struct PackageId {
    name: String,
    version: Version,
    arch: String,
}

impl PackageId {
    pub fn new(name: &str, version: &str, arch: &str) -> Result<Self> {
        check_name(name)?;
        let version = Version::parse(version).map_err(|err| {
            anyhow!(
                "invalid version '{version}': {err}; a package's version is \
                 [<epoch>:]<upstream>-<revision>, such as 1.0-1"
            )
        })?;
        check_component("arch", arch)?;
        Ok(Self {
            name: name.to_owned(),
            version,
            arch: arch.to_owned(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The version, as it is spelled.
    pub fn version(&self) -> &str {
        self.version.as_str()
    }

    /// The version, parsed, to compare with other versions.
    pub fn parsed_version(&self) -> &Version {
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

/// The same name, version and arch, each spelled the same.
impl PartialEq for PackageId {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.version.as_str() == other.version.as_str()
            && self.arch == other.arch
    }
}

impl Eq for PackageId {}

/// The canonical spelling, which always includes the arch.
impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}?arch={}", self.name, self.version, self.arch)
    }
}

/// The longest package name, from PSPU 5.3.
const NAME_MAX_BYTES: usize = 64;

/// The fewest labels a package name can have: a domain's two and a product.
const NAME_MIN_LABELS: usize = 3;

/// A package name is reverse-DNS: the domain of whoever owns the software,
/// backwards, then the product, as in `dev.peios.hello`, `org.gnu.gcc-c++`
/// and `io.github.eudev-project.eudev`. This takes PSPU 5.3's characters and
/// length and adds the structure, which is all a name can show: nothing here
/// knows whether the domain is the right one for the software.
///
/// Labels split on dots. Each starts with a lowercase letter or digit, ends
/// with one or with `+`, and holds no `--`. There are at least
/// [`NAME_MIN_LABELS`], and the first two are a real domain's: the first is
/// all letters, as a top-level domain is, and neither holds a `+`. A name is
/// therefore a safe path component, and an identifier splits unambiguously.
pub(crate) fn check_name(name: &str) -> Result<()> {
    let labels: Vec<&str> = name.split('.').collect();
    let valid = name.len() <= NAME_MAX_BYTES
        && labels.len() >= NAME_MIN_LABELS
        && labels.iter().copied().all(is_label)
        && labels[0].chars().all(|c| c.is_ascii_lowercase())
        && !labels[1].contains('+');
    if !valid {
        bail!(
            "invalid package name '{name}': use a reverse-DNS name such as dev.peios.hello, \
             of at most {NAME_MAX_BYTES} characters, with at least {NAME_MIN_LABELS} \
             dot-separated labels of lowercase ASCII letters, digits, - and +"
        );
    }
    Ok(())
}

fn is_label(label: &str) -> bool {
    label.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && !label.ends_with('-')
        && !label.contains("--")
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-".contains(c))
}

/// Arches become path components, so until their syntax is designed they are
/// limited to a conservative set that cannot escape the artifact directory:
/// ASCII letters, digits and `. _ + - ~`, starting with a letter or digit.
/// This also keeps `@`, `?`, `&` and `=` out of them, so an identifier always
/// splits unambiguously. A version's own grammar does the same for it: digits
/// and `:` in the epoch, and letters, digits and `. + - ~` after it, starting
/// with a letter or digit.
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
