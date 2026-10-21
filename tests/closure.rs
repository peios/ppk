use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use ppk::{Entry, Identity, PackageId, render_manifest};
use tempfile::TempDir;

/// An artifact directory with its closure directory in the default place.
struct Store {
    dir: TempDir,
}

impl Store {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn artifacts(&self) -> &Path {
        self.dir.path()
    }

    fn closures(&self) -> PathBuf {
        self.dir.path().join("closures")
    }

    /// Pack `files`, given as (path, content, mode), as `package`.
    fn pack(&self, package: &str, files: &[(&str, &[u8], u32)]) -> String {
        let content = tempfile::tempdir().unwrap();
        for (path, bytes, mode) in files {
            let path = content.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(*mode)).unwrap();
        }
        ppk::pack(&id(package), content.path(), self.artifacts())
            .unwrap()
            .identity
    }

    fn closure(&self, packages: &[&str]) -> anyhow::Result<ppk::Closed> {
        let ids: Vec<PackageId> = packages.iter().map(|p| id(p)).collect();
        ppk::closure(&ids, self.artifacts(), &self.closures())
    }

    fn blob(&self, content: &[u8], exec: bool) -> PathBuf {
        let suffix = if exec { ".exec" } else { "" };
        self.artifacts().join(format!(
            "blobs/sha256/{}{suffix}",
            Identity::of(content).hex()
        ))
    }
}

fn id(s: &str) -> PackageId {
    s.parse().unwrap()
}

const HELLO: &str = "dev.peios.hello@1.0?arch=x86_64";
const LIBZ: &str = "dev.peios.libz@1.3?arch=x86_64";

#[test]
fn files_are_hardlinks_to_their_blobs() {
    let store = Store::new();
    store.pack(
        HELLO,
        &[
            ("usr/bin/hello", b"binary", 0o755),
            ("usr/share/README", b"docs", 0o644),
        ],
    );
    let closed = store.closure(&[HELLO]).unwrap();
    assert!(!closed.reused);

    for (path, content, exec, mode) in [
        ("usr/bin/hello", &b"binary"[..], true, 0o555),
        ("usr/share/README", &b"docs"[..], false, 0o444),
    ] {
        let file = fs::metadata(closed.tree_path.join(path)).unwrap();
        let blob = fs::metadata(store.blob(content, exec)).unwrap();
        assert_eq!(file.ino(), blob.ino(), "{path} is the blob, not a copy");
        assert_eq!(file.nlink(), 2, "{path}");
        assert_eq!(file.permissions().mode() & 0o7777, mode, "{path}");
    }

    let other = store.closure(&[HELLO, LIBZ]).unwrap_err();
    assert!(other.to_string().contains("no such package"), "{other:#}");
    store.pack(LIBZ, &[("usr/lib/libz.so", b"lib", 0o755)]);
    let second = store.closure(&[HELLO, LIBZ]).unwrap();
    let file = fs::metadata(second.tree_path.join("usr/bin/hello")).unwrap();
    assert_eq!(file.nlink(), 3, "shared by both closures");
}

#[test]
fn symlinks_and_empty_directories_are_created() {
    let store = Store::new();
    let content = tempfile::tempdir().unwrap();
    fs::create_dir_all(content.path().join("var/lib/hello")).unwrap();
    std::os::unix::fs::symlink("../share/zoneinfo/UTC", content.path().join("localtime")).unwrap();
    ppk::pack(&id(HELLO), content.path(), store.artifacts()).unwrap();

    let closed = store.closure(&[HELLO]).unwrap();
    let link = fs::read_link(closed.tree_path.join("localtime")).unwrap();
    assert_eq!(link, Path::new("../share/zoneinfo/UTC"));
    assert!(closed.tree_path.join("var/lib/hello").is_dir());
}

