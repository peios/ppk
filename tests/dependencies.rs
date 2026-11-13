use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn ppk(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ppk"))
        .args(args)
        .output()
        .unwrap()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn pack(store: &Path, id: &str, file: &str, extra: &[&str]) {
    let tree = tempfile::tempdir().unwrap();
    fs::write(tree.path().join(file), file).unwrap();
    let mut args = vec!["pack", id, path(tree.path()), "--artifact-dir", path(store)];
    args.extend_from_slice(extra);
    let output = ppk(&args);
    assert!(output.status.success(), "{}", stderr(&output));
}

fn fixture(store: &Path) {
    pack(store, "dev.peios.base@1.0-1", "base", &[]);
    pack(
        store,
        "dev.peios.runtime@1.0-1",
        "runtime",
        &[
            "--provides",
            "test-runtime",
            "1",
            "--depends",
            "dev.peios.base",
            "",
        ],
    );
    pack(
        store,
        "dev.peios.app@1.0-1",
        "app",
        &["--requires", "test-runtime", ">= 1"],
    );
    pack(store, "dev.peios.app@2.0-1", "new-app", &[]);
}

#[test]
fn fetch_resolves_constraints_dependencies_and_capabilities_and_skips_present_packages() {
    use std::os::unix::fs::MetadataExt;

    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    let local = work.path().join("local");
    fixture(&source);
    let args = [
        "fetch",
        "dev.peios.app < 2",
        "--from",
        path(&source),
        "--artifact-dir",
        path(&local),
    ];
    let output = ppk(&args);
    assert!(output.status.success(), "{}", stderr(&output));
    let packages = ppk::stored_packages(&local).unwrap();
    assert_eq!(packages.len(), 3);
    let expected: String = packages
        .iter()
        .map(|(_, identity)| format!("{identity}\n"))
        .collect();
    assert_eq!(stdout(&output), expected);
    assert!(
        packages
            .iter()
            .any(|(id, _)| id.name() == "dev.peios.app" && id.version() == "1.0-1")
    );
    let reference = local.join(format!("refs/dev.peios.app/1.0-1/{}", ppk::HOST_ARCH));
    let before = fs::metadata(&reference).unwrap();
    let output = ppk(&args);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
    let after = fs::metadata(&reference).unwrap();
    assert_eq!(
        (before.ino(), before.mtime(), before.mtime_nsec()),
        (after.ino(), after.mtime(), after.mtime_nsec())
    );
}

#[test]
fn fetch_bare_names_choose_newest_and_exact_ids_preserve_version_spelling_and_arch() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    fixture(&source);
    for (request, extra, name, version, arch) in [
        (
            "dev.peios.app",
            vec![],
            "dev.peios.app",
            "2.0-1",
            ppk::HOST_ARCH,
        ),
        (
            "dev.peios.app@1.0-1",
            vec![],
            "dev.peios.app",
            "1.0-1",
            ppk::HOST_ARCH,
        ),
        (
            "dev.peios.alias@1.0~a-1?arch=noarch",
            vec![],
            "dev.peios.alias",
            "1.0~a-1",
            "noarch",
        ),
        (
            "dev.peios.alias",
            vec!["--arch", "noarch"],
            "dev.peios.alias",
            "1.0~a-1",
            "noarch",
        ),
    ] {
        pack(&source, "dev.peios.alias@1.0~a-1?arch=noarch", "alias", &[]);
        let local = tempfile::tempdir().unwrap();
        let mut args = vec![
            "fetch",
            request,
            "--from",
            path(&source),
            "--artifact-dir",
            path(local.path()),
        ];
        args.extend(extra);
        let output = ppk(&args);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(
            ppk::packed(
                &ppk::PackageId::new(name, version, arch).unwrap(),
                local.path()
            )
            .unwrap()
            .is_some()
        );
    }
    pack(
        &source,
        "dev.peios.alias@1.0~alpha-1?arch=noarch",
        "other-alias",
        &[],
    );
    let local = tempfile::tempdir().unwrap();
    let output = ppk(&[
        "fetch",
        "dev.peios.alias@1.0~a-1?arch=noarch",
        "--from",
        path(&source),
        "--artifact-dir",
        path(local.path()),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ppk::stored_packages(local.path()).unwrap().len(), 1);
}

#[test]
fn missing_dependencies_fail_before_fetching_and_no_deps_keeps_exact_id_forms() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    let local = work.path().join("local");
    pack(
        &source,
        "dev.peios.app@1.0-1",
        "app",
        &["--depends", "dev.peios.missing", ">= 1"],
    );
    let args = [
        "fetch",
        "dev.peios.app@1.0-1",
        "--from",
        path(&source),
        "--artifact-dir",
        path(&local),
    ];
    let output = ppk(&args);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("dev.peios.app@1.0-1 depends on dev.peios.missing >= 1"));
    assert!(!local.exists());
    let mut exact = args.to_vec();
    exact.push("--no-deps");
    let output = ppk(&exact);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ppk::stored_packages(&local).unwrap().len(), 1);
    for command in ["fetch", "run"] {
        let mut args = vec![command, "--no-deps", "--artifact-dir", path(&local)];
        if command == "fetch" {
            args.extend(["dev.peios.app", "--from", path(&source)]);
        } else {
            args.extend(["-p", "dev.peios.app"]);
        }
        let output = ppk(&args);
        assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
        assert!(stderr(&output).contains("missing '@<version>'"));
    }
}

