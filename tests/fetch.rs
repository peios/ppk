use std::fs;
use std::path::{Path, PathBuf};

use ppk::{Entry, Identity, Metadata, PackageId, render_manifest};
use tempfile::TempDir;

const HELLO: &str = "dev.peios.hello@1.0-1?arch=x86_64";

/// Build a directory tree from (relative path, content) pairs.
fn tree(files: &[(&str, &[u8])]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    dir
}

fn id(s: &str) -> PackageId {
    s.parse().unwrap()
}

fn pack(package: &str, files: &[(&str, &[u8])], artifact_dir: &Path) -> String {
    ppk::pack(&id(package), tree(files).path(), artifact_dir)
        .unwrap()
        .identity
}

fn fetch(package: &str, source: &Path, artifact_dir: &Path) -> anyhow::Result<ppk::Fetched> {
    ppk::fetch(&id(package), source, artifact_dir)
}

/// Every file under `dir` with its contents, sorted by path.
fn contents_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, found: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_owned();
                found.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

/// Write `bytes` at `rel` under `root`, as a hand-built, possibly hostile,
/// source artifact directory.
fn plant(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Plant `manifest` in `source` under its own hash, with a ref to it from
/// HELLO's name, and return its identity.
fn plant_manifest(source: &Path, manifest: &[u8]) -> Identity {
    let identity = Identity::of(manifest);
    plant(
        source,
        &format!("manifests/sha256/{}", identity.hex()),
        manifest,
    );
    plant(
        source,
        "refs/dev.peios.hello/1.0-1/x86_64",
        format!("{identity}\n").as_bytes(),
    );
    identity
}

fn blob_entry(content: &[u8]) -> Entry {
    Entry::file(Identity::of(content).hex().to_owned(), false)
}

#[test]
fn fetch_reproduces_the_package_exactly() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let identity = pack(
        HELLO,
        &[
            ("usr/bin/hello", b"binary"),
            ("usr/share/doc/README", b"docs"),
        ],
        source.path(),
    );

    let fetched = fetch(HELLO, source.path(), local.path()).unwrap();
    assert_eq!(fetched.identity, identity);
    assert_eq!(fetched.blobs_written, 2);
    assert_eq!(
        fetched.ref_path,
        local.path().join("refs/dev.peios.hello/1.0-1/x86_64")
    );
    assert_eq!(contents_under(local.path()), contents_under(source.path()));
}

#[test]
fn executables_arrive_in_their_executable_form() {
    use std::os::unix::fs::PermissionsExt;
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let content = tree(&[("bin/hello", b"binary")]);
    fs::set_permissions(
        content.path().join("bin/hello"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    ppk::pack(&id(HELLO), content.path(), source.path()).unwrap();

    fetch(HELLO, source.path(), local.path()).unwrap();
    let blob = local.path().join(format!(
        "blobs/sha256/{}.exec",
        Identity::of(b"binary").hex()
    ));
    let mode = fs::metadata(&blob).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o555);
}

#[test]
fn only_missing_blobs_are_copied() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    pack(
        "dev.peios.hello@1.0-1?arch=x86_64",
        &[("bin/hello", b"v1"), ("COPYING", b"license")],
        source.path(),
    );
    pack(
        "dev.peios.hello@2.0-1?arch=x86_64",
        &[("bin/hello", b"v2"), ("COPYING", b"license")],
        source.path(),
    );

    let first = fetch(
        "dev.peios.hello@1.0-1?arch=x86_64",
        source.path(),
        local.path(),
    )
    .unwrap();
    assert_eq!(first.blobs_written, 2);
    let second = fetch(
        "dev.peios.hello@2.0-1?arch=x86_64",
        source.path(),
        local.path(),
    )
    .unwrap();
    assert_eq!(second.blobs_written, 1, "COPYING is already present");
    let again = fetch(
        "dev.peios.hello@2.0-1?arch=x86_64",
        source.path(),
        local.path(),
    )
    .unwrap();
    assert_eq!(again.blobs_written, 0);
    assert_eq!(again.identity, second.identity);
}

