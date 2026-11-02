use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use ppk::{EnvDir, EnvSymlink, Extras, Identity, PackageId, RootArtifacts, UsrFiles};
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

    /// Pack `files` as `package`, claiming each (path, target) of `claims`.
    fn pack_claiming(&self, package: &str, files: &[(&str, &[u8], u32)], claims: &[(&str, &str)]) {
        let content = tempfile::tempdir().unwrap();
        for (path, bytes, mode) in files {
            let path = content.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(*mode)).unwrap();
        }
        let metadata = ppk::Metadata {
            claims: claims
                .iter()
                .map(|(path, target)| (path.to_string(), target.to_string()))
                .collect(),
            ..ppk::Metadata::default()
        };
        ppk::pack_with(&id(package), &metadata, content.path(), &self.artifacts()).unwrap();
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

const HELLO: &str = "dev.peios.hello@1.0-1?arch=x86_64";
const LIBZ: &str = "dev.peios.libz@1.3-1?arch=x86_64";

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
        ..Extras::default()
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
        "dev.peios.unused@1.0-1?arch=x86_64",
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
        "dev.peios.unused@1.0-1?arch=x86_64",
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
            ..Extras::default()
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

/// Whether a hardlink can be made from `from` to `to`. Only trying says: two
/// mounts of one filesystem have one device number, and refuse links too.
/// pekit's sandbox gives a test `/tmp` and the build tree that way.
fn can_link(from: &Path, to: &Path) -> bool {
    let probe = from.join("probe");
    fs::write(&probe, b"probe").unwrap();
    let linked = fs::hard_link(&probe, to.join("probe"));
    fs::remove_file(&probe).unwrap();
    let _ = fs::remove_file(to.join("probe"));
    match linked {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => false,
        Err(err) => panic!("{err}"),
    }
}

#[test]
fn a_root_on_another_mount_needs_its_files_or_artifacts_copied() {
    // As for closures: the build tree and the system temporary directory are
    // separate mounts on many hosts.
    let artifacts = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let parent = tempfile::tempdir().unwrap();
    if can_link(artifacts.path(), parent.path()) {
        eprintln!("skipped: the build tree and the temporary directory share a mount");
        return;
    }
    let content = tempfile::tempdir().unwrap();
    fs::write(content.path().join("a"), b"a").unwrap();
    ppk::pack(&id(HELLO), content.path(), artifacts.path()).unwrap();

    let out = parent.path().join("root");
    for (artifacts_mode, files, option) in [
        (RootArtifacts::None, UsrFiles::Hardlink, "--copy-files"),
        (
            RootArtifacts::Include,
            UsrFiles::Hardlink,
            "--copy-artifacts",
        ),
        // The artifacts are hardlinked whatever usr does.
        (RootArtifacts::Include, UsrFiles::Copy, "--copy-artifacts"),
    ] {
        let extras = Extras {
            artifacts: artifacts_mode,
            files,
            ..Extras::default()
        };
        let err = ppk::compose_env(&[id(HELLO)], artifacts.path(), &out, &extras).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "{}: not on the same mount as the artifact directory {}; use {option}",
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
    fs::remove_dir_all(&out).unwrap();

    let extras = Extras {
        files: UsrFiles::Copy,
        ..Extras::default()
    };
    ppk::compose_env(&[id(HELLO)], artifacts.path(), &out, &extras).unwrap();
    assert_eq!(fs::read(out.join("usr/a")).unwrap(), b"a");
    assert!(!out.join("system").exists(), "no store comes with it");
}

#[test]
fn copied_files_are_stored_as_blobs_are_but_share_nothing_with_them() {
    let store = Store::new();
    store.pack(
        HELLO,
        &[("bin/hello", b"program", 0o755), ("doc", b"text", 0o644)],
    );
    let extras = Extras {
        files: UsrFiles::Copy,
        ..Extras::default()
    };
    store.compose(&[HELLO], &extras).unwrap();

    for (path, content, exec, mode) in [
        ("usr/bin/hello", &b"program"[..], true, 0o555),
        ("usr/doc", &b"text"[..], false, 0o444),
    ] {
        let file = store.out().join(path);
        let meta = fs::metadata(&file).unwrap();
        let blob = fs::metadata(store.blob(content, exec)).unwrap();
        assert_ne!(meta.ino(), blob.ino(), "{path} is a copy");
        assert_eq!(blob.nlink(), 1, "{path}'s blob gained no link");
        assert_eq!(meta.mode() & 0o777, mode, "{path}");
        assert_eq!(fs::read(&file).unwrap(), content, "{path}");
    }
}

#[test]
fn copying_refuses_a_blob_that_does_not_match_its_hash() {
    let store = Store::new();
    store.pack(HELLO, &[("a", b"a", 0o644)]);
    let blob = store.blob(b"a", false);
    fs::set_permissions(&blob, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&blob, b"tampered").unwrap();

    let extras = Extras {
        files: UsrFiles::Copy,
        ..Extras::default()
    };
    let err = store.compose(&[HELLO], &extras).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{}: does not match its hash", blob.display())
    );
    assert!(!store.out().exists());
}

