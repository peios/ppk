//! Manifests: the canonical text that lists a package's paths.

use std::fmt::Write as _;

use crate::{ALGORITHM, PackageId};

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

/// Quote `s` as a TOML basic string in one fixed spelling: `"` and `\` are
/// backslash-escaped, every control character is written as `\uXXXX`, and
/// everything else is literal UTF-8.
fn toml_string(s: &str) -> String {
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