#[test]
fn a_different_local_package_is_refused_and_nothing_is_written() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let theirs = pack(HELLO, &[("a", b"theirs")], source.path());
    let ours = pack(HELLO, &[("a", b"ours")], local.path());
    let before = contents_under(local.path());

    let err = fetch(HELLO, source.path(), local.path()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "{HELLO}: already present as {ours}, but {} has {theirs}",
            source.path().display()
        )
    );
    assert_eq!(contents_under(local.path()), before);
}

#[test]
fn replacing_points_the_ref_at_the_fetched_package_and_keeps_the_old() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let theirs = pack(HELLO, &[("a", b"theirs")], source.path());
    let ours = pack(HELLO, &[("a", b"ours")], local.path());

    let fetched = ppk::fetch_replacing(&id(HELLO), source.path(), local.path()).unwrap();
    assert_eq!(fetched.identity, theirs);
    assert_eq!(
        ppk::packed(&id(HELLO), local.path())
            .unwrap()
            .map(|found| found.to_string()),
        Some(theirs.clone())
    );
    let manifests = local.path().join("manifests/sha256");
    for identity in [&ours, &theirs] {
        let hex = identity.strip_prefix("sha256:").unwrap();
        assert!(manifests.join(hex).exists(), "{identity} is kept");
    }

    let again = ppk::fetch_replacing(&id(HELLO), source.path(), local.path()).unwrap();
    assert_eq!(again.identity, theirs);
    assert_eq!(again.blobs_written, 0);
    fetch(HELLO, source.path(), local.path()).unwrap();
}

#[test]
fn replacing_still_checks_what_it_fetches() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let ours = pack(HELLO, &[("a", b"ours")], local.path());
    plant_manifest(
        source.path(),
        render_manifest(
            &id(HELLO),
            &Metadata::default(),
            &[("bin/hello", blob_entry(b"absent"))],
        )
        .as_bytes(),
    );

    let err = ppk::fetch_replacing(&id(HELLO), source.path(), local.path()).unwrap_err();
    assert!(
        format!("{err:#}").ends_with("No such file or directory (os error 2)"),
        "{err:#}"
    );
    assert_eq!(
        ppk::packed(&id(HELLO), local.path())
            .unwrap()
            .map(|found| found.to_string()),
        Some(ours),
        "the ref is unchanged"
    );
}

#[test]
fn a_missing_package_is_reported() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let err = fetch(HELLO, source.path(), local.path()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{HELLO}: no such package in {}", source.path().display())
    );
}

#[test]
fn a_source_that_is_not_a_directory_is_refused() {
    let source = tree(&[("file", b"x")]);
    let local = tempfile::tempdir().unwrap();
    let err = fetch(HELLO, &source.path().join("file"), local.path()).unwrap_err();
    assert!(err.to_string().ends_with("not a directory"), "{err:#}");
}

#[test]
fn a_manifest_that_does_not_match_its_hash_is_refused() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let id = PackageId::new("dev.peios.hello", "1.0-1", "x86_64").unwrap();
    let identity = plant_manifest(
        source.path(),
        render_manifest::<&str>(&id, &Metadata::default(), &[]).as_bytes(),
    );
    let manifest = format!("manifests/sha256/{}", identity.hex());
    fs::write(source.path().join(&manifest), b"tampered").unwrap();

    let err = fetch(HELLO, source.path(), local.path()).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        format!(
            "{}: does not match its hash",
            source.path().join(manifest).display()
        )
    );
    assert!(contents_under(local.path()).is_empty());
}

