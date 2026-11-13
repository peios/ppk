//! Named closures: a name in a closure directory that points at one of its
//! closures, so that what a system runs can be chosen, and changed, by name.
//!
//! A name is a symlink beside the closures, `closures/SYSTEM -> <hex>`. It is
//! changed by renaming a new link over it, so it always names one closure or
//! another, never neither. Anything that lists a closure directory must
//! tell names, which are symlinks, from closures, which are directories.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::Identity;
use crate::store;

/// The closure an installed system's `/usr` is.
pub const SYSTEM: &str = "SYSTEM";

/// The closure an installed system's initramfs is built from.
pub const INITRAMFS: &str = "INITRAMFS";

/// The longest a name may be.
const NAME_MAX_BYTES: usize = 64;

/// Check that `name` can name a closure: 1 to 64 ASCII letters, digits and
/// `-`, `.` and `_`, not starting with `.`, so that it is never a temporary
/// directory, and neither a closure's hex nor its manifest's file name.
pub fn check_closure_name(name: &str) -> Result<()> {
    let valid = (1..=NAME_MAX_BYTES).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c))
        && !name.starts_with('.')
        && !name.ends_with(".manifest")
        && !(name.len() == 64 && name.bytes().all(|c| c.is_ascii_hexdigit()));
    if !valid {
        bail!(
            "invalid closure name '{name}': use 1 to {NAME_MAX_BYTES} ASCII letters, digits, \
             - . and _, not starting with a dot, and not a closure's hash"
        );
    }
    Ok(())
}

/// Name the closure `identity` in `closure_dir` `name`, replacing whatever
/// the name named. The closure must exist there.
pub fn name_closure(closure_dir: &Path, name: &str, identity: &Identity) -> Result<()> {
    check_closure_name(name)?;
    if !closure_dir.join(identity.hex()).is_dir() {
        bail!("{identity}: no such closure in {}", closure_dir.display());
    }
    store::create_dirs(closure_dir)?;
    let tmp = tempfile::Builder::new()
        .prefix(".tmp-")
        .tempdir_in(closure_dir)
        .with_context(|| {
            format!(
                "{}: cannot create a temporary directory",
                closure_dir.display()
            )
        })?;
    let link = tmp.path().join(name);
    symlink(identity.hex(), &link).with_context(|| format!("{}", link.display()))?;
    let dest = closure_dir.join(name);
    if dest.is_dir() && !dest.is_symlink() {
        bail!("{}: is a directory, not a name", dest.display());
    }
    fs::rename(&link, &dest).with_context(|| format!("{}", dest.display()))
}

/// The closure `name` names in `closure_dir`, or `None` if it names none.
pub fn named_closure(closure_dir: &Path, name: &str) -> Result<Option<Identity>> {
    check_closure_name(name)?;
    let path = closure_dir.join(name);
    let target = match fs::read_link(&path) {
        Ok(target) => target,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("{}", path.display())),
    };
    let hex = target.to_str().unwrap_or_default();
    let identity: Identity = format!("{}:{hex}", crate::ALGORITHM)
        .parse()
        .with_context(|| format!("{}: does not name a closure", path.display()))?;
    if !closure_dir.join(identity.hex()).is_dir() {
        bail!("{}: names {identity}, which doesn't exist", path.display());
    }
    Ok(Some(identity))
}

/// Remove the name `name` from `closure_dir`. The closure stays. Removing a
/// name that names nothing is an error.
pub fn remove_closure_name(closure_dir: &Path, name: &str) -> Result<()> {
    check_closure_name(name)?;
    let path = closure_dir.join(name);
    if !path.is_symlink() {
        bail!("{}: no such name", path.display());
    }
    fs::remove_file(&path).with_context(|| format!("{}", path.display()))
}

/// Every name in `closure_dir`, sorted, with the closure it names.
pub fn closure_names(closure_dir: &Path) -> Result<Vec<(String, Identity)>> {
    let entries = match fs::read_dir(closure_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("{}", closure_dir.display())),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("{}", closure_dir.display()))?;
        if !entry.file_type()?.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if check_closure_name(&name).is_err() {
            continue;
        }
        if let Some(identity) = named_closure(closure_dir, &name)? {
            names.push((name, identity));
        }
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(names)
}
