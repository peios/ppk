use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use ppk::{Constraint, Metadata, PackageId, Request, Version, resolve};
use tempfile::TempDir;

const ARCH: &str = "x86_64";

/// Artifact directories to resolve from.
struct Dirs {
    dir: TempDir,
}

impl Dirs {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, n: usize) -> PathBuf {
        self.dir.path().join(format!("artifacts-{n}"))
    }

    /// Pack `package`, at `name@version`, into the first directory, with
    /// `metadata`. Its one file holds `content`, so that two packs of one
    /// version can differ.
    fn pack(&self, package: &str, metadata: &Metadata) {
        self.pack_into(0, package, metadata, package);
    }

    fn pack_into(&self, n: usize, package: &str, metadata: &Metadata, content: &str) {
        let tree = tempfile::tempdir().unwrap();
        fs::write(tree.path().join("file"), content).unwrap();
        let id = PackageId::new(
            package.split('@').next().unwrap(),
            package.split('@').nth(1).unwrap(),
            ARCH,
        )
        .unwrap();
        ppk::pack_with(&id, metadata, tree.path(), &self.path(n)).unwrap();
    }

    fn resolve(&self, requests: &[Request]) -> anyhow::Result<Vec<String>> {
        self.resolve_from(requests, &[self.path(0)])
    }

    fn resolve_from(&self, requests: &[Request], dirs: &[PathBuf]) -> anyhow::Result<Vec<String>> {
        let resolution = resolve(requests, dirs, ARCH)?;
        Ok(resolution
            .packages
            .iter()
            .map(|p| format!("{}@{}", p.id.name(), p.id.version()))
            .collect())
    }

    fn error(&self, requests: &[Request]) -> String {
        format!("{:#}", self.resolve(requests).unwrap_err())
    }
}

fn package(s: &str) -> Request {
    Request::parse_package(s).unwrap()
}

fn capability(s: &str) -> Request {
    Request::parse_capability(s).unwrap()
}

fn constraint(s: &str) -> Constraint {
    Constraint::from_manifest_string(s).unwrap()
}

fn depends(pairs: &[(&str, &str)]) -> Metadata {
    Metadata {
        dependencies: table(pairs),
        ..Metadata::default()
    }
}

fn requires(pairs: &[(&str, &str)]) -> Metadata {
    Metadata {
        requires: table(pairs),
        ..Metadata::default()
    }
}

fn provides(pairs: &[(&str, &str)]) -> Metadata {
    Metadata {
        provides: pairs
            .iter()
            .map(|(name, version)| {
                let version =
                    (!version.is_empty()).then(|| Version::parse_relaxed(version).unwrap());
                (name.to_string(), version)
            })
            .collect(),
        ..Metadata::default()
    }
}

fn table(pairs: &[(&str, &str)]) -> BTreeMap<String, Constraint> {
    pairs
        .iter()
        .map(|(name, c)| (name.to_string(), constraint(c)))
        .collect()
}

#[test]
fn the_newest_version_that_satisfies_is_chosen() {
    let dirs = Dirs::new();
    for version in ["1.0-1", "1.10-1", "1.9-2", "2.0~rc1-1"] {
        dirs.pack(&format!("org.gnu.make@{version}"), &Metadata::default());
    }
    assert_eq!(
        dirs.resolve(&[package("org.gnu.make")]).unwrap(),
        ["org.gnu.make@2.0~rc1-1"]
    );
    assert_eq!(
        dirs.resolve(&[package("org.gnu.make < 2")]).unwrap(),
        ["org.gnu.make@1.10-1"]
    );
    assert_eq!(
        dirs.resolve(&[package("org.gnu.make<1.10")]).unwrap(),
        ["org.gnu.make@1.9-2"]
    );
    assert_eq!(
        dirs.resolve(&[package("org.gnu.make = 1.0-1")]).unwrap(),
        ["org.gnu.make@1.0-1"]
    );
}

