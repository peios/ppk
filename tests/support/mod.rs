//! What the tests of repositories served over HTTP share: a small server
//! in a thread that serves a directory as it is, and a signed repository
//! to serve.

#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use ppk::repo::{self, Anchors, Signer, Trust};
use ppk::{PackageId, Remote, Request, Source};
use pso::core::{Certificate, KeyType, RustCrypto, Warrant};
use pso::keys::SecretKey;

/// The two publishers the tests sign as. Each is a label, from which
/// [`load`] makes a signer.
pub const TEST_KEY: &str = "test";
pub const OTHER_KEY: &str = "other";

pub const APP: &str = "dev.peios.app@1.0-1?arch=x86_64";
pub const BASE: &str = "dev.peios.base@1.0-1?arch=x86_64";

/// A key that is the same in every run, so that every test binary makes
/// the same certificates.
fn secret(seed: u8) -> SecretKey {
    SecretKey::from_seed(KeyType::MlDsa65, [seed; 32])
}

/// The tests' own root and the top-level it signs, written out as a
/// directory that `$PPK_DEVELOPMENT_ANCHORS` can name: the root's public
/// key in `roots`, and the top-level in the trust store of `system`.
struct World {
    dir: tempfile::TempDir,
    top: Certificate,
}

fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let root = secret(1);
        let top = pso::issue::top_level_under_root(
            &root,
            "ppk tests",
            &secret(2).public(),
            vec![
                Warrant::delegate(1),
                Warrant::name_warrant(&["example.com"]).unwrap(),
                Warrant::set("Repository", &[], true).unwrap(),
            ],
        )
        .unwrap();
        let roots = dir.path().join("roots");
        let store = dir
            .path()
            .join("system")
            .join(pso::store::PACKAGE_TOP_LEVELS);
        fs::create_dir_all(&roots).unwrap();
        fs::create_dir_all(&store).unwrap();
        fs::write(
            roots.join("root.pub"),
            pso::keys::format_public(&root.public()),
        )
        .unwrap();
        fs::write(
            store.join(pso::hex::encode(&top.hash(&RustCrypto))),
            top.bytes(),
        )
        .unwrap();
        World { dir, top }
    })
}

/// The directory to give the `ppk` command as `$PPK_DEVELOPMENT_ANCHORS`,
/// so that it trusts what the tests sign.
pub fn anchors_dir() -> &'static Path {
    world().dir.path()
}

/// What the tests verify against: their own root, and the trust store of
/// their own system.
pub fn anchors() -> Arc<Anchors> {
    Anchors::new(vec![secret(1).public()], anchors_dir().join("system"))
}

/// The name the publisher `label` signs as.
pub fn identity(label: &str) -> String {
    format!("{label}.example.com")
}

/// The signing certificate of the publisher `label`, and its key.
fn signing(label: &str) -> (Certificate, SecretKey) {
    let key = secret(if label == TEST_KEY { 3 } else { 4 });
    let certificate = pso::issue::intermediary(
        &world().top,
        &secret(2),
        label,
        &key.public(),
        vec![
            Warrant::name_warrant(&[&identity(label)]).unwrap(),
            Warrant::set("Repository", &[], true).unwrap(),
        ],
    )
    .unwrap();
    (certificate, key)
}

/// The signer of the publisher `label`.
pub fn load(label: &str) -> Signer {
    let (certificate, key) = signing(label);
    Signer::new(&identity(label), certificate, key, Vec::new())
}

/// Write a signer file for the publisher `label` in `dir`, with what it
/// names beside it, and return its path.
pub fn signer_file(dir: &Path, label: &str) -> PathBuf {
    let (certificate, key) = signing(label);
    fs::write(dir.join("signer.cert"), certificate.bytes()).unwrap();
    key.save(&dir.join("signer.key")).unwrap();
    let path = dir.join("signer.toml");
    let text = format!(
        "identity = \"{}\"\ncertificate = \"signer.cert\"\nkey = \"signer.key\"\n",
        identity(label)
    );
    fs::write(&path, text).unwrap();
    path
}

/// What a machine that trusts a signer requires of a signature.
pub trait Public {
    fn public(&self) -> Trust;
}

impl Public for Signer {
    fn public(&self) -> Trust {
        Trust::new(self.identity(), anchors())
    }
}

pub fn id(value: &str) -> PackageId {
    value.parse().unwrap()
}

/// A server of the directory `root`, and the path of every request it has
/// been sent, in order.
pub struct Server {
    pub url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub fn new(root: &Path) -> Self {
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

    pub fn requests(&self) -> Vec<String> {
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
pub fn dead_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}/", listener.local_addr().unwrap())
}

/// A signed repository at `repo` holding `dev.peios.app`, with an
/// executable file, which depends on `dev.peios.base`.
pub fn publish(repo: &Path, key: &Signer) {
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
pub fn copy_dir(from: &Path, to: &Path) {
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
pub fn overwrite(path: &Path, bytes: &[u8]) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(path, bytes).unwrap();
}

/// The path below the repository `repo` of the blob holding `content`.
pub fn blob_of(repo: &Path, content: &str) -> PathBuf {
    let hex = ppk::Identity::of(content.as_bytes()).hex().to_owned();
    let dir = repo.join("blobs/sha256");
    let name = (fs::read_dir(&dir).unwrap())
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .find(|name| name.starts_with(&hex))
        .unwrap();
    dir.join(name)
}

pub fn remote(urls: &[&str], key: Option<&Signer>, cache: &Path) -> Source {
    let urls: Vec<String> = urls.iter().map(|url| (*url).to_owned()).collect();
    Source::Remote(
        Remote::new(&urls)
            .unwrap()
            .with_trust(key.map(Public::public))
            .with_cache(cache),
    )
}

/// Resolve `request` from `source`, and fetch what it chose into `store`.
pub fn fetch(source: &Source, request: &str, store: &Path) -> anyhow::Result<Vec<String>> {
    let request = Request::parse_package(request)?;
    let resolution = ppk::resolve_sources(&[request], &[], std::slice::from_ref(source), "x86_64")?;
    let fetched = ppk::fetch_resolved(&resolution.packages, store)?;
    Ok(fetched.into_iter().map(|each| each.identity).collect())
}
