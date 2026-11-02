//! What a manifest says about a package besides its files: what it depends
//! on, requires and provides, the links it claims, and descriptive fields.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};

use crate::id::check_name;
use crate::manifest::toml_string;
use crate::{Constraint, Version};

/// What a package's manifest records besides its identity and files. Every
/// part is optional, and a package with none of them has the manifest it
/// would have had before they existed.
///
/// ppk records and checks these. Composing creates the claimed links, and
/// [`resolve`](crate::resolve) follows the dependencies and requirements.
///
/// The maps are keyed by their names and paths, so they are sorted by bytes,
/// as the manifest lists them. Check a value with [`Metadata::check`];
/// [`pack_with`](crate::pack_with) does, and a manifest is checked when it is
/// read.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    /// A one-line description, in printable ASCII (PSPU 5.18).
    pub description: Option<String>,
    /// The upstream project's `http` or `https` URL.
    pub homepage: Option<String>,
    /// The package's license, conventionally an SPDX expression.
    pub license: Option<String>,
    /// Which kind of terms the license is.
    pub license_class: LicenseClass,
    /// The license of the source the package was built from, where it is
    /// not the package's own.
    pub source_license: Option<String>,
    /// Packages this one needs, by name, each with the versions that do.
    pub dependencies: BTreeMap<String, Constraint>,
    /// Capabilities this one needs, such as a soname or `sh`, each with the
    /// versions that do.
    pub requires: BTreeMap<String, Constraint>,
    /// Capabilities this package provides, each at a version, if it has one.
    pub provides: BTreeMap<String, Option<Version>>,
    /// Links this package provides outside its files: each absolute path
    /// with its link's target.
    pub claims: BTreeMap<String, String>,
}

/// Which kind of terms a package's license is (PSPU 5.18). A manifest says
/// nothing for [`LicenseClass::Unknown`], which is the default, because
/// saying nothing and saying `unknown` mean the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LicenseClass {
    #[default]
    Unknown,
    Free,
    Firmware,
    Proprietary,
}

impl LicenseClass {
    /// The class as it is written.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Free => "free",
            Self::Firmware => "firmware",
            Self::Proprietary => "proprietary",
        }
    }
}

impl FromStr for LicenseClass {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "unknown" => Self::Unknown,
            "free" => Self::Free,
            "firmware" => Self::Firmware,
            "proprietary" => Self::Proprietary,
            _ => bail!("invalid license class '{s}': use unknown, free, firmware or proprietary"),
        })
    }
}

impl fmt::Display for LicenseClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Metadata {
    /// Whether there is nothing to record.
    pub fn is_empty(&self) -> bool {
        self.render_fields().is_empty() && self.render_tables().is_empty()
    }

    /// Check every part against its rules, naming the first that breaks one:
    ///
    /// - `description` is printable ASCII (PSPU 5.18).
    /// - `homepage` is an `http` or `https` URL (PSPU 5.18).
    /// - `license` and `source_license` have no control characters.
    /// - None of these is empty: an absent field is how a manifest says there
    ///   is nothing.
    /// - A dependency's name is a package name.
    /// - A required or provided capability's name is a virtual name (PSPU
    ///   5.4). That allows what a package name does, and more, such as
    ///   `libc.so.6`, `sh` and `pkgconfig(zlib)`.
    /// - A claim's path is absolute and normal: no empty, `.` or `..`
    ///   component and no trailing `/`. Its target is not empty. Neither
    ///   holds a NUL.
    ///
    /// Constraints and versions are checked as they are parsed.
    pub fn check(&self) -> Result<()> {
        if let Some(description) = &self.description {
            check_description(description).context("description")?;
        }
        if let Some(homepage) = &self.homepage {
            check_homepage(homepage).context("homepage")?;
        }
        if let Some(license) = &self.license {
            check_text(license).context("license")?;
        }
        if let Some(license) = &self.source_license {
            check_text(license).context("source_license")?;
        }
        for name in self.dependencies.keys() {
            check_name(name).context("dependencies")?;
        }
        for name in self.requires.keys() {
            check_capability(name).context("requires")?;
        }
        for name in self.provides.keys() {
            check_capability(name).context("provides")?;
        }
        for (path, target) in &self.claims {
            check_claim(path, target).context("claims")?;
        }
        Ok(())
    }