#[test]
fn dependencies_and_requirements_are_followed_into_a_profile() {
    let dirs = Dirs::new();
    dirs.pack("org.gnu.glibc@2.40-1", &provides(&[("libc.so.6", "2.40")]));
    dirs.pack("org.gnu.glibc@2.44-1", &provides(&[("libc.so.6", "2.44")]));
    dirs.pack(
        "org.git.kernel.dash@0.5.13-1",
        &Metadata {
            requires: table(&[("libc.so.6", ">= 2.34")]),
            provides: provides(&[("sh", "")]).provides,
            ..Metadata::default()
        },
    );
    dirs.pack(
        "org.gnu.make@4.4-1",
        &Metadata {
            dependencies: table(&[("org.gnu.glibc", ">= 2.34")]),
            requires: table(&[("sh", "")]),
            ..Metadata::default()
        },
    );
    let resolution = resolve(&[package("org.gnu.make")], &[dirs.path(0)], ARCH).unwrap();
    let chosen: Vec<(String, String)> = resolution
        .packages
        .iter()
        .map(|p| {
            (
                format!("{}@{}", p.id.name(), p.id.version()),
                p.reason.clone(),
            )
        })
        .collect();
    assert_eq!(
        chosen,
        [
            (
                "org.git.kernel.dash@0.5.13-1".to_owned(),
                "org.gnu.make@4.4-1 requires sh".to_owned()
            ),
            (
                "org.gnu.glibc@2.44-1".to_owned(),
                "org.gnu.make@4.4-1 depends on org.gnu.glibc >= 2.34".to_owned()
            ),
            (
                "org.gnu.make@4.4-1".to_owned(),
                "the request for org.gnu.make".to_owned()
            ),
        ]
    );
    assert!(
        resolution
            .packages
            .iter()
            .all(|p| p.artifact_dir == dirs.path(0))
    );
    assert_eq!(
        resolution.profile().render().unwrap(),
        "arch = \"x86_64\"\n\n[packages]\n\"org.git.kernel.dash\" = \"0.5.13-1\"\n\
         \"org.gnu.glibc\" = \"2.44-1\"\n\"org.gnu.make\" = \"4.4-1\"\n"
    );
}

#[test]
fn a_package_provides_its_own_name_at_its_own_version() {
    let dirs = Dirs::new();
    dirs.pack("org.gnu.bash@5.2-1", &Metadata::default());
    dirs.pack("org.gnu.bash@5.3-1", &Metadata::default());
    assert_eq!(
        dirs.resolve(&[capability("org.gnu.bash < 5.3")]).unwrap(),
        ["org.gnu.bash@5.2-1"]
    );
    // A provides entry for its own name satisfies at the entry's version,
    // and its own version still does too (PSPU 5.21).
    let dirs = Dirs::new();
    dirs.pack("org.gnu.bash@5.3-1", &provides(&[("org.gnu.bash", "4")]));
    for request in ["org.gnu.bash < 5", "org.gnu.bash >= 5.3"] {
        assert_eq!(
            dirs.resolve(&[capability(request)]).unwrap(),
            ["org.gnu.bash@5.3-1"],
            "{request}"
        );
    }
    // A dependency is on the package, so its provides entries don't count.
    let err = dirs.error(&[package("org.gnu.bash < 5")]);
    assert_eq!(
        err,
        "the request for org.gnu.bash < 5, but no version satisfies it; there are 5.3-1"
    );
}

#[test]
fn packages_named_are_chosen_before_capabilities_are_settled() {
    let dirs = Dirs::new();
    dirs.pack("org.git.kernel.dash@0.5.13-1", &provides(&[("sh", "")]));
    dirs.pack("net.busybox.busybox@1.37.0-1", &provides(&[("sh", "")]));
    dirs.pack("org.gnu.make@4.4-1", &requires(&[("sh", "")]));
    // Two names provide sh, so the profile must choose one.
    let err = dirs.error(&[package("org.gnu.make")]);
    assert_eq!(
        err,
        "org.gnu.make@4.4-1 requires sh, but net.busybox.busybox, org.git.kernel.dash all \
         provide it; request one of them by name"
    );
    // Named in any order, the package is chosen first and provides it.
    for requests in [
        [capability("sh"), package("org.git.kernel.dash")],
        [package("org.gnu.make"), package("org.git.kernel.dash")],
    ] {
        let chosen = dirs.resolve(&requests).unwrap();
        assert!(
            chosen.contains(&"org.git.kernel.dash@0.5.13-1".to_owned()),
            "{chosen:?}"
        );
        assert!(
            !chosen.iter().any(|p| p.starts_with("net.busybox")),
            "{chosen:?}"
        );
    }
}

