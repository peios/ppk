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
/// The name has passed [`check_name`], and the version and arch
/// [`check_component`], so each is safe to use as a single path component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageId {
    name: String,
    version: String,
    arch: String,
}

impl PackageId {
    pub fn new(name: &str, version: &str, arch: &str) -> Result<Self> {
        check_name(name)?;
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
fn check_name(name: &str) -> Result<()> {
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

/// Versions and arches become path components, so until their syntax is
/// designed they are limited to a conservative set that cannot escape the
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
