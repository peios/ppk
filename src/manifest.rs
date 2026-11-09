//! Manifests: the canonical text that lists a package's paths and says what
//! else is known about it.

use std::fmt::Write as _;

use anyhow::{Context, Result, anyhow, bail};

use crate::{ALGORITHM, Identity, Metadata, PackageId, SecurityDescriptor};

/// A manifest read back from its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The package the manifest says it is.
    pub id: PackageId,
    /// What else the manifest says about it.
    pub metadata: Metadata,
    /// Every entry, sorted by path.
    pub entries: Vec<(String, Entry)>,
}

/// One entry in a manifest's `[files]` table.
///
/// A file or directory may carry security metadata. It is part of the
/// entry, so two files with the same content and different metadata are
/// different entries, each stored as a file of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A regular file whose content is the blob with the hex SHA-256 `hex`.
    /// `exec` is true if any execute bit was set on the source file.
    /// `descriptor` replaces the security descriptor the file would get.
    /// `signature` is the hex SHA-256 of a blob holding the file's PIP
    /// signature, which becomes its `security.peios.sig` attribute.
    Blob {
        hex: String,
        exec: bool,
        descriptor: Option<SecurityDescriptor>,
        signature: Option<String>,
    },
    /// A symbolic link to this target, recorded exactly as `readlink` returns
    /// it. A relative target resolves from the link's directory; an absolute
    /// one from the root of the system, not from `/usr`, where the package's
    /// own paths are.
    Symlink(String),
    /// A directory that is empty, or that has a security descriptor to
    /// replace the one it would get. Other directories are implied by their
    /// contents and never listed, so each tree has one manifest.
    Dir {
        descriptor: Option<SecurityDescriptor>,
    },
}

impl Entry {
    /// A file whose content has the hex SHA-256 `hex`, with no security
    /// metadata.
    pub fn file(hex: impl Into<String>, exec: bool) -> Self {
        Self::Blob {
            hex: hex.into(),
            exec,
            descriptor: None,
            signature: None,
        }
    }

    /// A directory with no security descriptor.
    pub fn dir() -> Self {
        Self::Dir { descriptor: None }
    }

    /// Whether this is a directory.
    pub fn is_dir(&self) -> bool {
        matches!(self, Self::Dir { .. })
    }

    /// Whether this is a file or directory with security metadata.
    pub fn has_security(&self) -> bool {
        match self {
            Self::Blob {
                descriptor,
                signature,
                ..
            } => descriptor.is_some() || signature.is_some(),
            Self::Dir { descriptor } => descriptor.is_some(),
            Self::Symlink(_) => false,
        }
    }
}

/// Render the manifest for `id` with `metadata` and `entries`, which must
/// already be sorted by path.
///
/// After `name`, `version` and `arch` come the metadata's top-level fields,
/// in byte order of their keys, then its tables, `[claims]`,
/// `[dependencies]`, `[provides]` and `[requires]`, each sorted, and last
/// `[files]`. Absent fields and empty tables are left out, so a package with
/// no metadata has the manifest it had before metadata existed.
///
/// Every entry is an inline table with single spaces inside the braces and
/// around `=`, keys in a fixed order, `exec` written only when true, and
/// `descriptor` and `signature` only when present, so each entry has exactly
/// one spelling. Every other value is a string, and a constraint is written
/// in [`Constraint::to_manifest_string`]'s form.
///
/// [`Constraint::to_manifest_string`]: crate::Constraint::to_manifest_string
pub fn render_manifest<P: AsRef<str>>(
    id: &PackageId,
    metadata: &Metadata,
    entries: &[(P, Entry)],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "name = {}", toml_string(id.name()));
    let _ = writeln!(out, "version = {}", toml_string(id.version()));
    let _ = writeln!(out, "arch = {}", toml_string(id.arch()));
    out.push_str(&metadata.render_fields());
    out.push_str(&metadata.render_tables());
    out.push_str("\n[files]\n");
    for (path, entry) in entries {
        let _ = writeln!(
            out,
            "{} = {}",
            toml_string(path.as_ref()),
            render_entry(entry)
        );
    }
    out
}

