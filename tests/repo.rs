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
    let name = package.split_once('@').unwrap().0;
    fs::write(tree.path().join(name), package).unwrap();
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

/// What `request` resolves to from `repo`, as `name@version` for each
/// package, or the error.
fn resolve(repo: &Path, request: &str) -> Result<Vec<String>, String> {
    let request = ppk::Request::parse_package(request).unwrap();
    ppk::resolve(&[request], &[repo.to_path_buf()], "x86_64")
        .map(|resolution| {
            (resolution.packages.iter())
                .map(|p| format!("{}@{}", p.id.name(), p.id.version()))
                .collect()
        })
        .map_err(|err| format!("{err:#}"))
}

#[test]
fn a_request_from_an_index_takes_that_index_and_so_does_what_it_needs() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path();
    pack(repo, "dev.peios.base@1.0-1?arch=x86_64", &[]);
    pack(repo, "dev.peios.app@1.0-1?arch=x86_64", &["dev.peios.base"]);
    repo::update(repo, &key(), false).unwrap();
    repo::snapshot(repo, &key(), "2026.10.0", repo::LEADING).unwrap();
    repo::channel(repo, &key(), "stable", "2026.10.0", false).unwrap();
    pack(repo, "dev.peios.base@2.0-1?arch=x86_64", &[]);
    pack(repo, "dev.peios.app@2.0-1?arch=x86_64", &["dev.peios.base"]);
    repo::update(repo, &key(), false).unwrap();

    let old = ["dev.peios.app@1.0-1", "dev.peios.base@1.0-1"];
    let new = ["dev.peios.app@2.0-1", "dev.peios.base@2.0-1"];
    assert_eq!(resolve(repo, "dev.peios.app#2026.10.0").unwrap(), old);
    assert_eq!(resolve(repo, "dev.peios.app#stable").unwrap(), old);
    assert_eq!(resolve(repo, "dev.peios.app#leading").unwrap(), new);
    assert_eq!(resolve(repo, "dev.peios.app").unwrap(), new);
    let err = resolve(repo, "dev.peios.app#stable >= 2").unwrap_err();
    assert!(err.contains("no version in #stable satisfies it"), "{err}");
    let err = resolve(repo, "dev.peios.app#nowhere").unwrap_err();
    assert!(err.contains("#nowhere: no repository"), "{err}");

    // The closure records the index it was asked for from.
    let request = ppk::Request::parse_package("dev.peios.app#stable").unwrap();
    let packages: Vec<PackageId> = ppk::resolve(
        std::slice::from_ref(&request),
        &[repo.to_path_buf()],
        "x86_64",
    )
    .unwrap()
    .ids();
    let closed = ppk::closure_for(
        &packages,
        &[request],
        repo,
        &repo.join("closures"),
        ppk::UsrFiles::Hardlink,
    )
    .unwrap();
    let inventory = fs::read_to_string(closed.tree_path.join("inventory")).unwrap();
    assert!(
        inventory.contains("dev.peios.app@1.0-1?arch=x86_64 ")
            && inventory.contains(" [* #stable]\n"),
        "{inventory}"
    );
    assert!(ppk::read_inventory(&closed.tree_path).unwrap().is_some());
}

#[test]
fn yanked_versions_need_an_exact_request_and_deleted_ones_say_why() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path();
    pack(repo, "dev.peios.app@1.0-1?arch=x86_64", &[]);
    pack(repo, "dev.peios.app@2.0-1?arch=x86_64", &[]);
    repo::update(repo, &key(), false).unwrap();
    repo::yank(repo, &key(), &id("dev.peios.app@2.0-1?arch=x86_64"), false).unwrap();
    assert_eq!(
        resolve(repo, "dev.peios.app").unwrap(),
        ["dev.peios.app@1.0-1"]
    );
    assert_eq!(
        resolve(repo, "dev.peios.app = 2.0-1").unwrap(),
        ["dev.peios.app@2.0-1"]
    );

    repo::delete(
        repo,
        &key(),
        &id("dev.peios.app@2.0-1?arch=x86_64"),
        "legal takedown",
    )
    .unwrap();
    let err = resolve(repo, "dev.peios.app = 2.0-1").unwrap_err();
    assert!(
        err.contains("dev.peios.app@2.0-1 was deleted from") && err.contains("legal takedown"),
        "{err}"
    );
}

fn ppk_cli(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ppk"))
        .args(args)
        .output()
        .unwrap()
}

