//! Exporting a repository with its blobs compressed, and fetching from the
//! export, as a directory and over HTTP.

mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use ppk::repo::{self, Blobs};
use ppk::{Request, Source};

use support::*;

/// The magic number every zstd frame starts with.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

const APP_2: &str = "dev.peios.app@2.0-1?arch=x86_64";

/// The packages stored in `store`, as IDs.
fn stored(store: &Path) -> Vec<String> {
    (ppk::stored_packages(store).unwrap().into_iter())
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Check that `store` holds the files of `dev.peios.app` and
/// `dev.peios.base` as they were packed, not compressed.
fn assert_plain(store: &Path) {
    assert_eq!(stored(store), [APP, BASE]);
    for content in [APP, BASE, "dev.peios.app readme", "dev.peios.base readme"] {
        assert_eq!(
            fs::read_to_string(blob_of(store, content)).unwrap(),
            content
        );
    }
}

/// A zstd frame of `content`.
fn zstd_of(content: &[u8]) -> Vec<u8> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn an_export_is_fetched_from_as_a_directory_and_over_http() {
    let work = tempfile::tempdir().unwrap();
    let (repo, export) = (work.path().join("repo"), work.path().join("export"));
    let key = load(TEST_KEY);
    publish(&repo, &key);
    let published = repo::read_pointer(&repo, None).unwrap().unwrap();

    let exported = repo::export(&repo, &export, &key, 3).unwrap();
    assert_eq!(exported.compressed, 4);
    assert_eq!(exported.pointer.blobs, Blobs::Zstd);
    assert_eq!(exported.pointer.sequence, published.sequence);
    let text = fs::read_to_string(export.join("repository")).unwrap();
    assert!(text.contains("\nblobs zstd\n"), "{text}");
    let checked = repo::read_pointer(&export, Some(&key.public()))
        .unwrap()
        .unwrap();
    assert_eq!(checked.catalog, published.catalog);
    // The repository itself stays plain.
    assert_eq!(fs::read_to_string(blob_of(&repo, APP)).unwrap(), APP);
    let compressed = fs::read(blob_of(&export, APP)).unwrap();
    assert_eq!(compressed[..4], ZSTD_MAGIC);
    let mode = fs::metadata(blob_of(&export, APP))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o444);

    // From the export as a directory, checked against the key.
    let store = work.path().join("store");
    let signed = [(export.clone(), key.public())].into_iter().collect();
    let request = Request::parse_package("dev.peios.app").unwrap();
    let resolution =
        ppk::resolve_signed(&[request], std::slice::from_ref(&export), "x86_64", &signed).unwrap();
    ppk::fetch_resolved(&resolution.packages, &store).unwrap();
    assert_plain(&store);

    // Over HTTP.
    let server = Server::new(&export);
    let store = work.path().join("store-http");
    let source = remote(&[&server.url], Some(&key), &work.path().join("cache"));
    assert_eq!(fetch(&source, "dev.peios.app", &store).unwrap().len(), 2);
    assert_plain(&store);

    // An exact package with --no-deps, from the export as a directory.
    let store = work.path().join("store-one");
    ppk::fetch_from(&id(BASE), &Source::Dir(export.clone()), &store).unwrap();
    assert_eq!(stored(&store), [BASE]);
}

#[test]
fn every_form_of_a_blob_is_one_compressed_file() {
    let work = tempfile::tempdir().unwrap();
    let (repo, export) = (work.path().join("repo"), work.path().join("export"));
    let key = load(TEST_KEY);
    publish(&repo, &key);
    // The same content as a file that isn't executable, in another package.
    let tree = tempfile::tempdir().unwrap();
    fs::write(tree.path().join("copy"), APP).unwrap();
    ppk::pack(&id("dev.peios.copy@1.0-1?arch=x86_64"), tree.path(), &repo).unwrap();
    repo::update(&repo, &key, false).unwrap();

    repo::export(&repo, &export, &key, 3).unwrap();
    let hex = ppk::Identity::of(APP.as_bytes()).hex().to_owned();
    let blobs = export.join("blobs/sha256");
    let (exec, plain) = (blobs.join(format!("{hex}.exec")), blobs.join(&hex));
    let (exec, plain) = (fs::metadata(exec).unwrap(), fs::metadata(plain).unwrap());
    assert_eq!((exec.dev(), exec.ino()), (plain.dev(), plain.ino()));
}