/// The host need not allow a mount namespace. Inspect what run composed
/// before entering it; actual execution is also checked in a Peios guest.
fn run_inventory(args: &[&str], closures: &Path) -> Vec<ppk::PackageId> {
    let output = ppk(args);
    assert!(
        output.status.code() == Some(127)
            || (output.status.code() == Some(1)
                && stderr(&output).contains("cannot create a mount namespace")),
        "{}",
        stderr(&output)
    );
    let tree = fs::read_dir(closures)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    ppk::read_inventory(&tree)
        .unwrap()
        .unwrap()
        .packages
        .into_iter()
        .map(|package| package.id)
        .collect()
}

#[test]
fn run_resolves_names_constraints_and_profiles_but_no_deps_stays_exact() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    fixture(&store);
    let profile = work.path().join("env.toml");
    fs::write(&profile, "[packages]\n\"dev.peios.app\" = \"1.0-1\"\n").unwrap();
    for (i, selection, expected) in [
        (0, vec!["-p", "dev.peios.app"], 1),
        (1, vec!["-p", "dev.peios.app < 2"], 3),
        (2, vec!["--no-deps", "-p", "dev.peios.app@1.0-1"], 1),
        (3, vec![path(&profile)], 3),
        (4, vec![path(&profile), "-p", "dev.peios.app"], 3),
    ] {
        let closures = work.path().join(format!("closures-{i}"));
        let mut args = vec![
            "run",
            "--artifact-dir",
            path(&store),
            "--closure-dir",
            path(&closures),
        ];
        args.extend(selection);
        args.extend(["--", "/usr/ppk-test-missing"]);
        let packages = run_inventory(&args, &closures);
        assert_eq!(packages.len(), expected);
        let app = packages
            .iter()
            .find(|id| id.name() == "dev.peios.app")
            .unwrap();
        assert_eq!(app.version(), if i == 0 { "2.0-1" } else { "1.0-1" });
    }
    let output = ppk(&[
        "run",
        path(&profile),
        "-p",
        "dev.peios.app >= 2",
        "--artifact-dir",
        path(&store),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("dev.peios.app >= 2"));
    assert!(stderr(&output).contains("dev.peios.app@1.0-1 was chosen"));
}