fn arg(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn a_machine_fetches_bare_names_through_the_channel_it_tracks() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    pack(&repo, "dev.peios.app@1.0-1?arch=x86_64", &[]);
    repo::update(&repo, &key(), false).unwrap();
    repo::snapshot(&repo, &key(), "2026.10.0", repo::LEADING).unwrap();
    repo::channel(&repo, &key(), "stable", "2026.10.0", false).unwrap();
    pack(&repo, "dev.peios.app@2.0-1?arch=x86_64", &[]);
    repo::update(&repo, &key(), false).unwrap();

    let config = work.path().join("conf");
    let state = work.path().join("state");
    let store = work.path().join("store");
    let public = key().public().to_string();
    let added = ppk_cli(&[
        "repo",
        "add",
        "peios",
        arg(&repo),
        "--public-key",
        &public,
        "--track",
        "stable",
        "--config-dir",
        arg(&config),
    ]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let fetch = |request: &str| {
        ppk_cli(&[
            "fetch",
            request,
            "--artifact-dir",
            arg(&store),
            "--config-dir",
            arg(&config),
            "--state-dir",
            arg(&state),
        ])
    };
    let fetched = fetch("dev.peios.app");
    assert!(
        fetched.status.success(),
        "{}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    let stored: Vec<String> = (ppk::stored_packages(&store).unwrap().into_iter())
        .map(|(id, _)| id.to_string())
        .collect();
    assert_eq!(stored, ["dev.peios.app@1.0-1?arch=x86_64"]);
    // An index named, or an exact version, isn't the tracked channel's.
    assert!(fetch("dev.peios.app#leading").status.success());
    assert!(
        ppk::packed(&id("dev.peios.app@2.0-1?arch=x86_64"), &store)
            .unwrap()
            .is_some()
    );

    let listed = ppk_cli(&[
        "repo",
        "list",
        "--config-dir",
        arg(&config),
        "--state-dir",
        arg(&state),
    ]);
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.starts_with("peios "), "{listed}");
    assert!(listed.contains("#stable (sequence 4, signed "), "{listed}");
    assert!(listed.contains("signature checked"), "{listed}");

    // A repository that goes back in sequence is refused.
    fs::write(state.join("sequences/peios"), "9\n").unwrap();
    let refused = fetch("dev.peios.app");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("older than 9"));
    fs::write(state.join("sequences/peios"), "4\n").unwrap();

    // A repository signed by another key is refused.
    let other = load(OTHER_KEY).public().to_string();
    assert!(
        ppk_cli(&[
            "repo",
            "add",
            "peios",
            arg(&repo),
            "--public-key",
            &other,
            "--config-dir",
            arg(&config),
        ])
        .status
        .success()
    );
    let refused = fetch("dev.peios.app");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not signed by"));
}

#[test]
fn a_name_two_repositories_publish_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let config = work.path().join("conf");
    for name in ["one", "two"] {
        let repo = work.path().join(name);
        pack(&repo, "dev.peios.app@1.0-1?arch=x86_64", &[]);
        repo::update(&repo, &key(), false).unwrap();
        assert!(
            ppk_cli(&[
                "repo",
                "add",
                name,
                arg(&repo),
                "--config-dir",
                arg(&config)
            ])
            .status
            .success()
        );
    }
    let refused = ppk_cli(&[
        "fetch",
        "dev.peios.app",
        "--artifact-dir",
        arg(&work.path().join("store")),
        "--config-dir",
        arg(&config),
        "--state-dir",
        arg(&work.path().join("state")),
    ]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("published by the repositories one and two"),
        "{stderr}"
    );
}

#[test]
fn a_signed_repository_vouches_for_its_refs_through_its_catalog() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    pack(&repo, "dev.peios.app@1.0-1?arch=x86_64", &[]);
    pack(&repo, "dev.peios.other@1.0-1?arch=x86_64", &[]);
    repo::update(&repo, &key(), false).unwrap();
    // Packed after the last publication: in the refs, not in the catalog.
    pack(&repo, "dev.peios.app@2.0-1?arch=x86_64", &[]);

    let signed = [(repo.clone(), key().public())].into_iter().collect();
    let resolve = |request: &str| {
        let request = ppk::Request::parse_package(request).unwrap();
        ppk::resolve_signed(&[request], std::slice::from_ref(&repo), "x86_64", &signed)
            .map(|resolution| resolution.ids())
            .map_err(|err| format!("{err:#}"))
    };
    assert_eq!(
        resolve("dev.peios.app").unwrap(),
        [id("dev.peios.app@1.0-1?arch=x86_64")]
    );
    // Without a key, the refs are trusted for themselves.
    assert_eq!(
        ppk::resolve(
            &[ppk::Request::parse_package("dev.peios.app").unwrap()],
            std::slice::from_ref(&repo),
            "x86_64"
        )
        .unwrap()
        .ids(),
        [id("dev.peios.app@2.0-1?arch=x86_64")]
    );

    // A ref changed to name another package's manifest is refused.
    let other = ppk::packed(&id("dev.peios.other@1.0-1?arch=x86_64"), &repo)
        .unwrap()
        .unwrap();
    let path = repo.join("refs/dev.peios.app/1.0-1/x86_64");
    let mut perms = fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&path, perms).unwrap();
    fs::write(&path, format!("{other}\n")).unwrap();
    let err = resolve("dev.peios.app").unwrap_err();
    assert!(err.contains("signed catalog lists"), "{err}");
}
