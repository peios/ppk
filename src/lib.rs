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
//! <artifact-dir>/manifests/sha256/<hex>
//!     a manifest, named by the SHA-256 of its bytes
//! <artifact-dir>/refs/<name>/<version>/<arch>
//!     "sha256:<hex>\n": which manifest that package is
//! ```
//!
//! A manifest records name, version, arch, and a [files] table mapping each
//! path to an inline table whose first key says what kind of entry it is:
//!
//! ```text
//! "bin/hello" = { blob = "sha256:<hex>", exec = true }
//! "share/doc/hello/README" = { blob = "sha256:<hex>" }
//! "lib/libz.so" = { symlink = "libz.so.1" }
//! "var/lib/hello" = { dir = true }
//! ```
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
//! before `[files]`, each left out when empty. ppk checks and records these,
//! and acts on none of them yet: nothing resolves dependencies or
//! requirements, and composing creates no claimed links.

mod access;
mod closure;
mod compose;
pub mod environment;
mod fetch;
mod id;
mod identity;
mod manifest;
mod metadata;
mod pack;
mod profile;
mod store;
mod version;

pub use closure::{Closed, UsrFiles, closure, closure_copying};
pub use compose::{Composed, EnvDir, EnvSymlink, Extras, RootArtifacts, compose_env};
pub use fetch::{Fetched, fetch};
pub use id::{HOST_ARCH, PackageId};
pub use identity::Identity;
pub use manifest::{Entry, Manifest, parse_manifest, render_manifest};
pub use metadata::{LicenseClass, Metadata};
pub use pack::{Packed, pack, pack_replacing, pack_replacing_with, pack_with};
pub use profile::Profile;
pub use store::{packed, prepare_artifact_dir, versions};
pub use version::{Constraint, Operator, Version};

/// The hash algorithm, as it appears in artifact paths and manifest values.
pub const ALGORITHM: &str = "sha256";

/// Where packages are stored unless told otherwise.
pub const DEFAULT_ARTIFACT_DIR: &str = "/system/artifacts";

/// Where closures are built unless told otherwise: this directory inside the
/// artifact directory, so that they are on the same filesystem as its blobs.
pub const CLOSURE_SUBDIR: &str = "closures";