#[test]
fn copied_files_can_come_from_the_roots_own_artifacts() {
    let store = Store::new();
    store.pack(HELLO, &[("a", b"a", 0o644)]);
    let extras = Extras {
        artifacts: RootArtifacts::Copy,
        files: UsrFiles::Copy,
        ..Extras::default()
    };
    store.compose(&[HELLO], &extras).unwrap();
    let blob = store.out().join(format!(
        "system/artifacts/blobs/sha256/{}",
        Identity::of(b"a").hex()
    ));
    assert_ne!(
        fs::metadata(store.out().join("usr/a")).unwrap().ino(),
        fs::metadata(blob).unwrap().ino()
    );
}

const DASH: &str = "org.git.kernel.dash@0.5.13.5-1?arch=x86_64";
const BUSYBOX: &str = "net.busybox.busybox@1.37.0-1?arch=x86_64";

#[test]
fn claimed_links_are_created_in_usr_and_in_the_root() {
    let store = Store::new();
    store.pack_claiming(
        DASH,
        &[("bin/dash", b"dash", 0o755)],
        &[("/usr/bin/sh", "dash"), ("/init", "usr/bin/dash")],
    );
    store.compose(&[DASH], &Extras::default()).unwrap();
    let out = store.out();
    assert_eq!(
        fs::read_link(out.join("usr/bin/sh")).unwrap(),
        Path::new("dash")
    );
    assert_eq!(
        fs::read_link(out.join("init")).unwrap(),
        Path::new("usr/bin/dash")
    );
    assert_eq!(fs::read(out.join("init")).unwrap(), b"dash");

    // A closure is only usr, so it has the claim under usr and no other.
    let closures = store.dir.path().join("closures");
    let closed = ppk::closure(&[id(DASH)], &store.artifacts(), &closures).unwrap();
    let tree = closed.tree_path;
    assert_eq!(
        fs::read_link(tree.join("bin/sh")).unwrap(),
        Path::new("dash")
    );
    let names: Vec<_> = fs::read_dir(&tree)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["bin"]);
}

#[test]
fn a_path_claimed_twice_is_refused_naming_both() {
    let store = Store::new();
    store.pack_claiming(
        DASH,
        &[("bin/dash", b"dash", 0o755)],
        &[("/usr/bin/sh", "dash")],
    );
    // Even with the same target: a claim says who provides the name.
    store.pack_claiming(
        BUSYBOX,
        &[
            ("bin/busybox", b"busybox", 0o755),
            ("bin/dash", b"dash", 0o755),
        ],
        &[("/usr/bin/sh", "dash"), ("/init", "usr/bin/busybox")],
    );
    let err = format!(
        "{:#}",
        store
            .compose(&[DASH, BUSYBOX], &Extras::default())
            .unwrap_err()
    );
    // In the order of the packages' identities, as everything in a merge.
    let one = format!("\"/usr/bin/sh\": claimed by both {DASH} and {BUSYBOX}");
    let other = format!("\"/usr/bin/sh\": claimed by both {BUSYBOX} and {DASH}");
    assert!(err == one || err == other, "{err}");
    let closures = store.dir.path().join("closures");
    let err = ppk::closure(&[id(BUSYBOX), id(DASH)], &store.artifacts(), &closures).unwrap_err();
    assert!(format!("{err:#}").contains("claimed by both"), "{err:#}");
    assert!(store.beside_artifacts().is_empty());
}