#[test]
fn a_blob_that_does_not_match_its_hash_is_refused() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let id = PackageId::new("dev.peios.hello", "1.0-1", "x86_64").unwrap();
    let entry = blob_entry(b"genuine");
    let Entry::Blob { hex, .. } = &entry else {
        unreachable!()
    };
    plant(
        source.path(),
        &format!("blobs/sha256/{hex}"),
        b"substituted",
    );
    plant_manifest(
        source.path(),
        render_manifest(&id, &Metadata::default(), &[("bin/hello", entry.clone())]).as_bytes(),
    );

    let err = fetch(HELLO, source.path(), local.path()).unwrap_err();
    let blob = source.path().join(format!("blobs/sha256/{hex}"));
    assert_eq!(
        format!("{err:#}"),
        format!("{}: does not match its hash", blob.display())
    );
    assert!(
        contents_under(local.path()).is_empty(),
        "no blob, manifest or ref"
    );
}

#[test]
fn a_missing_blob_is_reported() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let id = PackageId::new("dev.peios.hello", "1.0-1", "x86_64").unwrap();
    plant_manifest(
        source.path(),
        render_manifest(
            &id,
            &Metadata::default(),
            &[("bin/hello", blob_entry(b"absent"))],
        )
        .as_bytes(),
    );
    let err = fetch(HELLO, source.path(), local.path()).unwrap_err();
    assert!(
        format!("{err:#}").ends_with("No such file or directory (os error 2)"),
        "{err:#}"
    );
    assert!(
        local.path().join("refs").read_dir().is_err(),
        "no ref written"
    );
}

#[test]
fn a_manifest_for_another_package_is_refused() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let other = PackageId::new("dev.peios.other", "1.0-1", "x86_64").unwrap();
    let identity = plant_manifest(
        source.path(),
        render_manifest::<&str>(&other, &Metadata::default(), &[]).as_bytes(),
    );

    let err = fetch(HELLO, source.path(), local.path()).unwrap_err();
    let manifest = source
        .path()
        .join(format!("manifests/sha256/{}", identity.hex()));
    assert_eq!(
        err.to_string(),
        format!(
            "{}: names dev.peios.other@1.0-1?arch=x86_64, not {HELLO}",
            manifest.display()
        )
    );
}

#[test]
fn a_malformed_manifest_is_refused() {
    let id = PackageId::new("dev.peios.hello", "1.0-1", "x86_64").unwrap();
    let canonical = render_manifest(&id, &Metadata::default(), &[("a", blob_entry(b"a"))]);
    for (manifest, problem) in [
        (canonical.replace(" = {", "= {"), "not in canonical form"),
        (
            render_manifest(
                &id,
                &Metadata::default(),
                &[("../escape", blob_entry(b"a"))],
            ),
            "\"../escape\": not a safe relative path",
        ),
    ] {
        let source = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        plant_manifest(source.path(), manifest.as_bytes());
        let err = fetch(HELLO, source.path(), local.path()).unwrap_err();
        assert!(format!("{err:#}").ends_with(problem), "{err:#}");
        assert!(contents_under(local.path()).is_empty());
    }
}

#[test]
fn a_ref_naming_a_path_is_refused_before_it_is_used() {
    let outside = tempfile::tempdir().unwrap();
    let source = outside.path().join("source");
    let local = tempfile::tempdir().unwrap();
    // If the ref were trusted, the manifest would be read from here.
    plant(outside.path(), "secret", b"not a manifest");
    plant(
        &source,
        "refs/dev.peios.hello/1.0-1/x86_64",
        b"sha256:../../../secret\n",
    );

    let err = fetch(HELLO, &source, local.path()).unwrap_err();
    assert!(
        format!("{err:#}").ends_with("'sha256:../../../secret' is not a sha256 identity"),
        "{err:#}"
    );
    assert!(contents_under(local.path()).is_empty());
}

#[test]
fn a_ref_must_end_with_one_newline() {
    let source = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let identity = Identity::of(b"anything");
    plant(
        source.path(),
        "refs/dev.peios.hello/1.0-1/x86_64",
        identity.to_string().as_bytes(),
    );
    let err = fetch(HELLO, source.path(), local.path()).unwrap_err();
    assert!(format!("{err:#}").ends_with(": not a ref"), "{err:#}");
}
