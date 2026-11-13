//! A closure laid out for a filesystem image that carries its own security
//! metadata, such as a read-only image a boot medium mounts as `/usr`.
//!
//! A tree ppk composes on Peios gets each file's and directory's descriptor
//! and signature as ppk writes them. An image built anywhere else can't be
//! given them that way, so this lists what the tree would hold, path by
//! path, with the extended attributes each would carry, for the image's
//! writer to store as they are.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::access::{Inherit, stored_descriptor};
use crate::closure::{merge, package_identities, read_packages};
use crate::inventory::INVENTORY;
use crate::store::ArtifactDir;
use crate::{Entry, Identity, Inventory, PackageId, Request};

/// The attribute the kernel reads a file's or directory's descriptor from.
pub const DESCRIPTOR_ATTRIBUTE: &str = "security.peios.sd";

/// The attribute the kernel reads a file's signature from.
pub const SIGNATURE_ATTRIBUTE: &str = "security.peios.sig";

/// The mode of a directory, as a composed tree has it.
const DIR_MODE: u32 = 0o755;

/// The mode of a file that isn't executable, as the store holds it.
const FILE_MODE: u32 = 0o444;

/// The mode of an executable file, as the store holds it.
const EXEC_MODE: u32 = 0o555;

/// A closure as an image would hold it.
#[derive(Debug)]
pub struct ClosureImage {
    /// The closure's identity, as its inventory names it.
    pub identity: Identity,
    /// Its root, which is `/usr` when the image is mounted there.
    pub root: ImageEntry,
    /// Everything beneath the root, each parent before what's in it, sorted
    /// by path.
    pub entries: Vec<ImageEntry>,
}

/// A file, directory or symlink of a [`ClosureImage`].
#[derive(Debug)]
pub struct ImageEntry {
    /// Its path relative to the root, or empty for the root.
    pub path: String,
    /// Its permission bits.
    pub mode: u32,
    pub kind: ImageKind,
    /// Its extended attributes, by name: a descriptor for each, and a
    /// signature for a signed file.
    pub attributes: Vec<(&'static str, Vec<u8>)>,
}

#[derive(Debug)]
pub enum ImageKind {
    Dir,
    /// A file, whose content is in the artifact directory at this path.
    Blob(PathBuf),
    /// A file whose content is these bytes, such as the inventory.
    Bytes(Vec<u8>),
    /// A symlink to this target.
    Symlink(String),
}

/// The closure of `packages` in `artifact_dir`, made for `requests`, as
/// [`closure_for`](crate::closure_for) would compose it: its packages'
/// files, links and directories, the directories they imply, and the
/// inventory.
///
/// Each file and directory carries the descriptor ppk would give it: its
/// package's, if it has one, and otherwise the store's, owned by SYSTEM.
/// A signed file carries its signature as well.
pub fn closure_image(
    packages: &[PackageId],
    requests: &[Request],
    artifact_dir: &Path,
) -> Result<ClosureImage> {
    let layout = ArtifactDir::new(artifact_dir);
    let identities = package_identities(&layout, packages, artifact_dir)?;
    let inventory = Inventory::of(&read_packages(&layout, &identities)?, requests)?;
    let merged = merge(&layout, &identities)?;

    let dir_descriptor = stored_descriptor(Inherit::ToContents);
    let file_descriptor = stored_descriptor(Inherit::No);
    let dir = |path: String| ImageEntry {
        path,
        mode: DIR_MODE,
        kind: ImageKind::Dir,
        attributes: vec![(DESCRIPTOR_ATTRIBUTE, dir_descriptor.clone())],
    };

    let mut entries: Vec<ImageEntry> = Vec::new();
    let mut dirs = std::collections::BTreeSet::new();
    for (path, (entry, _)) in &merged.usr {
        // The directories it's in, which a tree creates as it goes. Those
        // further up came earlier, being sorted before it.
        for parent in Path::new(path).ancestors().skip(1) {
            let parent = parent.to_str().expect("manifest paths are UTF-8");
            if parent.is_empty()
                || merged.usr.contains_key(parent)
                || !dirs.insert(parent.to_owned())
            {
                break;
            }
            entries.push(dir(parent.to_owned()));
        }

        entries.push(match entry {
            Entry::Blob {
                exec,
                descriptor,
                signature,
                ..
            } => {
                let descriptor = descriptor
                    .as_ref()
                    .map_or_else(|| file_descriptor.clone(), |sd| sd.as_bytes().to_vec());
                let mut attributes = vec![(DESCRIPTOR_ATTRIBUTE, descriptor)];
                if let Some(signature) = signature {
                    attributes.push((SIGNATURE_ATTRIBUTE, layout.read_signature(signature)?));
                }
                ImageEntry {
                    path: path.clone(),
                    mode: if *exec { EXEC_MODE } else { FILE_MODE },
                    kind: ImageKind::Blob(layout.file_path(entry)),
                    attributes,
                }
            }
            Entry::Dir { descriptor } => {
                dirs.insert(path.clone());
                let mut each = dir(path.clone());
                if let Some(sd) = descriptor {
                    each.attributes = vec![(DESCRIPTOR_ATTRIBUTE, sd.as_bytes().to_vec())];
                }
                each
            }
            // A link carries a descriptor too, as one created in a tree
            // inherits one: where descriptors are required, a link without
            // one would stop the paths that go through it.
            Entry::Symlink(target) => ImageEntry {
                path: path.clone(),
                mode: 0o777,
                kind: ImageKind::Symlink(target.clone()),
                attributes: vec![(DESCRIPTOR_ATTRIBUTE, file_descriptor.clone())],
            },
        });
    }
    if merged.usr.contains_key(INVENTORY) {
        bail!("a package claims /usr/{INVENTORY}, which is the closure's own");
    }
    entries.push(ImageEntry {
        path: INVENTORY.to_owned(),
        mode: FILE_MODE,
        kind: ImageKind::Bytes(inventory.render().into_bytes()),
        attributes: vec![(DESCRIPTOR_ATTRIBUTE, file_descriptor)],
    });
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(ClosureImage {
        identity: inventory.identity.clone(),
        root: dir(String::new()),
        entries,
    })
}
