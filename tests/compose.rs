use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use ppk::{EnvDir, EnvSymlink, Extras, Identity, PackageId, RootArtifacts};
use tempfile::TempDir;

/// An artifact directory, and room beside it for composed roots.
struct Store {
    dir: TempDir,
}

impl Store {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("artifacts")).unwrap();
        Self { dir }
    }

    fn artifacts(&self) -> PathBuf {
        self.dir.path().join("artifacts")
    }

    /// Where a composed root goes.
    fn out(&self) -> PathBuf {
        self.dir.path().join("root")
    }

    /// Pack `files`, given as (path, content, mode), as `package`.
    fn pack(&self, package: &str, files: &[(&str, &[u8], u32)]) {
        let content = tempfile::tempdir().unwrap();
        for (path, bytes, mode) in files {
            let path = content.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(*mode)).unwrap();
        }
        ppk::pack(&id(package), content.path(), &self.artifacts()).unwrap();
    }

    fn compose(&self, packages: &[&str], extras: &Extras) -> anyhow::Result<ppk::Composed> {
        let ids: Vec<PackageId> = packages.iter().map(|p| id(p)).collect();
        ppk::compose_env(&ids, &self.artifacts(), &self.out(), extras)
    }

    fn blob(&self, content: &[u8], exec: bool) -> PathBuf {
        let suffix = if exec { ".exec" } else { "" };
        self.artifacts().join(format!(
            "blobs/sha256/{}{suffix}",
            Identity::of(content).hex()
        ))
    }

    /// Everything beside the artifact directory, which a refusal must leave
    /// empty.
    fn beside_artifacts(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name != "artifacts")
            .collect();
        names.sort();
        names
    }
}

fn id(s: &str) -> PackageId {
    s.parse().unwrap()
}

fn symlinks(specs: &[&str]) -> Vec<EnvSymlink> {
    specs.iter().map(|s| s.parse().unwrap()).collect()
}

fn dirs(paths: &[&str]) -> Vec<EnvDir> {
    paths.iter().map(|p| p.parse().unwrap()).collect()
}

const HELLO: &str = "dev.peios.hello@1.0?arch=x86_64";
const LIBZ: &str = "dev.peios.libz@1.3?arch=x86_64";

#[test]
fn usr_is_the_closure_as_hardlinks_to_the_blobs() {
    let store = Store::new();
    store.pack(
        HELLO,
        &[
            ("bin/hello", b"binary", 0o755),
            ("share/README", b"docs", 0o644),
        ],
    );
    let composed = store.compose(&[HELLO], &Extras::default()).unwrap();
    assert_eq!(composed.path, store.out());

    for (path, content, exec) in [
        ("usr/bin/hello", &b"binary"[..], true),
        ("usr/share/README", &b"docs"[..], false),
    ] {
        let file = fs::metadata(store.out().join(path)).unwrap();
        let blob = fs::metadata(store.blob(content, exec)).unwrap();
        assert_eq!(file.ino(), blob.ino(), "{path} is its blob");
    }
    let mode = fs::metadata(store.out()).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o755, "anyone can read the root");
}

#[test]
fn the_identity_is_the_closures_and_the_store_is_not_written() {
    let store = Store::new();
    store.pack(HELLO, &[("bin/hello", b"binary", 0o755)]);
    store.pack(LIBZ, &[("lib/libz.so", b"lib", 0o644)]);
    let composed = store.compose(&[LIBZ, HELLO], &Extras::default()).unwrap();
    assert!(
        !store.artifacts().join("closures").exists(),
        "no closure in the store"
    );

    let elsewhere = store.dir.path().join("closures");
    let closed = ppk::closure(&[id(HELLO), id(LIBZ)], &store.artifacts(), &elsewhere).unwrap();
    assert_eq!(composed.identity, closed.identity);
}

#[test]
fn extras_are_created_around_usr() {
    let store = Store::new();
    store.pack(HELLO, &[("bin/hello", b"binary", 0o755)]);
    let extras = Extras {
        symlinks: symlinks(&["init::bin/hello", "lib/deep/hello::bin/hello", "abs:/x/y"]),
        dirs: dirs(&["proc", "var/lib"]),
        artifacts: RootArtifacts::None,
    };
    store.compose(&[HELLO], &extras).unwrap();
    let out = store.out();

    for (link, target) in [
        ("init", "usr/bin/hello"),
        ("lib/deep/hello", "../../usr/bin/hello"),
        ("abs", "/x/y"),
    ] {
        assert_eq!(fs::read_link(out.join(link)).unwrap(), Path::new(target));
    }
    let hello = fs::metadata(out.join("usr/bin/hello")).unwrap().ino();
    for link in ["init", "lib/deep/hello"] {
        let resolved = fs::metadata(out.join(link)).unwrap().ino();
        assert_eq!(resolved, hello, "{link} resolves inside the root");
    }
    assert!(out.join("proc").is_dir());
    assert!(out.join("var/lib").is_dir());
    assert!(
        !out.join("system").exists(),
        "no artifacts unless asked for"
    );
}