#[test]
fn exporting_again_compresses_only_new_blobs_and_removes_what_was_deleted() {
    let work = tempfile::tempdir().unwrap();
    let (repo, export) = (work.path().join("repo"), work.path().join("export"));
    let key = load(TEST_KEY);
    publish(&repo, &key);
    repo::export(&repo, &export, &key, 3).unwrap();
    let again = repo::export(&repo, &export, &key, 3).unwrap();
    assert_eq!((again.compressed, again.removed), (0, 0));

    // A new version, whose readme is the old one's.
    let tree = tempfile::tempdir().unwrap();
    fs::write(tree.path().join("dev.peios.app"), APP_2).unwrap();
    fs::write(tree.path().join("README"), "dev.peios.app readme").unwrap();
    ppk::pack(&id(APP_2), tree.path(), &repo).unwrap();
    repo::update(&repo, &key, false).unwrap();
    let exported = repo::export(&repo, &export, &key, 3).unwrap();
    assert_eq!(exported.compressed, 1);
    assert_eq!(exported.pointer.sequence, 2);

    // A version deleted is removed from the export too.
    let old = blob_of(&export, APP);
    repo::delete(&repo, &key, &id(APP), "withdrawn").unwrap();
    let exported = repo::export(&repo, &export, &key, 3).unwrap();
    assert!(exported.removed >= 3, "{}", exported.removed);
    assert!(!old.exists());
    assert!(!export.join("refs/dev.peios.app/1.0-1").exists());
    assert!(export.join("refs/dev.peios.app/2.0-1/x86_64").exists());
    assert_eq!(stored(&export), [APP_2, BASE]);
    let store = work.path().join("store");
    let source = Source::Dir(export.clone());
    let request = Request::parse_package("dev.peios.app").unwrap();
    let resolution =
        ppk::resolve_sources(&[request], &[], std::slice::from_ref(&source), "x86_64").unwrap();
    ppk::fetch_resolved(&resolution.packages, &store).unwrap();
    assert_eq!(fs::read_to_string(blob_of(&store, APP_2)).unwrap(), APP_2);
}

#[test]
fn an_export_goes_only_where_it_belongs() {
    let work = tempfile::tempdir().unwrap();
    let (repo, export) = (work.path().join("repo"), work.path().join("export"));
    let key = load(TEST_KEY);
    let err = |result: anyhow::Result<repo::Exported>| format!("{:#}", result.unwrap_err());

    fs::create_dir_all(&repo).unwrap();
    assert!(err(repo::export(&repo, &export, &key, 3)).ends_with("run ppk repo update first"));
    publish(&repo, &key);

    // Not over anything but an earlier export.
    let other = work.path().join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("keep"), "mine").unwrap();
    assert!(err(repo::export(&repo, &other, &key, 3)).contains("not an export"));
    assert!(err(repo::export(&repo, &repo, &key, 3)).contains("over itself"));

    // Not from an export, and not over a later one.
    repo::export(&repo, &export, &key, 3).unwrap();
    let again = work.path().join("again");
    assert!(err(repo::export(&export, &again, &key, 3)).contains("compressed already"));
    let newer = work.path().join("newer");
    copy_dir(&repo, &newer);
    repo::update(&newer, &key, false).unwrap();
    repo::export(&newer, &export, &key, 3).unwrap();
    assert!(err(repo::export(&repo, &export, &key, 3)).contains("later than 1"));
}

#[test]
fn a_corrupted_compressed_blob_is_refused_and_nothing_is_stored() {
    let work = tempfile::tempdir().unwrap();
    let (repo, export) = (work.path().join("repo"), work.path().join("export"));
    let key = load(TEST_KEY);
    publish(&repo, &key);
    repo::export(&repo, &export, &key, 3).unwrap();
    let blob = blob_of(&export, "dev.peios.app readme");
    let relative = blob
        .strip_prefix(&export)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();

    // Another content, well compressed, over HTTP.
    overwrite(&blob, &zstd_of(b"tampered"));
    let server = Server::new(&export);
    let store = work.path().join("store");
    let source = remote(&[&server.url], Some(&key), &work.path().join("cache"));
    let err = fetch(&source, "dev.peios.app", &store).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        format!("{}{relative}: does not match its hash", server.url)
    );
    assert!(stored(&store).is_empty());
    assert!(!store.join(&relative).exists());

    // What isn't a zstd frame, from the directory.
    overwrite(&blob, b"not compressed");
    let source = Source::Dir(export.clone());
    let err = fetch(&source, "dev.peios.app", &store).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.starts_with(&format!("{}: ", blob.display())), "{err}");
    assert!(stored(&store).is_empty());
    assert!(!store.join(&relative).exists());
}

#[test]
fn repo_export_prints_the_pointer_file_it_published() {
    let work = tempfile::tempdir().unwrap();
    let (repo, export) = (work.path().join("repo"), work.path().join("export"));
    publish(&repo, &load(TEST_KEY));
    let key = signer_file(work.path(), TEST_KEY);
    let ppk = |level: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_ppk"))
            .env("PPK_DEVELOPMENT_ANCHORS", anchors_dir())
            .args(["repo", "export", "--artifact-dir"])
            .args([
                &repo,
                Path::new("--to"),
                &export,
                Path::new("--signer"),
                &key,
            ])
            .args(["--level", level])
            .output()
            .unwrap()
    };
    let refused = ppk("23");
    assert_eq!(refused.status.code(), Some(2));
    let exported = ppk("1");
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let printed = String::from_utf8(exported.stdout).unwrap();
    assert_eq!(
        printed,
        fs::read_to_string(export.join("repository")).unwrap()
    );
    assert!(printed.contains("\nblobs zstd\n"), "{printed}");
}