#[test]
fn the_root_is_readable_by_everyone() {
    let store = Store::new();
    store.pack(HELLO, &[("a", b"a", 0o644)]);
    let closed = store.closure(&[HELLO]).unwrap();
    let mode = fs::metadata(&closed.tree_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o755);
}

#[test]
fn the_manifest_lists_sorted_identities_and_names_the_closure() {
    let store = Store::new();
    let hello = store.pack(HELLO, &[("a", b"a", 0o644)]);
    let libz = store.pack(LIBZ, &[("b", b"b", 0o644)]);
    let closed = store.closure(&[HELLO, LIBZ]).unwrap();

    let mut identities = [hello, libz];
    identities.sort();
    let expected = format!(
        "packages = [\n    \"{}\",\n    \"{}\",\n]\n",
        identities[0], identities[1]
    );
    let manifest = fs::read_to_string(&closed.manifest_path).unwrap();
    assert_eq!(manifest, expected);
    let identity = Identity::of(manifest.as_bytes());
    assert_eq!(closed.identity, identity.to_string());
    assert_eq!(closed.tree_path, store.closures().join(identity.hex()));
    assert_eq!(
        closed.manifest_path,
        store
            .closures()
            .join(format!("{}.manifest", identity.hex()))
    );
}

#[test]
fn order_and_repetition_do_not_matter() {
    let store = Store::new();
    store.pack(HELLO, &[("a", b"a", 0o644)]);
    store.pack(LIBZ, &[("b", b"b", 0o644)]);
    let first = store.closure(&[HELLO, LIBZ]).unwrap();
    let again = store.closure(&[LIBZ, HELLO, LIBZ]).unwrap();
    assert_eq!(first.identity, again.identity);
    assert!(again.reused);
}

#[test]
fn an_existing_closure_is_reused_untouched() {
    let store = Store::new();
    store.pack(HELLO, &[("a", b"a", 0o644)]);
    let first = store.closure(&[HELLO]).unwrap();
    let inode = fs::metadata(first.tree_path.join("a")).unwrap().ino();
    let again = store.closure(&[HELLO]).unwrap();
    assert!(again.reused);
    assert_eq!(
        fs::metadata(again.tree_path.join("a")).unwrap().ino(),
        inode
    );
    let entries: Vec<_> = fs::read_dir(store.closures()).unwrap().collect();
    assert_eq!(entries.len(), 2, "one tree and one manifest, no leftovers");
}

#[test]
fn reusing_a_closure_needs_no_write_access() {
    let store = Store::new();
    store.pack(HELLO, &[("a", b"a", 0o644)]);
    let first = store.closure(&[HELLO]).unwrap();
    fs::set_permissions(store.closures(), fs::Permissions::from_mode(0o555)).unwrap();

    let again = store.closure(&[HELLO]);
    fs::set_permissions(store.closures(), fs::Permissions::from_mode(0o755)).unwrap();
    let again = again.unwrap();
    assert!(again.reused);
    assert_eq!(again.identity, first.identity);
}

#[test]
fn identical_entries_from_two_packages_are_allowed() {
    let store = Store::new();
    store.pack(HELLO, &[("usr/share/licenses/COPYING", b"GPL", 0o644)]);
    store.pack(LIBZ, &[("usr/share/licenses/COPYING", b"GPL", 0o644)]);
    let closed = store.closure(&[HELLO, LIBZ]).unwrap();
    assert!(
        closed
            .tree_path
            .join("usr/share/licenses/COPYING")
            .is_file()
    );
}

#[test]
fn an_empty_directory_can_be_filled_by_another_package() {
    let store = Store::new();
    let content = tempfile::tempdir().unwrap();
    fs::create_dir_all(content.path().join("usr/lib")).unwrap();
    ppk::pack(&id(HELLO), content.path(), store.artifacts()).unwrap();
    store.pack(LIBZ, &[("usr/lib/libz.so", b"lib", 0o755)]);
    let closed = store.closure(&[HELLO, LIBZ]).unwrap();
    assert!(closed.tree_path.join("usr/lib/libz.so").is_file());
}