/// `entry` as the manifest writes it, after its path and ` = `. A stored
/// file with security metadata is named by this text's hash.
pub(crate) fn render_entry(entry: &Entry) -> String {
    let mut parts = Vec::new();
    match entry {
        Entry::Blob {
            hex,
            exec,
            descriptor,
            signature,
        } => {
            parts.push(format!("blob = \"{ALGORITHM}:{hex}\""));
            if *exec {
                parts.push("exec = true".to_owned());
            }
            if let Some(descriptor) = descriptor {
                parts.push(format!("descriptor = \"{descriptor}\""));
            }
            if let Some(signature) = signature {
                parts.push(format!("signature = \"{ALGORITHM}:{signature}\""));
            }
        }
        Entry::Symlink(target) => parts.push(format!("symlink = {}", toml_string(target))),
        Entry::Dir { descriptor } => {
            parts.push("dir = true".to_owned());
            if let Some(descriptor) = descriptor {
                parts.push(format!("descriptor = \"{descriptor}\""));
            }
        }
    }
    format!("{{ {} }}", parts.join(", "))
}

/// Parse `bytes` as a manifest from an untrusted source.
///
/// Only the canonical form that [`render_manifest`] writes is accepted: the
/// parsed manifest is rendered again and must reproduce `bytes` exactly. That
/// one comparison refuses unknown keys, unsorted entries and any other
/// spelling. Every path must also be relative and contain no empty, `.` or
/// `..` component, so that it stays inside whatever root it is placed under,
/// and the metadata must pass [`Metadata::check`].
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest> {
    let text = std::str::from_utf8(bytes).context("not valid UTF-8")?;
    let doc: toml::Table =
        toml::from_str(text).map_err(|err| anyhow!("not valid TOML: {}", err.message()))?;
    let id = PackageId::new(
        string_field(&doc, "name")?,
        string_field(&doc, "version")?,
        string_field(&doc, "arch")?,
    )?;
    let metadata = Metadata::from_manifest(&doc)?;
    let Some(files) = doc.get("files").and_then(toml::Value::as_table) else {
        bail!("missing the [files] table");
    };

    let mut entries = Vec::with_capacity(files.len());
    for (path, value) in files {
        check_path(path)?;
        let entry = parse_entry(value).with_context(|| toml_string(path))?;
        entries.push((path.clone(), entry));
    }
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    if render_manifest(&id, &metadata, &entries).as_bytes() != bytes {
        bail!("not in canonical form");
    }
    Ok(Manifest {
        id,
        metadata,
        entries,
    })
}

fn string_field<'a>(doc: &'a toml::Table, key: &str) -> Result<&'a str> {
    doc.get(key)
        .and_then(toml::Value::as_str)
        .with_context(|| format!("missing the string '{key}'"))
}

/// The entry `value` describes. Keys this does not read, and values it reads
/// loosely, are left to the canonical-form comparison to refuse.
fn parse_entry(value: &toml::Value) -> Result<Entry> {
    let Some(table) = value.as_table() else {
        bail!("not a table");
    };
    let descriptor = match table.get("descriptor") {
        Some(descriptor) => Some(
            descriptor
                .as_str()
                .context("descriptor is not a string")?
                .parse::<SecurityDescriptor>()
                .context("descriptor")?,
        ),
        None => None,
    };
    if let Some(blob) = table.get("blob") {
        let identity: Identity = blob.as_str().context("blob is not a string")?.parse()?;
        let exec = table
            .get("exec")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        let signature = match table.get("signature") {
            Some(signature) => {
                let identity: Identity = signature
                    .as_str()
                    .context("signature is not a string")?
                    .parse()?;
                Some(identity.hex().to_owned())
            }
            None => None,
        };
        return Ok(Entry::Blob {
            hex: identity.hex().to_owned(),
            exec,
            descriptor,
            signature,
        });
    }
    if let Some(target) = table.get("symlink") {
        let target = target.as_str().context("symlink is not a string")?;
        return Ok(Entry::Symlink(target.to_owned()));
    }
    if table.contains_key("dir") {
        return Ok(Entry::Dir { descriptor });
    }
    bail!("not a blob, symlink or directory")
}

pub(crate) fn check_path(path: &str) -> Result<()> {
    let safe = !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if !safe {
        bail!("{}: not a safe relative path", toml_string(path));
    }
    Ok(())
}

/// Quote `s` as a TOML basic string in one fixed spelling: `"` and `\` are
/// backslash-escaped, every control character is written as `\uXXXX`, and
/// everything else is literal UTF-8.
pub(crate) fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() && (c as u32) < 0x80 => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
