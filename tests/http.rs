//! Fetching and resolving from repositories served over HTTP, by a small
//! server in a thread that serves a directory as it is.

mod support;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use ppk::Request;
use ppk::repo;

use support::*;

fn ppk_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ppk"))
        .env("PPK_DEVELOPMENT_ANCHORS", anchors_dir())
        .args(args)
        .output()
        .unwrap()
}

fn arg(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_configured_repository_is_fetched_from_over_http() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    let key = load(TEST_KEY);
    publish(&repo, &key);
    let server = Server::new(&repo);
    let (config, state, cache, store) = (
        work.path().join("conf"),
        work.path().join("state"),
        work.path().join("cache"),
        work.path().join("store"),
    );

    let public = identity(TEST_KEY);
    let added = ppk_cli(&[
        "repo",
        "add",
        "peios",
        &dead_url(),
        &server.url,
        "--identity",
        &public,
        "--config-dir",
        arg(&config),
    ]);
    assert!(added.status.success(), "{}", stderr(&added));
    let file = fs::read_to_string(config.join("repositories/peios.toml")).unwrap();
    assert!(file.starts_with("urls = [\"http://127.0.0.1:"), "{file}");

    let fetched = ppk_cli(&[
        "fetch",
        "dev.peios.app",
        "--artifact-dir",
        arg(&store),
        "--config-dir",
        arg(&config),
        "--state-dir",
        arg(&state),
        "--cache-dir",
        arg(&cache),
    ]);
    assert!(fetched.status.success(), "{}", stderr(&fetched));
    let stored: Vec<String> = (ppk::stored_packages(&store).unwrap().into_iter())
        .map(|(id, _)| id.to_string())
        .collect();
    assert_eq!(stored, [APP, BASE]);
    for (package, identity) in ppk::stored_packages(&store).unwrap() {
        assert_eq!(ppk::packed(&package, &repo).unwrap(), Some(identity));
    }
    // The executable file is stored in its executable form.
    let exec = blob_of(&store, APP);
    assert!(
        exec.to_str().unwrap().ends_with(".exec"),
        "{}",
        exec.display()
    );
    assert_eq!(fs::read_to_string(&exec).unwrap(), APP);
    assert_eq!(
        fs::read_to_string(state.join("sequences/peios")).unwrap(),
        "1\n"
    );
    // Nothing was asked of the server but files of the repository.
    let requests = server.requests();
    assert_eq!(requests[..2], ["repository", "repository.pso"]);
    assert!(
        (requests.iter()).all(|path| path.starts_with("repository")
            || path.starts_with("trees/sha256/")
            || path.starts_with("manifests/sha256/")
            || path.starts_with("blobs/sha256/")),
        "{requests:?}"
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
    assert!(
        listed.contains(&format!(" {} #- (sequence 1, signed ", server.url)),
        "{listed}"
    );
    assert!(listed.contains("signature checked"), "{listed}");

    let shown = ppk_cli(&[
        "repo",
        "show",
        "--artifact-dir",
        &server.url,
        "--identity",
        &public,
    ]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert!(
        String::from_utf8(shown.stdout)
            .unwrap()
            .starts_with("sequence 1\n")
    );
}

#[test]
fn resolve_and_fetch_take_a_url_as_from() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    publish(&repo, &load(TEST_KEY));
    let server = Server::new(&repo);
    let (cache, store) = (work.path().join("cache"), work.path().join("store"));

    let resolved = ppk_cli(&[
        "resolve",
        "-p",
        "dev.peios.app",
        "--from",
        &server.url,
        "--cache-dir",
        arg(&cache),
        "--arch",
        "x86_64",
    ]);
    assert!(resolved.status.success(), "{}", stderr(&resolved));
    let profile = String::from_utf8(resolved.stdout).unwrap();
    assert!(profile.contains("dev.peios.base"), "{profile}");

    let fetched = ppk_cli(&[
        "fetch",
        "--no-deps",
        APP,
        "--from",
        &server.url,
        "--artifact-dir",
        arg(&store),
        "--cache-dir",
        arg(&cache),
    ]);
    assert!(fetched.status.success(), "{}", stderr(&fetched));
    let stored: Vec<String> = (ppk::stored_packages(&store).unwrap().into_iter())
        .map(|(id, _)| id.to_string())
        .collect();
    assert_eq!(stored, [APP]);
}

#[test]
fn a_pointer_signed_by_another_key_is_refused_without_trying_the_next_url() {
    let work = tempfile::tempdir().unwrap();
    let (good, bad) = (work.path().join("good"), work.path().join("bad"));
    publish(&good, &load(TEST_KEY));
    copy_dir(&good, &bad);
    repo::update(&bad, &load(OTHER_KEY), false).unwrap();
    let (bad_server, good_server) = (Server::new(&bad), Server::new(&good));

    let source = remote(
        &[&bad_server.url, &good_server.url],
        Some(&load(TEST_KEY)),
        &work.path().join("cache"),
    );
    let err = fetch(&source, "dev.peios.app", &work.path().join("store")).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        format!(
            "{}repository: not signed as {}: it is signed as {}",
            bad_server.url,
            identity(TEST_KEY),
            identity(OTHER_KEY)
        )
    );
    assert!(good_server.requests().is_empty());
    assert!(!work.path().join("store").exists());
}