#[test]
fn different_entries_for_one_path_are_refused_and_nothing_is_written() {
    let store = Store::new();
    store.pack(HELLO, &[("usr/bin/tool", b"hello's", 0o755)]);
    store.pack(LIBZ, &[("usr/bin/tool", b"libz's", 0o755)]);
    let err = store.closure(&[HELLO, LIBZ]).unwrap_err();

    let message = err.to_string();
    assert!(
        message.starts_with("\"usr/bin/tool\": ")
            && message.ends_with(" provide different entries")
            && message.contains(HELLO)
            && message.contains(LIBZ),
        "{message}"
    );
    assert!(!store.closures().exists(), "nothing written");
}

#[test]
fn a_path_under_another_packages_file_is_refused() {
    let store = Store::new();
    store.pack(HELLO, &[("usr/lib", b"a file", 0o644)]);
    store.pack(LIBZ, &[("usr/lib/libz.so", b"lib", 0o755)]);
    let err = store.closure(&[LIBZ, HELLO]).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("\"usr/lib/libz.so\": in {LIBZ}, but \"usr/lib\" is a file or symlink in {HELLO}")
    );
    assert!(!store.closures().exists());
}

#[test]
fn a_symlink_cannot_lead_the_tree_outside_itself() {
    let outside = tempfile::tempdir().unwrap();
    let store = Store::new();
    let content = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), content.path().join("escape")).unwrap();
    ppk::pack(&id(HELLO), content.path(), store.artifacts()).unwrap();
    store.pack(LIBZ, &[("escape/planted", b"x", 0o644)]);

    let err = store.closure(&[HELLO, LIBZ]).unwrap_err();
    assert!(
        err.to_string()
            .ends_with(&format!("\"escape\" is a file or symlink in {HELLO}")),
        "{err:#}"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn a_missing_package_is_reported() {
    let store = Store::new();
    let err = store.closure(&[HELLO]).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "{HELLO}: no such package in {}",
            store.artifacts().display()
        )
    );
}

#[test]
fn a_tampered_package_manifest_is_refused() {
    let store = Store::new();
    let identity: Identity = store.pack(HELLO, &[("a", b"a", 0o644)]).parse().unwrap();
    let path = store
        .artifacts()
        .join(format!("manifests/sha256/{}", identity.hex()));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let hello = PackageId::new("dev.peios.hello", "1.0", "x86_64").unwrap();
    let blob = Entry::Blob {
        hex: Identity::of(b"other").hex().to_owned(),
        exec: false,
    };
    fs::write(&path, render_manifest(&hello, &[("a", blob)])).unwrap();

    let err = store.closure(&[HELLO]).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{}: does not match its hash", path.display())
    );
}

#[test]
fn a_closure_directory_on_another_filesystem_is_refused() {
    // The artifact directory is under the build tree; tempdir() is under the
    // system temporary directory, which is a separate tmpfs on many hosts.
    let artifacts = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let closures = tempfile::tempdir().unwrap();
    let same_device = fs::metadata(artifacts.path()).unwrap().dev()
        == fs::metadata(closures.path()).unwrap().dev();
    if same_device {
        eprintln!("skipped: the build tree and the temporary directory share a filesystem");
        return;
    }
    let content = tempfile::tempdir().unwrap();
    fs::write(content.path().join("a"), b"a").unwrap();
    ppk::pack(&id(HELLO), content.path(), artifacts.path()).unwrap();

    let err = ppk::closure(&[id(HELLO)], artifacts.path(), closures.path()).unwrap_err();
    assert!(
        err.to_string().ends_with(&format!(
            "not on the same filesystem as the artifact directory {}",
            artifacts.path().display()
        )),
        "{err:#}"
    );
    assert_eq!(fs::read_dir(closures.path()).unwrap().count(), 0);
}