    /// The top-level fields in their canonical form, one line each, sorted,
    /// without the absent ones.
    pub(crate) fn render_fields(&self) -> String {
        let class = (self.license_class != LicenseClass::Unknown)
            .then(|| self.license_class.as_str().to_owned());
        let fields = [
            ("description", self.description.clone()),
            ("homepage", self.homepage.clone()),
            ("license", self.license.clone()),
            ("license_class", class),
            ("source_license", self.source_license.clone()),
        ];
        let mut out = String::new();
        for (key, value) in fields {
            if let Some(value) = value {
                let _ = writeln!(out, "{key} = {}", toml_string(&value));
            }
        }
        out
    }

    /// The tables in their canonical form, each after a blank line, in the
    /// order `[claims]`, `[dependencies]`, `[provides]`, `[requires]`,
    /// without the empty ones.
    pub(crate) fn render_tables(&self) -> String {
        let mut out = String::new();
        render_table(&mut out, "claims", &self.claims, |target| target.clone());
        render_table(
            &mut out,
            "dependencies",
            &self.dependencies,
            Constraint::to_manifest_string,
        );
        render_table(&mut out, "provides", &self.provides, |version| {
            version
                .as_ref()
                .map_or_else(String::new, |version| version.as_str().to_owned())
        });
        render_table(
            &mut out,
            "requires",
            &self.requires,
            Constraint::to_manifest_string,
        );
        out
    }

    /// Read the fields and tables of a parsed manifest. Values this reads
    /// loosely, and keys it doesn't read, are left to the canonical form
    /// comparison to refuse.
    pub(crate) fn from_manifest(doc: &toml::Table) -> Result<Self> {
        let field = |key: &str| -> Result<Option<String>> {
            match doc.get(key) {
                None => Ok(None),
                Some(value) => value
                    .as_str()
                    .map(|s| Some(s.to_owned()))
                    .with_context(|| format!("'{key}' is not a string")),
            }
        };
        let metadata = Self {
            description: field("description")?,
            homepage: field("homepage")?,
            license: field("license")?,
            license_class: match field("license_class")? {
                Some(class) => class.parse()?,
                None => LicenseClass::Unknown,
            },
            source_license: field("source_license")?,
            dependencies: read_table(doc, "dependencies", Constraint::from_manifest_string)?,
            requires: read_table(doc, "requires", Constraint::from_manifest_string)?,
            provides: read_table(doc, "provides", |s| {
                if s.is_empty() {
                    Ok(None)
                } else {
                    Version::parse_relaxed(s).map(Some)
                }
            })?,
            claims: read_table(doc, "claims", |s| Ok(s.to_owned()))?,
        };
        metadata.check()?;
        Ok(metadata)
    }
}

/// The same metadata, spelled the same, as the manifest would write it.
/// Versions and constraints are compared by spelling, not by PSPU 5.6's
/// order, as their manifests would be.
impl PartialEq for Metadata {
    fn eq(&self, other: &Self) -> bool {
        self.render_fields() == other.render_fields()
            && self.render_tables() == other.render_tables()
    }
}

impl Eq for Metadata {}

fn render_table<T>(
    out: &mut String,
    name: &str,
    table: &BTreeMap<String, T>,
    value: impl Fn(&T) -> String,
) {
    if table.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n[{name}]");
    for (key, item) in table {
        let _ = writeln!(out, "{} = {}", toml_string(key), toml_string(&value(item)));
    }
}

fn read_table<T>(
    doc: &toml::Table,
    name: &str,
    parse: impl Fn(&str) -> Result<T>,
) -> Result<BTreeMap<String, T>> {
    let Some(value) = doc.get(name) else {
        return Ok(BTreeMap::new());
    };
    let table = value
        .as_table()
        .with_context(|| format!("'{name}' is not a table"))?;
    let mut out = BTreeMap::new();
    for (key, value) in table {
        let value = value
            .as_str()
            .with_context(|| format!("{name}: {} is not a string", toml_string(key)))?;
        let parsed = parse(value).with_context(|| format!("{name}: {}", toml_string(key)))?;
        out.insert(key.clone(), parsed);
    }
    Ok(out)
}

