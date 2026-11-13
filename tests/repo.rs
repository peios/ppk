use std::fs;
use std::path::{Path, PathBuf};

use ppk::repo::{self, PrivateKey, PublicKey, Standing, Trees};
use ppk::{Identity, PackageId};

/// Test-only keys, built into the test, since its source isn't there when
/// it runs.
const TEST_KEY: &str = include_str!("fixtures/repo-test-key.pem");
const OTHER_KEY: &str = include_str!("fixtures/repo-other-key.pem");

/// The private key in the PEM text `pem`.
fn load(pem: &str) -> PrivateKey {
    let dir = tempfile::tempdir().unwrap();
    let path: PathBuf = dir.path().join("key.pem");
    fs::write(&path, pem).unwrap();
    PrivateKey::load(&path).unwrap()
}

fn key() -> PrivateKey {
    load(TEST_KEY)
}

fn id(value: &str) -> PackageId {
    value.parse().unwrap()
}

/// Pack `package` into `repo`, depending on each of `depends`.
fn pack(repo: &Path, package: &str, depends: &[&str]) {
    let tree = tempfile::tempdir().unwrap();
    fs::write(tree.path().join("file"), package).unwrap();
    let mut options = ppk::PackOptions::default();
    for name in depends {
        options
            .metadata
            .dependencies
            .insert((*name).to_owned(), ppk::Constraint::default());
    }
    ppk::pack_with_options(&id(package), &options, tree.path(), repo).unwrap();
}

fn leading(repo: &Path) -> ppk::repo::Packages {
    let pointer = repo::read_pointer(repo, None).unwrap().unwrap();
    Trees::new(repo)
        .read_tree(&pointer.indexes[repo::LEADING])
        .unwrap()
}

fn versions(packages: &ppk::repo::Packages, name: &str) -> Vec<String> {
    packages
        .get(name)
        .map(|releases| releases.iter().map(|r| r.version.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn publishing_signs_the_pointer_and_leading_holds_the_newest_that_can_install() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path();
    pack(repo, "dev.peios.base@1.0-1?arch=x86_64", &[]);
    pack(repo, "dev.peios.app@1.0-1?arch=x86_64", &["dev.peios.base"]);
    pack(repo, "dev.peios.app@2.0-1?arch=x86_64", &["dev.peios.base"]);
    pack(
        repo,
        "dev.peios.orphan@1.0-1?arch=x86_64",
        &["dev.peios.missing"],
    );

    let published = repo::update(repo, &key(), false).unwrap();
    assert_eq!(published.pointer.sequence, 1);
    assert_eq!(published.left_out.len(), 1);
    assert_eq!(published.left_out[0].id.name(), "dev.peios.orphan");
    let leading = leading(repo);
    assert_eq!(versions(&leading, "dev.peios.app"), ["2.0-1"]);
    assert!(!leading.contains_key("dev.peios.orphan"));
    assert!(repo::update(repo, &key(), true).is_err());

    // The signature is the key's, and no other key's.
    let public = key().public();
    assert!(repo::read_pointer(repo, Some(&public)).unwrap().is_some());
    let other = load(OTHER_KEY).public();
    let err = repo::read_pointer(repo, Some(&other)).unwrap_err();
    assert!(err.to_string().contains("not signed by"), "{err}");
    let round: PublicKey = public.to_string().parse().unwrap();
    assert_eq!(round, public);

    // Publishing again goes up in sequence.
    assert_eq!(
        repo::update(repo, &key(), false).unwrap().pointer.sequence,
        2
    );
}

#[test]
fn snapshots_and_channels_name_indexes() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path();
    pack(repo, "dev.peios.app@1.0-1?arch=x86_64", &[]);
    repo::update(repo, &key(), false).unwrap();
    repo::snapshot(repo, &key(), "2026.10.0", repo::LEADING).unwrap();
    assert!(repo::snapshot(repo, &key(), "2026.10.0", repo::LEADING).is_err());
    repo::channel(repo, &key(), "2026.10", "2026.10.0", false).unwrap();
    repo::channel(repo, &key(), "stable", "2026.10", false).unwrap();
    repo::channel(repo, &key(), "unstable", repo::LEADING, false).unwrap();
    assert!(repo::channel(repo, &key(), "broken", "nowhere", false).is_err());
    assert!(repo::channel(repo, &key(), "2026.10.0", repo::LEADING, false).is_err());

    // A new version moves leading and unstable, not the snapshot or stable.
    pack(repo, "dev.peios.app@2.0-1?arch=x86_64", &[]);
    let pointer = repo::update(repo, &key(), false).unwrap().pointer;
    let trees = Trees::new(repo);
    let at = |name: &str| {
        let (_, top) = pointer.index_of(name).unwrap();
        versions(&trees.read_tree(top).unwrap(), "dev.peios.app")
    };
    assert_eq!(at("stable"), ["1.0-1"]);
    assert_eq!(at("unstable"), ["2.0-1"]);
    let text = pointer.render();
    assert!(text.contains("channel stable 2026.10\n"), "{text}");
}

#[test]
fn a_yanked_version_leaves_leading_and_a_deleted_one_leaves_a_tombstone() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path();
    pack(repo, "dev.peios.app@1.0-1?arch=x86_64", &[]);
    pack(repo, "dev.peios.app@2.0-1?arch=x86_64", &[]);
    repo::update(repo, &key(), false).unwrap();

    repo::yank(repo, &key(), &id("dev.peios.app@2.0-1?arch=x86_64"), false).unwrap();
    assert_eq!(versions(&leading(repo), "dev.peios.app"), ["1.0-1"]);
    // Publishing again keeps the yank.
    repo::update(repo, &key(), false).unwrap();
    assert_eq!(versions(&leading(repo), "dev.peios.app"), ["1.0-1"]);
    repo::yank(repo, &key(), &id("dev.peios.app@2.0-1?arch=x86_64"), true).unwrap();
    assert_eq!(versions(&leading(repo), "dev.peios.app"), ["2.0-1"]);

    let doomed = id("dev.peios.app@2.0-1?arch=x86_64");
    let identity = ppk::packed(&doomed, repo).unwrap().unwrap();
    repo::delete(repo, &key(), &doomed, "legal takedown").unwrap();
    assert_eq!(ppk::packed(&doomed, repo).unwrap(), None);
    assert!(!repo.join("manifests/sha256").join(identity.hex()).exists());
    assert_eq!(versions(&leading(repo), "dev.peios.app"), ["1.0-1"]);
    let pointer = repo::read_pointer(repo, None).unwrap().unwrap();
    let catalog = Trees::new(repo)
        .read_tree(pointer.catalog.as_ref().unwrap())
        .unwrap();
    let deleted = catalog["dev.peios.app"]
        .iter()
        .find(|r| r.version == "2.0-1")
        .unwrap();
    assert_eq!(deleted.standing, Standing::Deleted("legal takedown".into()));
    // The tombstone outlives later publications.
    repo::update(repo, &key(), false).unwrap();
    let pointer = repo::read_pointer(repo, None).unwrap().unwrap();
    let catalog = Trees::new(repo)
        .read_tree(pointer.catalog.as_ref().unwrap())
        .unwrap();
    assert_eq!(catalog["dev.peios.app"].len(), 2);
    // The other version's files are still there.
    let kept = ppk::packed(&id("dev.peios.app@1.0-1?arch=x86_64"), repo)
        .unwrap()
        .unwrap();
    assert!(repo.join("manifests/sha256").join(kept.hex()).exists());
    assert_ne!(kept, Identity::of(b""));
}