#[test]
fn a_conflict_says_what_conflicted_and_whether_backtracking_would_help() {
    let dirs = Dirs::new();
    for version in ["1.0-1", "1.5-1", "2.0-1"] {
        dirs.pack(&format!("org.example.c@{version}"), &Metadata::default());
    }
    dirs.pack(
        "org.example.a@1.0-1",
        &depends(&[("org.example.c", ">= 1")]),
    );
    dirs.pack("org.example.b@1.0-1", &depends(&[("org.example.c", "< 2")]));
    dirs.pack("org.example.d@1.0-1", &depends(&[("org.example.c", "> 2")]));

    // a chooses c 2.0, and b then needs < 2, which 1.5 would have met.
    assert_eq!(
        dirs.error(&[package("org.example.a"), package("org.example.b")]),
        "org.example.b@1.0-1 depends on org.example.c < 2, but org.example.c@2.0-1 was chosen \
         for org.example.a@1.0-1 depends on org.example.c >= 1; org.example.c@1.5-1 would \
         satisfy both, but finding it needs backtracking, which ppk doesn't do"
    );
    // Asked the other way round, greedy succeeds.
    assert_eq!(
        dirs.resolve(&[package("org.example.b"), package("org.example.a")])
            .unwrap(),
        [
            "org.example.a@1.0-1",
            "org.example.b@1.0-1",
            "org.example.c@1.5-1"
        ]
    );
    // When nothing would satisfy both, it says so.
    assert_eq!(
        dirs.error(&[package("org.example.b"), package("org.example.d")]),
        "org.example.d@1.0-1 depends on org.example.c > 2, but org.example.c@1.5-1 was chosen \
         for org.example.b@1.0-1 depends on org.example.c < 2; no version of org.example.c \
         satisfies both"
    );
}

#[test]
fn a_capability_a_choice_provides_at_another_version_conflicts() {
    let dirs = Dirs::new();
    dirs.pack("org.gnu.glibc@2.34-1", &provides(&[("libc.so.6", "2.34")]));
    dirs.pack("org.gnu.glibc@2.44-1", &provides(&[("libc.so.6", "2.44")]));
    dirs.pack(
        "org.example.new@1.0-1",
        &requires(&[("libc.so.6", ">= 2.40")]),
    );
    // The request pins glibc, and new's floor then conflicts with it.
    assert_eq!(
        dirs.error(&[
            package("org.gnu.glibc = 2.34-1"),
            package("org.example.new")
        ]),
        "org.example.new@1.0-1 requires libc.so.6 >= 2.40, but org.gnu.glibc@2.34-1 was chosen \
         for the request for org.gnu.glibc = 2.34-1; no version of org.gnu.glibc satisfies both"
    );

    // A chosen package that provides it in another version only conflicts
    // too, rather than being replaced.
    let dirs = Dirs::new();
    dirs.pack("org.example.lib@1.0-1", &Metadata::default());
    dirs.pack("org.example.lib@2.0-1", &provides(&[("libnew.so.2", "")]));
    dirs.pack("org.example.app@1.0-1", &requires(&[("libnew.so.2", "")]));
    let err = dirs.error(&[package("org.example.lib < 2"), package("org.example.app")]);
    assert!(
        err.starts_with(
            "org.example.app@1.0-1 requires libnew.so.2, but org.example.lib@1.0-1 was chosen"
        ),
        "{err}"
    );
}

#[test]
fn what_cannot_be_found_is_named() {
    let dirs = Dirs::new();
    dirs.pack("org.gnu.make@4.3-1", &provides(&[("make", "4.3")]));
    dirs.pack("org.gnu.make@4.4-1", &provides(&[("make", "4.4")]));
    let dir = dirs.path(0);
    assert_eq!(
        dirs.error(&[package("org.gnu.gcc")]),
        format!(
            "the request for org.gnu.gcc, but no artifact directory holds org.gnu.gcc for \
             x86_64: {}",
            dir.display()
        )
    );
    assert_eq!(
        dirs.error(&[package("org.gnu.make >= 5")]),
        "the request for org.gnu.make >= 5, but no version satisfies it; there are 4.4-1, 4.3-1"
    );
    assert_eq!(
        dirs.error(&[capability("sh")]),
        "the request for sh, but no package provides sh"
    );
    assert_eq!(
        dirs.error(&[capability("make >= 5")]),
        "the request for make >= 5, but no package provides a version that satisfies it; \
         org.gnu.make@4.4-1 provides 4.4, org.gnu.make@4.3-1 provides 4.3"
    );
}

