//! Resolving from a repository by its catalog's summaries and provides
//! tree, without reading a manifest, and checking each manifest fetched
//! against its summary.

mod support;

use std::fs;
use std::path::Path;

use ppk::repo::{self, PrivateKey, Trees};
use ppk::{Request, Source, Version};

use support::*;

const FOO_1: &str = "dev.peios.foo@1.0-1?arch=x86_64";
const FOO_2: &str = "dev.peios.foo@2.0-1?arch=x86_64";
const USER: &str = "dev.peios.user@1.0-1?arch=x86_64";

/// Pack `package` into `repo`, providing `provides` at the version given,
/// and requiring each of `requires`.
fn pack(repo: &Path, package: &str, provides: &[(&str, &str)], requires: &[(&str, &str)]) {
    let tree = tempfile::tempdir().unwrap();
    fs::write(tree.path().join("file"), package).unwrap();
    let mut options = ppk::PackOptions::default();
    for (capability, version) in provides {
        (options.metadata.provides).insert(
            (*capability).to_owned(),
            Some(Version::parse_relaxed(version).unwrap()),
        );
    }
    for (capability, constraint) in requires {
        (options.metadata.requires).insert(
            (*capability).to_owned(),
            ppk::Constraint::from_manifest_string(constraint).unwrap(),
        );
    }
    ppk::pack_with_options(&id(package), &options, tree.path(), repo).unwrap();
}

/// A signed repository at `repo` with two versions of `dev.peios.foo`,
/// providing `libfoo.so.1` at 1.0 and 2.0, and `dev.peios.user`, which
/// requires `libfoo.so.1 < 2`.
fn publish_providers(repo: &Path, key: &PrivateKey) {
    pack(repo, FOO_1, &[("libfoo.so.1", "1.0")], &[]);
    pack(repo, FOO_2, &[("libfoo.so.1", "2.0")], &[]);
    pack(repo, USER, &[], &[("libfoo.so.1", "< 2")]);
    repo::update(repo, key, false).unwrap();
}

fn ids(resolution: anyhow::Result<ppk::Resolution>) -> Vec<String> {
    (resolution.unwrap().ids().into_iter())
        .map(|id| id.to_string())
        .collect()
}

fn resolve(source: &Source, requests: &[Request]) -> anyhow::Result<ppk::Resolution> {
    ppk::resolve_sources(requests, &[], std::slice::from_ref(source), "x86_64")
}

/// The manifests `server` was asked for.
fn manifests(server: &Server) -> Vec<String> {
    (server.requests().into_iter())
        .filter(|path| path.starts_with("manifests/"))
        .collect()
}

#[test]
fn a_capability_is_resolved_through_the_provides_tree_without_a_manifest() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    let key = load(TEST_KEY);
    publish_providers(&repo, &key);

    // The provides tree lists both versions, and each package's own name.
    let pointer = repo::read_pointer(&repo, None).unwrap().unwrap();
    let trees = Trees::new(&repo);
    let provides = pointer.provides.as_ref().unwrap();
    let providers: Vec<(String, Option<String>)> = (trees
        .lookup_providers(provides, "libfoo.so.1")
        .unwrap()
        .into_iter())
    .map(|p| (p.version, p.provided))
    .collect();
    assert_eq!(
        providers,
        [
            ("1.0-1".to_owned(), Some("1.0".to_owned())),
            ("2.0-1".to_owned(), Some("2.0".to_owned()))
        ]
    );
    assert_eq!(
        trees.lookup_providers(provides, "dev.peios.user").unwrap()[0].provided,
        Some("1.0-1".to_owned())
    );

    let server = Server::new(&repo);
    let source = remote(&[&server.url], Some(&key), &work.path().join("cache"));
    let capability = Request::parse_capability("libfoo.so.1 < 2").unwrap();
    assert_eq!(ids(resolve(&source, &[capability])), [FOO_1]);
    // A requirement reaches its providers the same way.
    let user = Request::parse_package("dev.peios.user").unwrap();
    assert_eq!(
        ids(resolve(&source, std::slice::from_ref(&user))),
        [FOO_1, USER]
    );
    assert!(manifests(&server).is_empty(), "{:?}", server.requests());

    // The same as from the repository as a directory.
    let dir = Source::Dir(repo.clone());
    assert_eq!(ids(resolve(&dir, &[user])), [FOO_1, USER]);

    // Fetching reads the manifests it stores, and only those.
    let store = work.path().join("store");
    fetch(&source, "dev.peios.user", &store).unwrap();
    assert_eq!(manifests(&server).len(), 2);
}

#[test]
fn a_yanked_version_is_chosen_only_by_a_request_for_exactly_it() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    let key = load(TEST_KEY);
    publish_providers(&repo, &key);
    repo::yank(&repo, &key, &id(FOO_2), false).unwrap();
    let server = Server::new(&repo);
    let source = remote(&[&server.url], Some(&key), &work.path().join("cache"));

    let any = Request::parse_package("dev.peios.foo").unwrap();
    assert_eq!(ids(resolve(&source, &[any])), [FOO_1]);
    let capability = Request::parse_capability("libfoo.so.1").unwrap();
    assert_eq!(ids(resolve(&source, &[capability])), [FOO_1]);
    let exact = Request::parse_package("dev.peios.foo = 2.0-1").unwrap();
    assert_eq!(ids(resolve(&source, &[exact])), [FOO_2]);
    assert!(manifests(&server).is_empty(), "{:?}", server.requests());
}