#[test]
fn included_artifacts_are_a_store_ppk_can_use() {
    let store = Store::new();
    store.pack(HELLO, &[("bin/hello", b"binary", 0o755)]);
    store.pack(LIBZ, &[("lib/libz.so", b"lib", 0o644)]);
    store.pack(
        "dev.peios.unused@1.0?arch=x86_64",
        &[("unused", b"unused", 0o644)],
    );
    let extras = Extras {
        artifacts: RootArtifacts::Include,
        ..Extras::default()
    };
    let composed = store.compose(&[HELLO, LIBZ], &extras).unwrap();

    let included = store.out().join("system/artifacts");
    assert!(
        !included.join("refs/dev.peios.unused").exists(),
        "only the composed packages"
    );
    let blob = included.join(format!(
        "blobs/sha256/{}.exec",
        Identity::of(b"binary").hex()
    ));
    assert_eq!(
        fs::metadata(&blob).unwrap().ino(),
        fs::metadata(store.blob(b"binary", true)).unwrap().ino(),
        "blobs are hardlinked"
    );

    let closures = store.dir.path().join("closures");
    let closed = ppk::closure(&[id(HELLO), id(LIBZ)], &included, &closures).unwrap();
    assert_eq!(closed.identity, composed.identity);
}

#[test]
fn copied_artifacts_are_a_separate_store_that_usr_links_into() {
    let store = Store::new();
    store.pack(HELLO, &[("bin/hello", b"binary", 0o755)]);
    store.pack(
        "dev.peios.unused@1.0?arch=x86_64",
        &[("unused", b"unused", 0o644)],
    );
    let extras = Extras {
        artifacts: RootArtifacts::Copy,
        ..Extras::default()
    };
    let composed = store.compose(&[HELLO], &extras).unwrap();

    let copied = store.out().join("system/artifacts");
    assert!(!copied.join("refs/dev.peios.unused").exists());
    let blob = copied.join(format!(
        "blobs/sha256/{}.exec",
        Identity::of(b"binary").hex()
    ));
    let source_ino = fs::metadata(store.blob(b"binary", true)).unwrap().ino();
    let copy_ino = fs::metadata(&blob).unwrap().ino();
    assert_ne!(copy_ino, source_ino, "blobs are copied");
    assert_eq!(
        fs::metadata(store.out().join("usr/bin/hello"))
            .unwrap()
            .ino(),
        copy_ino,
        "usr links into the root's own store"
    );

    let closures = store.dir.path().join("closures");
    let closed = ppk::closure(&[id(HELLO)], &copied, &closures).unwrap();
    assert_eq!(closed.identity, composed.identity);
}

#[test]
fn a_tampered_blob_is_not_copied() {
    let store = Store::new();
    store.pack(HELLO, &[("bin/hello", b"binary", 0o755)]);
    let blob = store.blob(b"binary", true);
    fs::set_permissions(&blob, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&blob, b"tampered").unwrap();
    let extras = Extras {
        artifacts: RootArtifacts::Copy,
        ..Extras::default()
    };
    let err = store.compose(&[HELLO], &extras).unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err:#}");
    assert!(store.beside_artifacts().is_empty(), "nothing written");
}

#[test]
fn an_existing_root_is_refused() {
    let store = Store::new();
    store.pack(HELLO, &[("bin/hello", b"binary", 0o755)]);
    fs::create_dir(store.out()).unwrap();
    let err = store.compose(&[HELLO], &Extras::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{}: already exists", store.out().display())
    );
    assert_eq!(fs::read_dir(store.out()).unwrap().count(), 0);
    assert_eq!(store.beside_artifacts(), ["root"]);
}