#[test]
fn a_tampered_blob_is_refused_without_trying_the_next_url_and_nothing_is_stored() {
    let work = tempfile::tempdir().unwrap();
    let (good, bad) = (work.path().join("good"), work.path().join("bad"));
    let key = load(TEST_KEY);
    publish(&good, &key);
    copy_dir(&good, &bad);
    let blob = blob_of(&bad, "dev.peios.app readme");
    overwrite(&blob, b"tampered");
    let (bad_server, good_server) = (Server::new(&bad), Server::new(&good));

    let store = work.path().join("store");
    let source = remote(
        &[&bad_server.url, &good_server.url],
        Some(&key),
        &work.path().join("cache"),
    );
    let err = fetch(&source, "dev.peios.app", &store).unwrap_err();
    let relative = blob.strip_prefix(&bad).unwrap().to_str().unwrap();
    assert_eq!(
        format!("{err:#}"),
        format!("{}{relative}: does not match its hash", bad_server.url)
    );
    assert!(
        !(good_server.requests().iter()).any(|path| path.starts_with("blobs/")),
        "{:?}",
        good_server.requests()
    );
    // No ref, no manifest, and not the blob.
    assert!(ppk::stored_packages(&store).unwrap().is_empty());
    assert!(!store.join("manifests").exists());
    assert!(!store.join(relative).exists());
    let leftovers: Vec<_> = (fs::read_dir(store.join("blobs/sha256")).unwrap())
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_str().unwrap().starts_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_dead_url_and_a_missing_file_fall_back_to_the_next_url() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    let key = load(TEST_KEY);
    publish(&repo, &key);
    let empty = Server::new(&work.path().join("nothing"));
    let live = Server::new(&repo);
    let dead = dead_url();

    let store = work.path().join("store");
    let source = remote(
        &[&dead, &empty.url, &live.url],
        Some(&key),
        &work.path().join("cache"),
    );
    assert_eq!(fetch(&source, "dev.peios.app", &store).unwrap().len(), 2);
    assert_eq!(empty.requests()[0], "repository");
    assert_eq!(live.requests()[..2], ["repository", "repository.pso"]);
    assert_eq!(ppk::stored_packages(&store).unwrap().len(), 2);

    // A URL that holds the pointer file but not its signature is passed
    // over too.
    let unsigned = work.path().join("unsigned");
    copy_dir(&repo, &unsigned);
    fs::remove_file(unsigned.join("repository.pso")).unwrap();
    let unsigned = Server::new(&unsigned);
    let source = remote(
        &[&unsigned.url, &live.url],
        Some(&key),
        &work.path().join("c2"),
    );
    assert_eq!(
        fetch(&source, "dev.peios.app", &work.path().join("s2"))
            .unwrap()
            .len(),
        2
    );

    // When no URL serves it, the error says why each didn't.
    let source = remote(&[&dead, &empty.url], Some(&key), &work.path().join("c3"));
    let err = format!("{:#}", fetch(&source, "dev.peios.app", &store).unwrap_err());
    assert!(
        err.starts_with("repository: no URL of the repository serves it ("),
        "{err}"
    );
    assert!(err.contains(&format!("{dead}repository: ")), "{err}");
    assert!(
        err.contains(&format!("{}repository: 404 Not Found", empty.url)),
        "{err}"
    );

    // Where every URL says there is no pointer file, there is no
    // repository, which fetching over HTTP needs.
    let source = remote(&[&empty.url], None, &work.path().join("c4"));
    let err = format!("{:#}", fetch(&source, "dev.peios.app", &store).unwrap_err());
    assert_eq!(
        err,
        format!(
            "{}: not published as a repository, which fetching over HTTP needs",
            empty.url
        )
    );
}

