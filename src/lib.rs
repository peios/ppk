//! The Peios package format and the artifact directory that stores it.
//!
//! [`pack`] turns a directory into content-addressed blobs and a manifest in
//! an artifact directory:
//!
//! ```text
//! <artifact-dir>/blobs/sha256/<hex>
//!     file content, uncompressed, named by its SHA-256 (mode 0444)
//! <artifact-dir>/blobs/sha256/<hex>.exec
//!     the same content in executable form (mode 0555), for exec entries
//! <artifact-dir>/blobs/sha256/<hex>.<form>
//!     the same content for an entry with security metadata, which it
//!     carries; <form> is the SHA-256 of the entry as the manifest writes it
//! <artifact-dir>/manifests/sha256/<hex>
//!     a manifest, named by the SHA-256 of its bytes
//! <artifact-dir>/refs/<name>/<version>/<arch>
//!     "sha256:<hex>\n": which manifest that package is
//! ```
//!
//! A manifest records name, version, arch, and a `[files]` table mapping each
//! path to an inline table whose first key says what kind of entry it is:
//!
//! ```text
//! "bin/hello" = { blob = "sha256:<hex>", exec = true }
//! "share/doc/hello/README" = { blob = "sha256:<hex>" }
//! "lib/libz.so" = { symlink = "libz.so.1" }
//! "var/lib/hello" = { dir = true }
//! "lib/firmware/hello.bin" = { blob = "sha256:<hex>", signature = "sha256:<hex>" }
//! "libexec/hello" = { dir = true, descriptor = "<hex>" }
//! ```
//!
//! A file or directory may carry a [`SecurityDescriptor`] that replaces the
//! one it would get, and a file a PIP signature, stored as a blob, that
//! becomes its `security.peios.sig` attribute. Each file with security
//! metadata is stored as a file of its own carrying it, so a hardlink to it
//! carries it too.
//!
//! The manifest is emitted by hand rather than through a TOML serializer so
//! that its bytes are canonical: the same input tree always produces the same
//! manifest, and so the same hash. That hash is the package's identity.
//!
//! The name, version and arch are written into the manifest as well as its
//! ref. The ref is only an index, rebuildable from the manifests; a signature
//! will cover the manifest's bytes, so those bytes must say which package
//! they are. A version is a [`Version`]: `[<epoch>:]<upstream>-<revision>`,
//! ordered as PSPU 5.6 orders it.
//!
//! A manifest can also record [`Metadata`]: descriptive fields after the arch,
//! and `[claims]`, `[dependencies]`, `[provides]` and `[requires]` tables
//! before `[files]`, each left out when empty. [`pack`] adds to the written
//! metadata what it derives from the payload's ELF objects: each soname they
//! have as a provide and each they need as a requirement, with a floor from
//! the symbol versions of the libraries in [`SYMBOL_VERSION_FLOORS`] and
//! capabilities from those in [`SYMBOL_CAPABILITIES`]. It refuses a floor
//! written by hand on a library whose floor it derives. Composing creates the
//! links a package claims, and [`resolve`] chooses the packages that
//! dependencies and requirements need, for a [`Profile`].

mod access;
mod closure;
mod compose;
mod derive;
pub mod environment;
mod fetch;
mod id;
mod identity;
mod inventory;
mod manifest;
mod metadata;
mod pack;
mod profile;
mod resolve;
mod security;
#[cfg(test)]
mod stamp_tests;
mod store;
mod version;

pub use closure::{Closed, UsrFiles, closure, closure_copying};
pub use compose::{Composed, EnvDir, EnvSymlink, Extras, RootArtifacts, compose_env};
pub use derive::{SYMBOL_CAPABILITIES, SYMBOL_VERSION_FLOORS};
pub use fetch::{Fetched, fetch, fetch_replacing};
pub use id::{HOST_ARCH, PackageId};
pub use identity::Identity;
pub use inventory::{Inventory, read_inventory};
pub use manifest::{Entry, Manifest, parse_manifest, render_manifest};
pub use metadata::{LicenseClass, Metadata};
pub use pack::{
    PackOptions, Packed, pack, pack_replacing, pack_replacing_with, pack_with, pack_with_options,
};
pub use profile::Profile;
pub use resolve::{Request, Resolution, Resolved, Wanted, resolve, resolve_pinned};
pub use security::SecurityDescriptor;
pub use store::{packed, prepare_artifact_dir, stored_packages, versions};
pub use version::{Constraint, Operator, Version};

/// The hash algorithm, as it appears in artifact paths and manifest values.
pub const ALGORITHM: &str = "sha256";

/// Where packages are stored unless told otherwise.
pub const DEFAULT_ARTIFACT_DIR: &str = "/system/artifacts";

/// Where closures are built unless told otherwise: this directory inside the
/// artifact directory, so that they are on the same filesystem as its blobs.
pub const CLOSURE_SUBDIR: &str = "closures";
