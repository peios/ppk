//! Repositories served over HTTP: a repository's files, read from one or
//! more base URLs, each a copy of the same repository, tried in order.
//!
//! A file is read from the first base URL that serves it. One that can't be
//! reached, or answers with anything but the file, such as `404 Not Found`,
//! is passed over for the next. What a base URL serves is never trusted for
//! itself: the pointer file is checked against the repository's key, and
//! everything below it against its hash, and a file that fails its check
//! is an error, not a reason to try the next base URL. The pointer file and
//! its signature are read from the same base URL, so that a publication
//! between the two reads can't mix two of them.
//!
//! Tree nodes and manifests read are kept in a cache directory, under the
//! paths a repository gives them, and checked against their hash again
//! whenever they're read from it. Blobs aren't kept: fetching one writes it
//! straight into the store.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::repo::{self, Pointer, Trust};
use crate::{ALGORITHM, Identity};

/// How long to wait to connect to a server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait for a server to answer a request it has been sent.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a server may send nothing while it sends a file.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The most a pointer file or its signature may be.
const SMALL_LIMIT: u64 = 1 << 20;

/// The most a tree node or a manifest may be.
const OBJECT_LIMIT: u64 = 1 << 30;

/// Whether `s` is an HTTP or HTTPS URL, rather than a path.
pub fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// A repository served over HTTP.
#[derive(Clone)]
pub struct Remote {
    /// Its base URLs, in the order they're tried, each ending in `/`.
    urls: Vec<String>,
    /// What its pointer file's signature must show, if it must be signed.
    /// Behind an `Arc`, since a remote is cloned into everything resolved
    /// from it.
    trust: Option<Arc<Trust>>,
    cache: Option<PathBuf>,
    agent: ureq::Agent,
    /// Its pointer file, once read: a run reads it once, so that everything
    /// it does follows one publication.
    pointer: Arc<Mutex<Option<Option<Pointer>>>>,
}

impl fmt::Debug for Remote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Remote")
            .field("urls", &self.urls)
            .field("trust", &self.trust)
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Remote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.urls.join(" "))
    }
}

impl Remote {
    /// The repository served at the base URLs `urls`, tried in order. Each
    /// is an `http` or `https` URL, with no query or fragment.
    pub fn new(urls: &[String]) -> Result<Self> {
        if urls.is_empty() {
            bail!("a repository served over HTTP needs at least one URL");
        }
        let urls = urls
            .iter()
            .map(|url| base_url(url))
            .collect::<Result<_>>()?;
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .timeout_per_read(Some(READ_TIMEOUT))
            .user_agent(concat!("ppk/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        Ok(Self {
            urls,
            trust: None,
            cache: None,
            agent,
            pointer: Arc::new(Mutex::new(None)),
        })
    }

    /// The same repository, whose pointer file must be signed as `trust`
    /// requires.
    pub fn with_trust(mut self, trust: Option<Trust>) -> Self {
        self.trust = trust.map(Arc::new);
        self.pointer = Arc::new(Mutex::new(None));
        self
    }

    /// The same repository, keeping the tree nodes and manifests it reads
    /// in `cache`.
    pub fn with_cache(mut self, cache: &Path) -> Self {
        self.cache = Some(cache.to_owned());
        self
    }

    /// Its base URLs, in the order they're tried.
    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    /// What its pointer file's signature must show, if it must be signed.
    pub fn trust(&self) -> Option<&Trust> {
        self.trust.as_deref()
    }

    /// Its pointer file, checked against what it requires of its signature, if anything, or `None`
    /// if every base URL says it has none. Read once, and then remembered.
    pub fn pointer(&self) -> Result<Option<Pointer>> {
        let mut held = self.pointer.lock().expect("no reader panics holding it");
        if let Some(pointer) = &*held {
            return Ok(pointer.clone());
        }
        let pointer = self.read_pointer()?;
        *held = Some(pointer.clone());
        Ok(pointer)
    }

    fn read_pointer(&self) -> Result<Option<Pointer>> {
        let mut failures = Vec::new();
        for base in &self.urls {
            let url = format!("{base}{}", repo::POINTER);
            let bytes = match self.get(&url, SMALL_LIMIT) {
                Ok(bytes) => bytes,
                Err(failure) => {
                    failures.push(failure);
                    continue;
                }
            };
            let signature = match &self.trust {
                None => None,
                Some(_) => match self.get(&format!("{base}{}", repo::SIGNATURE), SMALL_LIMIT) {
                    Ok(signature) => Some(signature),
                    // A pointer file without its signature is a repository,
                    // not the lack of one.
                    Err(failure) => {
                        failures.push(Failure {
                            missing: false,
                            ..failure
                        });
                        continue;
                    }
                },
            };
            return repo::check_pointer(bytes, signature.as_deref(), self.trust(), &url).map(Some);
        }
        if failures.iter().all(|failure| failure.missing) {
            return Ok(None);
        }
        bail!("{}", unavailable(repo::POINTER, &failures))
    }

