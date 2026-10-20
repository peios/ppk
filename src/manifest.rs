//! Manifests: the canonical text that lists a package's paths.

use std::fmt::Write as _;

use anyhow::{Context, Result, anyhow, bail};

use crate::{ALGORITHM, Identity, PackageId};

/// A manifest read back from its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The package the manifest says it is.
    pub id: PackageId,
    /// Every entry, sorted by path.
    pub entries: Vec<(String, Entry)>,
}

/// One entry in a manifest's `[files]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A regular file whose content is the blob with this hex SHA-256.
    /// `exec` is true if any execute bit was set on the source file.
    Blob { hex: String, exec: bool },
    /// A symbolic link to this target, recorded exactly as `readlink` returns
    /// it. A relative target resolves from the link's directory; an absolute
    /// one from the root of the system the package is installed into.
    Symlink(String),
    /// An empty directory. Directories with anything in them are implied by
    /// their contents and never listed, so each tree has one manifest.
    Dir,
}

/// Render the manifest for `entries`, which must already be sorted by path.
///
/// Every entry is an inline table with single spaces inside the braces and
/// around `=`, keys in a fixed order, and `exec` written only when true, so
/// each entry has exactly one spelling.
pub fn render_manifest<P: AsRef<str>>(id: &PackageId, entries: &[(P, Entry)]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "name = {}", toml_string(id.name()));
    let _ = writeln!(out, "version = {}", toml_string(id.version()));
    let _ = writeln!(out, "arch = {}", toml_string(id.arch()));
    out.push_str("\n[files]\n");
    for (path, entry) in entries {
        let value = match entry {
            Entry::Blob { hex, exec: false } => format!("blob = \"{ALGORITHM}:{hex}\""),
            Entry::Blob { hex, exec: true } => {
                format!("blob = \"{ALGORITHM}:{hex}\", exec = true")
            }
            Entry::Symlink(target) => format!("symlink = {}", toml_string(target)),
            Entry::Dir => "dir = true".to_owned(),
        };
        let _ = writeln!(out, "{} = {{ {value} }}", toml_string(path.as_ref()));
    }
    out
}

/// Parse `bytes` as a manifest from an untrusted source.
///
/// Only the canonical form that [`render_manifest`] writes is accepted: the
/// parsed manifest is rendered again and must reproduce `bytes` exactly. That
/// one comparison refuses unknown keys, unsorted entries and any other
/// spelling. Every path must also be relative and contain no empty, `.` or
/// `..` component, so that it stays inside whatever root it is placed under.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest> {
    let text = std::str::from_utf8(bytes).context("not valid UTF-8")?;
    let doc: toml::Table =
        toml::from_str(text).map_err(|err| anyhow!("not valid TOML: {}", err.message()))?;
    let id = PackageId::new(
        string_field(&doc, "name")?,
        string_field(&doc, "version")?,
        string_field(&doc, "arch")?,
    )?;
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

    if render_manifest(&id, &entries).as_bytes() != bytes {
        bail!("not in canonical form");
    }
    Ok(Manifest { id, entries })
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
    if let Some(blob) = table.get("blob") {
        let identity: Identity = blob.as_str().context("blob is not a string")?.parse()?;
        let exec = table
            .get("exec")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        return Ok(Entry::Blob {
            hex: identity.hex().to_owned(),
            exec,
        });
    }
    if let Some(target) = table.get("symlink") {
        let target = target.as_str().context("symlink is not a string")?;
        return Ok(Entry::Symlink(target.to_owned()));
    }
    if table.contains_key("dir") {
        return Ok(Entry::Dir);
    }
    bail!("not a blob, symlink or directory")
}

fn check_path(path: &str) -> Result<()> {
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