#[test]
fn two_spellings_of_the_chosen_version_are_refused() {
    let dirs = Dirs::new();
    dirs.pack("org.example.x@1.0~a-1", &Metadata::default());
    dirs.pack("org.example.x@1.0~alpha-1", &Metadata::default());
    dirs.pack("org.example.x@0.9-1", &Metadata::default());
    let err = dirs.error(&[package("org.example.x")]);
    assert_eq!(
        err,
        format!(
            "the request for org.example.x, and org.example.x@1.0~a-1 and \
             org.example.x@1.0~alpha-1 are one version spelt two ways, which no constraint can \
             tell apart; remove one of them from {}",
            dirs.path(0).display()
        )
    );
    // Not chosen, they do no harm.
    assert_eq!(
        dirs.resolve(&[package("org.example.x < 1.0~a")]).unwrap(),
        ["org.example.x@0.9-1"]
    );
}

#[test]
fn several_directories_are_one_set_of_packages() {
    let dirs = Dirs::new();
    dirs.pack_into(0, "org.gnu.make@4.3-1", &Metadata::default(), "make");
    dirs.pack_into(1, "org.gnu.make@4.3-1", &Metadata::default(), "make");
    dirs.pack_into(1, "org.gnu.make@4.4-1", &Metadata::default(), "make");
    let both = [dirs.path(0), dirs.path(1)];
    let resolution = resolve(&[package("org.gnu.make")], &both, ARCH).unwrap();
    assert_eq!(resolution.packages[0].id.version(), "4.4-1");
    assert_eq!(resolution.packages[0].artifact_dir, dirs.path(1));
    let resolution = resolve(&[package("org.gnu.make < 4.4")], &both, ARCH).unwrap();
    // The first directory given that holds it.
    assert_eq!(resolution.packages[0].artifact_dir, dirs.path(0));

    // One version that is two packages is refused.
    dirs.pack_into(2, "org.gnu.make@4.3-1", &Metadata::default(), "other");
    let err = format!(
        "{:#}",
        dirs.resolve_from(&[package("org.gnu.make")], &[dirs.path(0), dirs.path(2)])
            .unwrap_err()
    );
    assert!(
        err.starts_with("org.gnu.make@4.3-1?arch=x86_64 is sha256:"),
        "{err}"
    );
    assert!(
        err.contains(&format!("in {}, but sha256:", dirs.path(0).display())),
        "{err}"
    );
}

#[test]
fn only_packages_for_the_architecture_are_chosen() {
    let dirs = Dirs::new();
    dirs.pack("org.gnu.make@4.3-1", &Metadata::default());
    let tree = tempfile::tempdir().unwrap();
    let id = PackageId::new("org.gnu.make", "4.4-1", "aarch64").unwrap();
    ppk::pack(&id, tree.path(), &dirs.path(0)).unwrap();
    assert_eq!(
        dirs.resolve(&[package("org.gnu.make")]).unwrap(),
        ["org.gnu.make@4.3-1"]
    );
    let resolution = resolve(&[package("org.gnu.make")], &[dirs.path(0)], "aarch64").unwrap();
    assert_eq!(resolution.packages[0].id, id);
}

#[test]
fn a_request_is_a_name_and_optionally_a_constraint() {
    for (text, name, constraint) in [
        ("org.gnu.gcc", "org.gnu.gcc", "any"),
        ("org.gnu.gcc >= 16", "org.gnu.gcc", ">= 16"),
        ("org.gnu.gcc>=16,<17", "org.gnu.gcc", ">= 16, < 17"),
        ("  org.gnu.gcc 16.1-1 ", "org.gnu.gcc", "= 16.1-1"),
    ] {
        let request = Request::parse_package(text).unwrap();
        assert_eq!(
            request.wanted,
            ppk::Wanted::Package(name.to_owned()),
            "{text}"
        );
        assert_eq!(request.constraint.to_string(), constraint, "{text}");
    }
    let request = Request::parse_capability("pkgconfig(gtk+-3.0) >= 3").unwrap();
    assert_eq!(
        request.wanted,
        ppk::Wanted::Capability("pkgconfig(gtk+-3.0)".to_owned())
    );

    for (text, expected) in [
        ("", "no name"),
        (">= 1", "no name"),
        ("org.gnu.gcc >=", "invalid constraint"),
        ("sh", "invalid package name"),
    ] {
        let err = format!("{:#}", Request::parse_package(text).unwrap_err());
        assert!(err.contains(expected), "{text}: {err}");
    }
    assert!(Request::parse_capability("-x").is_err());
}