/// PSPU 5.18: printable ASCII only, which closes off terminal escapes and
/// lookalike characters in what an operator reads.
fn check_description(s: &str) -> Result<()> {
    check_not_empty(s)?;
    if let Some((i, c)) = s
        .bytes()
        .enumerate()
        .find(|(_, c)| !(0x20..=0x7e).contains(c))
    {
        bail!("byte {c:#04x} at offset {i} is not printable ASCII");
    }
    Ok(())
}

/// PSPU 5.18: an `http` or `https` URL. This checks the scheme, that a host
/// follows it, and that the rest is printable ASCII without spaces, as a URL
/// is. It does not check the URL's syntax further.
fn check_homepage(s: &str) -> Result<()> {
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .ok_or_else(|| anyhow!("'{s}' is not an http or https URL"))?;
    if rest.is_empty() || rest.starts_with('/') || !s.bytes().all(|c| c.is_ascii_graphic()) {
        bail!("'{s}' is not an http or https URL");
    }
    Ok(())
}

fn check_text(s: &str) -> Result<()> {
    check_not_empty(s)?;
    if s.chars().any(char::is_control) {
        bail!("{} holds a control character", toml_string(s));
    }
    Ok(())
}

fn check_not_empty(s: &str) -> Result<()> {
    if s.is_empty() {
        bail!("empty; leave it out instead");
    }
    Ok(())
}

/// The longest virtual name (PSPU 5.4).
const CAPABILITY_MAX_BYTES: usize = 128;

/// A capability's name is a virtual name (PSPU 5.4), as peipkg's
/// `capability.ValidateName` checks it: 2 to 128 bytes, either plain or
/// `namespace(argument)`.
///
/// A plain name is letters of either case, digits, `-`, `.`, `+` and `_`,
/// starting with a letter or digit and ending with one or with `+`, so that
/// it can be an exact soname such as `libGL.so.1` or `libstdc++.so.6`. A
/// namespace is lowercase letters and digits, starting with a letter; its
/// argument is not empty, and is the plain characters with `:` and `/`, as in
/// `pkgconfig(gtk+-3.0)` and `perl(Foo::Bar)`.
pub(crate) fn check_capability(name: &str) -> Result<()> {
    let plain = |c: u8| c.is_ascii_alphanumeric() || b"-.+_".contains(&c);
    let bytes = name.as_bytes();
    let valid = (2..=CAPABILITY_MAX_BYTES).contains(&bytes.len())
        && match name.split_once('(') {
            Some((namespace, rest)) => {
                namespace.starts_with(|c: char| c.is_ascii_lowercase())
                    && namespace
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    && rest.strip_suffix(')').is_some_and(|arg| {
                        !arg.is_empty() && arg.bytes().all(|c| plain(c) || c == b':' || c == b'/')
                    })
            }
            None => {
                bytes.iter().all(|&c| plain(c))
                    && bytes[0].is_ascii_alphanumeric()
                    && (bytes[bytes.len() - 1].is_ascii_alphanumeric()
                        || bytes[bytes.len() - 1] == b'+')
            }
        };
    if !valid {
        bail!(
            "invalid capability '{name}': use 2 to {CAPABILITY_MAX_BYTES} ASCII letters, digits \
             and - . + _, starting with a letter or digit and ending with one or +, \
             or namespace(argument)"
        );
    }
    Ok(())
}

/// A claimed path is absolute and normal, and its target is not empty.
fn check_claim(path: &str, target: &str) -> Result<()> {
    let normal = path.strip_prefix('/').is_some_and(|rel| {
        rel.split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
    }) && !path.contains('\0');
    if !normal {
        bail!("{}: not a normal absolute path", toml_string(path));
    }
    if target.is_empty() || target.contains('\0') {
        bail!(
            "{}: target {} is not a link target",
            toml_string(path),
            toml_string(target)
        );
    }
    Ok(())
}