#[test]
fn clashing_extras_are_refused_and_nothing_is_written() {
    let store = Store::new();
    store.pack(HELLO, &[("bin/hello", b"binary", 0o755)]);
    for (links, directories, artifacts, message) in [
        (
            vec!["a:/etc"],
            vec!["a/b"],
            RootArtifacts::None,
            "\"a/b\": under \"a\", which is a symlink",
        ),
        (
            vec!["a:/etc", "a/b:x"],
            vec![],
            RootArtifacts::None,
            "\"a/b\": under \"a\", which is a symlink",
        ),
        (
            vec!["a:x", "a:y"],
            vec![],
            RootArtifacts::None,
            "\"a\": given more than once",
        ),
        (
            vec!["a:x"],
            vec!["a"],
            RootArtifacts::None,
            "\"a\": given as both a directory and a symlink",
        ),
        (
            vec!["system:/tmp"],
            vec![],
            RootArtifacts::Include,
            "\"system/artifacts\": under \"system\", which is a symlink",
        ),
        (
            vec![],
            vec!["system/artifacts/blobs"],
            RootArtifacts::Copy,
            "\"system/artifacts/blobs\": inside system/artifacts, which holds the packages",
        ),
    ] {
        let extras = Extras {
            symlinks: symlinks(&links),
            dirs: dirs(&directories),
            artifacts,
        };
        let err = store.compose(&[HELLO], &extras).unwrap_err();
        assert_eq!(err.to_string(), message, "{links:?} {directories:?}");
        assert!(store.beside_artifacts().is_empty(), "nothing written");
    }
}

#[test]
fn clashing_packages_are_refused_and_nothing_is_written() {
    let store = Store::new();
    store.pack(HELLO, &[("bin/tool", b"hello's", 0o755)]);
    store.pack(LIBZ, &[("bin/tool", b"libz's", 0o755)]);
    let err = store
        .compose(&[HELLO, LIBZ], &Extras::default())
        .unwrap_err();
    assert!(
        err.to_string().ends_with(" provide different entries"),
        "{err}"
    );
    assert!(store.beside_artifacts().is_empty());
}

#[test]
fn a_symlink_is_a_path_and_a_target() {
    for (spec, path, target) in [
        ("init::sbin/prelude", "init", "usr/sbin/prelude"),
        ("init:usr/sbin/prelude", "init", "usr/sbin/prelude"),
        ("a/b::x", "a/b", "../usr/x"),
        ("a:/abs", "a", "/abs"),
        ("a:b:c", "a", "b:c"),
    ] {
        let link: EnvSymlink = spec.parse().unwrap();
        assert_eq!((link.path(), link.target()), (path, target), "{spec}");
    }
}

#[test]
fn malformed_symlinks_and_directories_are_refused() {
    for (spec, message) in [
        ("init", "init: not <path>:<target>"),
        ("init:", "init:: not <path>:<target>"),
        ("init::", "init::: not <path>:<target>"),
        ("/init:x", "\"/init\": not a safe relative path"),
        ("../init:x", "\"../init\": not a safe relative path"),
        (
            "usr/x:y",
            "\"usr/x\": inside usr, which the packages provide",
        ),
    ] {
        let err = spec.parse::<EnvSymlink>().unwrap_err();
        assert_eq!(err.to_string(), message, "{spec}");
    }
    for (path, message) in [
        ("usr", "\"usr\": inside usr, which the packages provide"),
        ("a//b", "\"a//b\": not a safe relative path"),
        ("a/", "\"a/\": not a safe relative path"),
        ("", "\"\": not a safe relative path"),
    ] {
        let err = path.parse::<EnvDir>().unwrap_err();
        assert_eq!(err.to_string(), message, "{path:?}");
    }
}

#[test]
fn a_root_on_another_filesystem_needs_copied_artifacts() {
    // As for closures: the build tree and the system temporary directory are
    // separate filesystems on many hosts.
    let artifacts = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let parent = tempfile::tempdir().unwrap();
    let same_device =
        fs::metadata(artifacts.path()).unwrap().dev() == fs::metadata(parent.path()).unwrap().dev();
    if same_device {
        eprintln!("skipped: the build tree and the temporary directory share a filesystem");
        return;
    }
    let content = tempfile::tempdir().unwrap();
    fs::write(content.path().join("a"), b"a").unwrap();
    ppk::pack(&id(HELLO), content.path(), artifacts.path()).unwrap();

    let out = parent.path().join("root");
    for artifacts_mode in [RootArtifacts::None, RootArtifacts::Include] {
        let extras = Extras {
            artifacts: artifacts_mode,
            ..Extras::default()
        };
        let err = ppk::compose_env(&[id(HELLO)], artifacts.path(), &out, &extras).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "{}: not on the same filesystem as the artifact directory {}; \
                 use --copy-artifacts",
                out.display(),
                artifacts.path().display()
            ),
            "{artifacts_mode:?}"
        );
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    let extras = Extras {
        artifacts: RootArtifacts::Copy,
        ..Extras::default()
    };
    ppk::compose_env(&[id(HELLO)], artifacts.path(), &out, &extras).unwrap();
    let file = fs::metadata(out.join("usr/a")).unwrap();
    let blob = out.join(format!(
        "system/artifacts/blobs/sha256/{}",
        Identity::of(b"a").hex()
    ));
    assert_eq!(file.ino(), fs::metadata(blob).unwrap().ino());
}