#[test]
fn a_claim_that_collides_with_files_is_refused() {
    for (files, claims, expected) in [
        (
            &[("bin/sh", &b"sh"[..], 0o755)][..],
            &[("/usr/bin/sh", "dash")][..],
            format!("\"/usr/bin/sh\": claimed by {DASH}, but in the files of {DASH}"),
        ),
        (
            &[("bin/sh/x", &b"x"[..], 0o644)][..],
            &[("/usr/bin/sh", "dash")][..],
            format!("\"bin/sh/x\": in {DASH}, but \"bin/sh\" is a link claimed by {DASH}"),
        ),
        (
            &[("bin", &b"bin"[..], 0o644)][..],
            &[("/usr/bin/sh", "dash")][..],
            format!("\"bin/sh\": claimed by {DASH}, but \"bin\" is a file or symlink in {DASH}"),
        ),
        (
            &[("bin/dash", &b"dash"[..], 0o755)][..],
            &[("/usr/bin/sh", "dash"), ("/usr/bin/sh/x", "dash")][..],
            format!("\"bin/sh/x\": claimed by {DASH}, but \"bin/sh\" is a link claimed by {DASH}"),
        ),
        (
            &[("bin/dash", &b"dash"[..], 0o755)][..],
            &[("/usr", "dash")][..],
            format!("\"/usr\": claimed by {DASH}, but is usr itself"),
        ),
    ] {
        let store = Store::new();
        store.pack_claiming(DASH, files, claims);
        let err = store.compose(&[DASH], &Extras::default()).unwrap_err();
        assert_eq!(format!("{err:#}"), expected);
        assert!(store.beside_artifacts().is_empty());
    }
}

#[test]
fn a_claim_outside_usr_that_collides_with_the_environment_is_refused() {
    let claims = [
        ("/init", "usr/bin/dash"),
        ("/sbin/registryd", "../usr/bin/dash"),
    ];
    for (extras, expected) in [
        (
            Extras {
                symlinks: symlinks(&["init::bin/busybox"]),
                ..Extras::default()
            },
            format!("\"init\": claimed by {DASH}, but the environment links it"),
        ),
        (
            Extras {
                dirs: dirs(&["init"]),
                ..Extras::default()
            },
            format!("\"init\": claimed by {DASH}, but the environment makes it a directory"),
        ),
        (
            Extras {
                symlinks: symlinks(&["sbin::sbin"]),
                ..Extras::default()
            },
            "\"sbin/registryd\": under \"sbin\", which is a symlink".to_owned(),
        ),
        (
            Extras {
                dirs: dirs(&["init/x"]),
                ..Extras::default()
            },
            format!("\"init/x\": under \"init\", which is a link claimed by {DASH}"),
        ),
    ] {
        let store = Store::new();
        store.pack_claiming(DASH, &[("bin/dash", b"dash", 0o755)], &claims);
        let err = store.compose(&[DASH], &extras).unwrap_err();
        assert_eq!(format!("{err:#}"), expected);
        assert!(store.beside_artifacts().is_empty());
    }

    let store = Store::new();
    store.pack_claiming(
        DASH,
        &[("bin/dash", b"dash", 0o755)],
        &[("/system/artifacts/x", "y")],
    );
    let extras = Extras {
        artifacts: RootArtifacts::Include,
        ..Extras::default()
    };
    let err = store.compose(&[DASH], &extras).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "\"system/artifacts/x\": inside system/artifacts, which holds the packages"
    );
    // Without an artifact directory of its own, the root has room for it.
    store.compose(&[DASH], &Extras::default()).unwrap();
    assert_eq!(
        fs::read_link(store.out().join("system/artifacts/x")).unwrap(),
        Path::new("y")
    );
}
