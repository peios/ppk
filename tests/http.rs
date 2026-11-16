//! Fetching and resolving from repositories served over HTTP, by a small
//! server in a thread that serves a directory as it is.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use ppk::repo::{self, PrivateKey};
use ppk::{PackageId, Remote, Request, Source};

/// Test-only keys, built into the test, since its source isn't there when
/// it runs.
const TEST_KEY: &str = include_str!("fixtures/repo-test-key.pem");
const OTHER_KEY: &str = include_str!("fixtures/repo-other-key.pem");

const APP: &str = "dev.peios.app@1.0-1?arch=x86_64";
const BASE: &str = "dev.peios.base@1.0-1?arch=x86_64";

fn load(pem: &str) -> PrivateKey {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key.pem");
    fs::write(&path, pem).unwrap();
    PrivateKey::load(&path).unwrap()
}

fn id(value: &str) -> PackageId {
    value.parse().unwrap()
}

/// A server of the directory `root`, and the path of every request it has
/// been sent, in order.
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn new(root: &Path) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (root, log) = (root.to_owned(), requests.clone());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (root, log) = (root.clone(), log.clone());
                thread::spawn(move || answer(stream, &root, &log));
            }
        });
        Self { url, requests }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

/// Answer one request on `stream` with the file below `root` it names, or
/// `404 Not Found`, and close the connection.
fn answer(mut stream: TcpStream, root: &Path, log: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
    }
    let path = line.split(' ').nth(1).unwrap_or("/");
    let path = path.trim_start_matches('/').to_owned();
    log.lock().unwrap().push(path.clone());
    let file = match path.split('/').any(|part| part == "..") {
        true => None,
        false => fs::read(root.join(&path)).ok(),
    };
    let response = match file {
        Some(bytes) => {
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .into_bytes();
            response.extend(bytes);
            response
        }
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
    };
    let _ = stream.write_all(&response);
}

/// A URL nothing listens at.
fn dead_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}/", listener.local_addr().unwrap())
}

/// A signed repository at `repo` holding `dev.peios.app`, with an
/// executable file, which depends on `dev.peios.base`.
fn publish(repo: &Path, key: &PrivateKey) {
    for (package, depends) in [(BASE, None), (APP, Some("dev.peios.base"))] {
        let tree = tempfile::tempdir().unwrap();
        let name = id(package).name().to_owned();
        let file = tree.path().join(&name);
        fs::write(&file, package).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(tree.path().join("README"), format!("{name} readme")).unwrap();
        let mut options = ppk::PackOptions::default();
        if let Some(depends) = depends {
            options
                .metadata
                .dependencies
                .insert(depends.to_owned(), ppk::Constraint::default());
        }
        ppk::pack_with_options(&id(package), &options, tree.path(), repo).unwrap();
    }
    repo::update(repo, key, false).unwrap();
}

/// A copy of the directory `from` at `to`.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            fs::write(&dest, fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}

/// Replace the file at `path`, which is read-only.
fn overwrite(path: &Path, bytes: &[u8]) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(path, bytes).unwrap();
}

/// The path below the repository `repo` of the blob holding `content`.
fn blob_of(repo: &Path, content: &str) -> PathBuf {
    let hex = ppk::Identity::of(content.as_bytes()).hex().to_owned();
    let dir = repo.join("blobs/sha256");
    let name = (fs::read_dir(&dir).unwrap())
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .find(|name| name.starts_with(&hex))
        .unwrap();
    dir.join(name)
}

fn remote(urls: &[&str], key: Option<&PrivateKey>, cache: &Path) -> Source {
    let urls: Vec<String> = urls.iter().map(|url| (*url).to_owned()).collect();
    Source::Remote(
        Remote::new(&urls)
            .unwrap()
            .with_key(key.map(PrivateKey::public))
            .with_cache(cache),
    )
}

/// Resolve `request` from `source`, and fetch what it chose into `store`.
fn fetch(source: &Source, request: &str, store: &Path) -> anyhow::Result<Vec<String>> {
    let request = Request::parse_package(request)?;
    let resolution = ppk::resolve_sources(&[request], &[], std::slice::from_ref(source), "x86_64")?;
    let fetched = ppk::fetch_resolved(&resolution.packages, store)?;
    Ok(fetched.into_iter().map(|each| each.identity).collect())
}

fn ppk_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ppk"))
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

    let public = key.public().to_string();
    let added = ppk_cli(&[
        "repo",
        "add",
        "peios",
        &dead_url(),
        &server.url,
        "--public-key",
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
    assert_eq!(requests[..2], ["repository", "repository.sig"]);
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
        "--public-key",
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
            "{}repository: not signed by the repository's key",
            bad_server.url
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
    assert_eq!(live.requests()[..2], ["repository", "repository.sig"]);
    assert_eq!(ppk::stored_packages(&store).unwrap().len(), 2);

    // A URL that holds the pointer file but not its signature is passed
    // over too.
    let unsigned = work.path().join("unsigned");
    copy_dir(&repo, &unsigned);
    fs::remove_file(unsigned.join("repository.sig")).unwrap();
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
fn tree_nodes_and_manifests_are_cached_and_checked_again() {
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

    let first = Server::new(&repo);
    let ids = resolve(&first);
    assert_eq!(ids, [id(APP), id(BASE)]);
    let manifests: Vec<_> = fs::read_dir(cache.join("manifests/sha256"))
        .unwrap()
        .collect();
    assert_eq!(manifests.len(), 2);
    assert!(cache.join("trees/sha256").is_dir());

    // A second run reads only the pointer file and its signature.
    let second = Server::new(&repo);
    assert_eq!(resolve(&second), ids);
    assert_eq!(second.requests(), ["repository", "repository.sig"]);

    // A cached manifest that was changed is read again.
    let identity = ppk::packed(&id(APP), &repo).unwrap().unwrap();
    fs::write(
        cache.join("manifests/sha256").join(identity.hex()),
        b"tampered",
    )
    .unwrap();
    let third = Server::new(&repo);
    assert_eq!(resolve(&third), ids);
    assert_eq!(
        third.requests()[2..],
        [format!("manifests/sha256/{}", identity.hex())]
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