#[test]
fn run_reports_missing_requirements_without_composing() {
    let work = tempfile::tempdir().unwrap();
    pack(
        work.path(),
        "dev.peios.app@1.0-1",
        "app",
        &["--requires", "missing-runtime", ""],
    );
    let output = ppk(&[
        "run",
        "-p",
        "dev.peios.app",
        "--artifact-dir",
        path(work.path()),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains(
        "dev.peios.app@1.0-1 requires missing-runtime, but no package provides missing-runtime"
    ));
    assert!(!work.path().join("closures").exists());
}

/// Each line of the inventory at `usr`, as its package's name and its
/// brackets.
fn wanted_by(usr: &Path) -> Vec<(String, String)> {
    let text = fs::read_to_string(usr.join("inventory")).unwrap();
    text.lines()
        .skip(1)
        .map(|line| {
            let (name, rest) = line.split_once('@').unwrap();
            let (_, brackets) = rest.split_once(" [").unwrap();
            (name.to_owned(), format!("[{brackets}"))
        })
        .collect()
}

#[test]
fn the_inventory_records_what_wants_each_package_and_the_requests_name_the_closure() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    fixture(&store);
    let requests = [ppk::Request::parse_package("dev.peios.app < 2").unwrap()];
    let resolved = ppk::resolve(&requests, std::slice::from_ref(&store), ppk::HOST_ARCH).unwrap();
    let packages: Vec<_> = resolved.packages.iter().map(|p| p.id.clone()).collect();
    let closures = work.path().join("closures");
    let closed = ppk::closure_for(
        &packages,
        &requests,
        &store,
        &closures,
        ppk::UsrFiles::Hardlink,
    )
    .unwrap();
    assert_eq!(
        wanted_by(&closed.tree_path),
        [
            ("dev.peios.app", "[* <2]"),
            ("dev.peios.base", "[dev.peios.runtime]"),
            ("dev.peios.runtime", "[dev.peios.app test-runtime >=1]"),
        ]
        .map(|(name, brackets)| (name.to_owned(), brackets.to_owned()))
    );

    // The same packages asked for differently are another closure.
    let exact = ppk::closure(&packages, &store, &closures).unwrap();
    assert_ne!(exact.identity, closed.identity);
    assert_eq!(
        wanted_by(&exact.tree_path),
        [
            ("dev.peios.app", "[* =1.0-1]"),
            ("dev.peios.base", "[* =1.0-1] [dev.peios.runtime]"),
            (
                "dev.peios.runtime",
                "[* =1.0-1] [dev.peios.app test-runtime >=1]"
            ),
        ]
        .map(|(name, brackets)| (name.to_owned(), brackets.to_owned()))
    );

    // Resolving the same requests again gives the same closure.
    let again = ppk::resolve(&requests, std::slice::from_ref(&store), ppk::HOST_ARCH).unwrap();
    let again: Vec<_> = again.packages.iter().map(|p| p.id.clone()).collect();
    let reused = ppk::closure_for(
        &again,
        &requests,
        &store,
        &closures,
        ppk::UsrFiles::Hardlink,
    )
    .unwrap();
    assert!(reused.reused);
    assert_eq!(reused.identity, closed.identity);
}

#[test]
fn every_package_of_a_closure_must_be_wanted_and_every_request_satisfied() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    fixture(&store);
    let resolved = ppk::resolve(
        &[ppk::Request::parse_package("dev.peios.app < 2").unwrap()],
        std::slice::from_ref(&store),
        ppk::HOST_ARCH,
    )
    .unwrap();
    let packages: Vec<_> = resolved.packages.iter().map(|p| p.id.clone()).collect();
    let closures = work.path().join("closures");
    let only_base = [ppk::Request::parse_package("dev.peios.base").unwrap()];
    let err = ppk::closure_for(
        &packages,
        &only_base,
        &store,
        &closures,
        ppk::UsrFiles::Hardlink,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("nothing asks for it"),
        "{err:#}"
    );
    let unmet = [
        ppk::Request::parse_package("dev.peios.app < 2").unwrap(),
        ppk::Request::parse_capability("sh").unwrap(),
    ];
    let err = ppk::closure_for(
        &packages,
        &unmet,
        &store,
        &closures,
        ppk::UsrFiles::Hardlink,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("sh: no package of the closure satisfies it"),
        "{err:#}"
    );
}

#[test]
fn compose_env_resolves_a_profile_of_requests_and_records_them() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    fixture(&store);
    let profile = work.path().join("env.toml");
    fs::write(&profile, "[packages]\n\"dev.peios.app\" = \"< 2\"\n").unwrap();
    let root = work.path().join("root");
    let output = ppk(&[
        "compose-env",
        path(&profile),
        "--artifact-dir",
        path(&store),
        "--out",
        path(&root),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        wanted_by(&root.join("usr")),
        [
            ("dev.peios.app", "[* <2]"),
            ("dev.peios.base", "[dev.peios.runtime]"),
            ("dev.peios.runtime", "[dev.peios.app test-runtime >=1]"),
        ]
        .map(|(name, brackets)| (name.to_owned(), brackets.to_owned()))
    );
}