    /// The tree node or manifest `identity`, under `dir` (`trees` or
    /// `manifests`), checked against its hash.
    pub(crate) fn object(&self, dir: &str, identity: &Identity) -> Result<Vec<u8>> {
        let path = format!("{dir}/{ALGORITHM}/{}", identity.hex());
        let cached = self.cache.as_ref().map(|cache| cache.join(&path));
        if let Some(cached) = &cached
            && let Ok(bytes) = fs::read(cached)
            && Identity::of(&bytes) == *identity
        {
            return Ok(bytes);
        }
        let mut failures = Vec::new();
        for base in &self.urls {
            let url = format!("{base}{path}");
            let bytes = match self.get(&url, OBJECT_LIMIT) {
                Ok(bytes) => bytes,
                Err(failure) => {
                    failures.push(failure);
                    continue;
                }
            };
            if Identity::of(&bytes) != *identity {
                bail!("{url}: does not match its hash");
            }
            if let Some(cached) = &cached {
                keep(cached, &bytes)?;
            }
            return Ok(bytes);
        }
        bail!("{}", unavailable(&path, &failures))
    }

    /// A reader of the file at `path` below the base URLs, from the first
    /// that serves it, and the URL it is read from. Nothing checks what it
    /// reads; the caller checks it against its hash.
    pub(crate) fn open(&self, path: &str) -> Result<(String, ureq::BodyReader<'static>)> {
        let mut failures = Vec::new();
        for base in &self.urls {
            let url = format!("{base}{path}");
            match self.request(&url) {
                Ok(response) => return Ok((url, response.into_body().into_reader())),
                Err(failure) => failures.push(failure),
            }
        }
        bail!("{}", unavailable(path, &failures))
    }

    /// The file at `url`, of at most `limit` bytes.
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, Failure> {
        let response = self.request(url)?;
        (response.into_body().into_with_config().limit(limit))
            .read_to_vec()
            .map_err(|err| Failure::new(url, err.to_string(), false))
    }

    /// The response to a request for `url`, if it is the file.
    fn request(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, Failure> {
        let response = (self.agent.get(url).call())
            .map_err(|err| Failure::new(url, err.to_string(), false))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let missing = status == ureq::http::StatusCode::NOT_FOUND;
        Err(Failure::new(url, status.to_string(), missing))
    }
}

/// Why a base URL didn't serve a file.
struct Failure {
    url: String,
    reason: String,
    /// Whether the server said it has no such file.
    missing: bool,
}

impl Failure {
    fn new(url: &str, reason: String, missing: bool) -> Self {
        Self {
            url: url.to_owned(),
            reason,
            missing,
        }
    }
}

/// The error for `path`, which no base URL served, saying why each didn't.
fn unavailable(path: &str, failures: &[Failure]) -> String {
    let reasons: Vec<String> = (failures.iter())
        .map(|failure| format!("{}: {}", failure.url, failure.reason))
        .collect();
    format!(
        "{path}: no URL of the repository serves it ({})",
        reasons.join("; ")
    )
}

/// `url` as a base URL: checked, and ending in `/`.
fn base_url(url: &str) -> Result<String> {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        bail!("{url}: not an http or https URL");
    };
    if rest.is_empty() || rest.starts_with('/') {
        bail!("{url}: no host");
    }
    if url.contains(['?', '#']) || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("{url}: a repository's URL has no query, fragment or spaces");
    }
    Ok(match url.ends_with('/') {
        true => url.to_owned(),
        false => format!("{url}/"),
    })
}

/// Keep `bytes`, already checked, at `path` in the cache, through a
/// temporary file beside it, so a reader sees all of it or none.
fn keep(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().expect("a cached file is in a directory");
    fs::create_dir_all(dir)
        .with_context(|| format!("{}: cannot create the cache", dir.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".tmp-")
        .tempfile_in(dir)
        .with_context(|| format!("{}: cannot write to the cache", dir.display()))?;
    tmp.write_all(bytes)
        .with_context(|| format!("{}", path.display()))?;
    match tmp.persist(path) {
        Ok(_) => Ok(()),
        Err(err) if err.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err.error).with_context(|| format!("{}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_url_ends_in_a_slash_and_has_no_query() {
        assert_eq!(
            base_url("https://pkgs.peios.org").unwrap(),
            "https://pkgs.peios.org/"
        );
        assert_eq!(
            base_url("http://cache.lan/peios/").unwrap(),
            "http://cache.lan/peios/"
        );
        for bad in [
            "ftp://pkgs.peios.org/",
            "https://",
            "https:///peios",
            "https://pkgs.peios.org/?x=1",
            "https://pkgs.peios.org/#top",
            "https://pkgs peios.org/",
        ] {
            assert!(base_url(bad).is_err(), "{bad}");
        }
    }
}