#[test]
fn a_summary_that_was_changed_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    let key = load(TEST_KEY);
    publish(&repo, &key);
    let pointer = repo::read_pointer(&repo, None).unwrap().unwrap();
    let release = &Trees::new(&repo)
        .lookup(pointer.catalog.as_ref().unwrap(), "dev.peios.app")
        .unwrap()[0];
    let summary = release.summary.as_ref().unwrap();
    overwrite(
        &repo.join("trees/sha256").join(summary.hex()),
        b"name = \"dev.peios.app\"\n",
    );

    let server = Server::new(&repo);
    let source = remote(&[&server.url], Some(&key), &work.path().join("cache"));
    let request = Request::parse_package("dev.peios.app").unwrap();
    let err = format!("{:#}", resolve(&source, &[request]).unwrap_err());
    assert!(
        err.contains(&format!(
            "{}trees/sha256/{}: does not match its hash",
            server.url,
            summary.hex()
        )),
        "{err}"
    );
}

#[test]
fn a_manifest_that_isnt_what_its_summary_says_is_refused_at_fetch() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    let key = load(TEST_KEY);
    publish(&repo, &key);

    // Publish dev.peios.app with a summary that says more than its
    // manifest, signed as the repository's own catalog would be.
    let pointer = repo::read_pointer(&repo, None).unwrap().unwrap();
    let trees = Trees::new(&repo);
    let mut catalog = trees.read_tree(pointer.catalog.as_ref().unwrap()).unwrap();
    let release = &mut catalog.get_mut("dev.peios.app").unwrap()[0];
    let summary = fs::read_to_string(
        repo.join("trees/sha256")
            .join(release.summary.as_ref().unwrap().hex()),
    )
    .unwrap();
    let forged = summary.replace(
        "arch = \"x86_64\"\n",
        "arch = \"x86_64\"\ndescription = \"Not what it is\"\n",
    );
    assert_ne!(forged, summary);
    let forged_id = ppk::Identity::of(forged.as_bytes());
    fs::write(repo.join("trees/sha256").join(forged_id.hex()), &forged).unwrap();
    release.summary = Some(forged_id.clone());
    let mut pointer = pointer.clone();
    pointer.catalog = Some(trees.write_tree(&catalog).unwrap());
    let text = pointer.render();
    overwrite(
        &repo.join("repository.sig"),
        &key.sign(text.as_bytes()).unwrap(),
    );
    overwrite(&repo.join("repository"), text.as_bytes());

    let server = Server::new(&repo);
    let source = remote(&[&server.url], Some(&key), &work.path().join("cache"));
    let store = work.path().join("store");
    let err = format!("{:#}", fetch(&source, "dev.peios.app", &store).unwrap_err());
    let identity = ppk::packed(&id(APP), &repo).unwrap().unwrap();
    assert_eq!(
        err,
        format!(
            "{}manifests/sha256/{}: does not begin with its summary, {forged_id}",
            server.url,
            identity.hex()
        )
    );
    assert!(ppk::stored_packages(&store).unwrap().is_empty());
}

#[test]
fn publishing_again_gives_a_catalog_from_before_summaries_its_summaries() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    let key = load(TEST_KEY);
    publish(&repo, &key);
    repo::snapshot(&repo, &key, "2026.10.0", repo::LEADING).unwrap();

    // The catalog and the snapshot as they were written before summaries.
    let mut pointer = repo::read_pointer(&repo, None).unwrap().unwrap();
    let trees = Trees::new(&repo);
    let without = |top: &ppk::Identity| {
        let mut packages = trees.read_tree(top).unwrap();
        for release in packages.values_mut().flatten() {
            release.summary = None;
        }
        trees.write_tree(&packages).unwrap()
    };
    pointer.catalog = Some(without(pointer.catalog.as_ref().unwrap()));
    let snapshot = without(&pointer.indexes["2026.10.0"]);
    pointer
        .indexes
        .insert("2026.10.0".to_owned(), snapshot.clone());
    pointer.provides = None;
    let text = pointer.render();
    overwrite(
        &repo.join("repository.sig"),
        &key.sign(text.as_bytes()).unwrap(),
    );
    overwrite(&repo.join("repository"), text.as_bytes());
    let err = format!("{:#}", trees.read_tree(&snapshot).unwrap_err());
    assert!(err.contains("expected its summary's identity"), "{err}");
    // Read as a directory, it resolves as it did.
    let dir = Source::Dir(repo.clone());
    let request = Request::parse_package("dev.peios.app#2026.10.0").unwrap();
    assert_eq!(ids(resolve(&dir, &[request])), [APP, BASE]);

    let published = repo::update(&repo, &key, false).unwrap();
    let pointer = published.pointer;
    assert!(pointer.provides.is_some());
    for top in [
        pointer.catalog.as_ref().unwrap(),
        &pointer.indexes["2026.10.0"],
    ] {
        let packages = trees.read_tree(top).unwrap();
        assert!((packages.values().flatten()).all(|release| release.summary.is_some()));
    }
    let server = Server::new(&repo);
    let source = remote(&[&server.url], Some(&key), &work.path().join("cache"));
    let request = Request::parse_package("dev.peios.app#2026.10.0").unwrap();
    assert_eq!(ids(resolve(&source, &[request])), [APP, BASE]);
}