#[test]
fn resolving_reads_tree_nodes_and_summaries_and_caches_them() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    let key = load(TEST_KEY);
    publish(&repo, &key);
    let cache = work.path().join("cache");
    let resolve = |server: &Server| {
        let source = remote(&[&server.url], Some(&key), &cache);
        let request = Request::parse_package("dev.peios.app").unwrap();
        ppk::resolve_sources(&[request], &[], &[source], "x86_64")
            .unwrap()
            .ids()
    };

    // Resolving reads no manifest, only tree nodes, summaries among them.
    let first = Server::new(&repo);
    let ids = resolve(&first);
    assert_eq!(ids, [id(APP), id(BASE)]);
    let requests = first.requests();
    assert_eq!(requests[..2], ["repository", "repository.pso"]);
    assert!(
        requests[2..]
            .iter()
            .all(|path| path.starts_with("trees/sha256/")),
        "{requests:?}"
    );
    assert!(!cache.join("manifests").exists());

    // A second run reads only the pointer file and its signature.
    let second = Server::new(&repo);
    assert_eq!(resolve(&second), ids);
    assert_eq!(second.requests(), ["repository", "repository.pso"]);

    // A cached summary that was changed is read again.
    let pointer = repo::read_pointer(&repo, None).unwrap().unwrap();
    let release = &repo::Trees::new(&repo)
        .lookup(pointer.catalog.as_ref().unwrap(), "dev.peios.app")
        .unwrap()[0];
    let summary = release.summary.as_ref().unwrap();
    fs::write(cache.join("trees/sha256").join(summary.hex()), b"tampered").unwrap();
    let third = Server::new(&repo);
    assert_eq!(resolve(&third), ids);
    assert_eq!(
        third.requests()[2..],
        [format!("trees/sha256/{}", summary.hex())]
    );
}

#[test]
fn a_repository_file_takes_a_path_or_urls_but_not_both() {
    let work = tempfile::tempdir().unwrap();
    let config = work.path().join("conf");
    let dir = config.join("repositories");
    fs::create_dir_all(&dir).unwrap();
    let list = || ppk::config::repositories(&config).map_err(|err| format!("{err:#}"));

    fs::write(
        dir.join("peios.toml"),
        "urls = [\"https://pkgs.peios.org\"]\n",
    )
    .unwrap();
    let repositories = list().unwrap();
    assert_eq!(
        repositories[0].location,
        ppk::config::Location::Urls(vec!["https://pkgs.peios.org".to_owned()])
    );
    for (text, message) in [
        (
            "path = \"/r\"\nurls = [\"https://a/\"]\n",
            "both path and urls; a repository has one or the other",
        ),
        ("urls = []\n", "urls is empty"),
        ("urls = \"https://a/\"\n", "urls is not a list of strings"),
        (
            "urls = [\"ftp://a/\"]\n",
            "ftp://a/: not an http or https URL",
        ),
        ("track = \"stable\"\n", "no path or urls"),
    ] {
        fs::write(dir.join("peios.toml"), text).unwrap();
        let err = list().unwrap_err();
        assert!(err.ends_with(message), "{text}: {err}");
    }

    // `repo add` writes a path or URLs, not a mixture.
    let refused = ppk_cli(&[
        "repo",
        "add",
        "mixed",
        "https://pkgs.peios.org/",
        "/mnt/repo",
        "--config-dir",
        arg(&config),
    ]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("/mnt/repo: not an http or https URL"),
        "{}",
        stderr(&refused)
    );
}
