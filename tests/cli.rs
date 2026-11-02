use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn ppk(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ppk"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn content_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("bin")).unwrap();
    fs::write(dir.path().join("bin/hello"), b"binary").unwrap();
    dir
}

fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn pack_prints_the_identity_and_writes_the_ref() {
    let content = content_dir();
    let store = tempfile::tempdir().unwrap();
    let output = ppk(&[
        "pack",
        "dev.peios.hello@1.0-1?arch=noarch",
        path(content.path()),
        "--artifact-dir",
        path(store.path()),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let identity = stdout(&output);
    assert!(
        identity.starts_with("sha256:") && identity.ends_with('\n'),
        "{identity}"
    );
    let ref_path = store.path().join("refs/dev.peios.hello/1.0-1/noarch");
    assert_eq!(fs::read_to_string(ref_path).unwrap(), identity);

    let local = tempfile::tempdir().unwrap();
    let fetched = ppk(&[
        "fetch",
        "dev.peios.hello@1.0-1?arch=noarch",
        "--from",
        path(store.path()),
        "--artifact-dir",
        path(local.path()),
    ]);
    assert_eq!(fetched.status.code(), Some(0), "{}", stderr(&fetched));
    assert_eq!(stdout(&fetched), identity, "fetch prints the same identity");
}

#[test]
fn fetch_without_a_source_is_a_usage_error() {
    let output = ppk(&["fetch", "dev.peios.hello@1.0-1"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output),
        "ppk: missing option --from <dir>\nTry 'ppk --help' for more information.\n"
    );
}

#[test]
fn compose_env_prints_the_identity_and_lays_out_the_root() {
    let content = content_dir();
    let store = tempfile::tempdir().unwrap();
    let artifacts = store.path().join("artifacts");
    let packed = ppk(&[
        "pack",
        "dev.peios.hello@1.0-1",
        path(content.path()),
        "--artifact-dir",
        path(&artifacts),
    ]);
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));

    let out = store.path().join("root");
    let output = ppk(&[
        "compose-env",
        "-p",
        "dev.peios.hello@1.0-1",
        "--artifact-dir",
        path(&artifacts),
        "--out",
        path(&out),
        "--symlink",
        "init::bin/hello",
        "--dir",
        "proc",
        "--include-artifacts",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with("sha256:"),
        "{}",
        stdout(&output)
    );
    assert_eq!(
        fs::read_link(out.join("init")).unwrap(),
        Path::new("usr/bin/hello")
    );
    assert!(out.join("proc").is_dir());
    assert!(out.join("system/artifacts/refs/dev.peios.hello").is_dir());
}

#[test]
fn compose_env_needs_an_output_directory_and_well_formed_extras() {
    for (args, message) in [
        (
            vec!["compose-env", "-p", "dev.peios.hello@1.0-1"],
            "missing option --out <dir>",
        ),
        (
            vec![
                "compose-env",
                "-p",
                "dev.peios.hello@1.0-1",
                "--out",
                "o",
                "--symlink",
                "init",
            ],
            "init: not <path>:<target>",
        ),
        (
            vec![
                "compose-env",
                "-p",
                "dev.peios.hello@1.0-1",
                "--out",
                "o",
                "--dir",
                "usr/x",
            ],
            "\"usr/x\": inside usr, which the packages provide",
        ),
    ] {
        let output = ppk(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            stderr(&output),
            format!("ppk: {message}\nTry 'ppk --help' for more information.\n"),
            "{args:?}"
        );
    }
}

#[test]
fn closures_default_to_inside_the_artifact_directory() {
    let content = content_dir();
    let store = tempfile::tempdir().unwrap();
    let packed = ppk(&[
        "pack",
        "dev.peios.hello@1.0-1",
        path(content.path()),
        "--artifact-dir",
        path(store.path()),
    ]);
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));

    let output = ppk(&[
        "closure",
        "dev.peios.hello@1.0-1",
        "--artifact-dir",
        path(store.path()),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let identity = stdout(&output);
    let hex = identity.trim_end().strip_prefix("sha256:").unwrap();
    assert!(
        store
            .path()
            .join("closures")
            .join(hex)
            .join("bin/hello")
            .is_file()
    );
}

/// Packs a package `id` holding one file, `bin/<file>`, into `artifacts`.
fn pack_one(id: &str, file: &str, artifacts: &Path) {
    let content = tempfile::tempdir().unwrap();
    fs::create_dir(content.path().join("bin")).unwrap();
    fs::write(content.path().join("bin").join(file), file).unwrap();
    let packed = ppk(&[
        "pack",
        id,
        path(content.path()),
        "--artifact-dir",
        path(artifacts),
    ]);
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));
}

#[test]
fn compose_env_composes_a_profile_and_what_the_options_add() {
    let store = tempfile::tempdir().unwrap();
    let artifacts = store.path().join("artifacts");
    pack_one("dev.peios.hello@1.0-1", "hello", &artifacts);
    pack_one("dev.peios.greet@2.0-1", "greet", &artifacts);
    pack_one("dev.peios.wave@3.0-1", "wave", &artifacts);
    let profile = store.path().join("env.toml");
    fs::write(
        &profile,
        "dirs = [\"proc\"]\n\n[packages]\n\"dev.peios.hello\" = \"1.0-1\"\n\n\
         [symlinks]\n\"init\" = \":bin/hello\"\n",
    )
    .unwrap();

    // One -p takes every package after it.
    let out = store.path().join("root");
    let output = ppk(&[
        "compose-env",
        path(&profile),
        "-p",
        "dev.peios.greet@2.0-1",
        "dev.peios.wave@3.0-1",
        "--artifact-dir",
        path(&artifacts),
        "--out",
        path(&out),
        "--dir",
        "tmp",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    for file in ["hello", "greet", "wave"] {
        assert!(out.join("usr/bin").join(file).is_file(), "{file}");
    }
    assert_eq!(
        fs::read_link(out.join("init")).unwrap(),
        Path::new("usr/bin/hello")
    );
    assert!(out.join("proc").is_dir());
    assert!(out.join("tmp").is_dir());

    // Another version of a package the profile holds is refused.
    let output = ppk(&[
        "compose-env",
        path(&profile),
        "-p",
        "dev.peios.hello@2.0-1",
        "--artifact-dir",
        path(&artifacts),
        "--out",
        path(&store.path().join("other")),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "ppk: dev.peios.hello@2.0-1?arch={arch}: the environment holds \
             dev.peios.hello@1.0-1?arch={arch} already\n",
            arch = ppk::HOST_ARCH
        )
    );
}

#[test]
fn a_profile_can_include_its_artifacts_and_copying_wins() {
    use std::os::unix::fs::MetadataExt;

    let store = tempfile::tempdir().unwrap();
    let artifacts = store.path().join("artifacts");
    pack_one("dev.peios.hello@1.0-1", "hello", &artifacts);
    let profile = store.path().join("env.toml");
    fs::write(
        &profile,
        "include_artifacts = true\n\n[packages]\n\"dev.peios.hello\" = \"1.0-1\"\n",
    )
    .unwrap();
    // The links to the one blob, which hardlinking adds to.
    let blob = fs::read_dir(artifacts.join("blobs/sha256"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let links = || fs::metadata(&blob).unwrap().nlink();
    let compose = |out: &str, extra: &[&str]| {
        let out = store.path().join(out);
        let mut args = vec![
            "compose-env",
            path(&profile),
            "--artifact-dir",
            path(&artifacts),
            "--out",
            path(&out),
        ];
        args.extend(extra);
        let output = ppk(&args);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert!(out.join("system/artifacts/refs/dev.peios.hello").is_dir());
    };

    let before = links();
    compose("included", &[]);
    assert!(links() > before, "the profile's artifacts are hardlinked");
    let included = links();
    // Copying is a way of including, so it wins rather than conflicts.
    compose("copied", &["--include-artifacts", "--copy-artifacts"]);
    assert_eq!(links(), included, "the copy shares nothing with the source");
}

#[test]
fn copy_files_copies_a_closure_and_a_roots_usr() {
    use std::os::unix::fs::MetadataExt;

    let store = tempfile::tempdir().unwrap();
    let artifacts = store.path().join("artifacts");
    pack_one("dev.peios.hello@1.0-1", "hello", &artifacts);
    let closures = store.path().join("closures");
    let out = store.path().join("root");
    let blob = fs::read_dir(artifacts.join("blobs/sha256"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();

    for args in [
        vec![
            "closure",
            "dev.peios.hello@1.0-1",
            "--artifact-dir",
            path(&artifacts),
            "--closure-dir",
            path(&closures),
            "--copy-files",
        ],
        vec![
            "compose-env",
            "-p",
            "dev.peios.hello@1.0-1",
            "--artifact-dir",
            path(&artifacts),
            "--out",
            path(&out),
            "--copy-files",
        ],
    ] {
        let output = ppk(&args);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    }
    assert_eq!(fs::metadata(&blob).unwrap().nlink(), 1, "nothing linked it");
    assert_eq!(
        fs::read(out.join("usr/bin/hello")).unwrap(),
        fs::read(blob).unwrap()
    );
}

#[test]
fn a_name_that_is_not_reverse_dns_is_a_usage_error() {
    let content = content_dir();
    let store = tempfile::tempdir().unwrap();
    let output = ppk(&[
        "pack",
        "hello@1.0-1",
        path(content.path()),
        "--artifact-dir",
        path(store.path()),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).starts_with("ppk: hello@1.0-1: invalid package name 'hello': "),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read_dir(store.path()).unwrap().count(), 0);
}

#[test]
fn pack_records_metadata_from_its_options() {
    let content = content_dir();
    let store = tempfile::tempdir().unwrap();
    let output = ppk(&[
        "pack",
        "org.git.kernel.dash@0.5.13.5-1?arch=x86_64",
        path(content.path()),
        "--artifact-dir",
        path(store.path()),
        "--depends",
        "org.gnu.glibc",
        ">=2.44",
        "--requires",
        "libc.so.6",
        ">= 2.34",
        "--requires",
        "sh",
        "",
        "--provides",
        "sh",
        "1",
        "--claims",
        "/usr/bin/sh",
        "dash",
        "--description",
        "-POSIX shell",
        "--license",
        "BSD-3-Clause",
        "--license-class",
        "free",
        "--homepage",
        "https://git.kernel.org/pub/scm/utils/dash/dash.git",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let identity: ppk::Identity = stdout(&output).trim_end().parse().unwrap();
    let manifest = fs::read_to_string(
        store
            .path()
            .join(format!("manifests/sha256/{}", identity.hex())),
    )
    .unwrap();
    assert!(
        manifest.starts_with(
            "name = \"org.git.kernel.dash\"\nversion = \"0.5.13.5-1\"\narch = \"x86_64\"\n\
             description = \"-POSIX shell\"\n\
             homepage = \"https://git.kernel.org/pub/scm/utils/dash/dash.git\"\n\
             license = \"BSD-3-Clause\"\nlicense_class = \"free\"\n\n\
             [claims]\n\"/usr/bin/sh\" = \"dash\"\n\n\
             [dependencies]\n\"org.gnu.glibc\" = \">= 2.44\"\n\n\
             [provides]\n\"sh\" = \"1\"\n\n\
             [requires]\n\"libc.so.6\" = \">= 2.34\"\n\"sh\" = \"\"\n\n[files]\n"
        ),
        "{manifest}"
    );
}

#[test]
fn malformed_metadata_is_a_usage_error() {
    for (args, message) in [
        (
            vec!["--depends", "org.gnu.glibc", ">= 2.44,"],
            "--depends org.gnu.glibc: constraint '>= 2.44,' has an empty expression",
        ),
        (
            vec!["--depends", "sh", ""],
            "dependencies: invalid package name 'sh': ",
        ),
        (
            vec!["--requires", "a", ""],
            "requires: invalid capability 'a': ",
        ),
        (
            vec!["--provides", "sh", "1.0-0"],
            "--provides sh: revision in '1.0-0' must be a positive integer",
        ),
        (
            vec!["--claims", "usr/bin/sh", "dash"],
            "claims: \"usr/bin/sh\": not a normal absolute path",
        ),
        (
            vec!["--requires", "sh", "", "--requires", "sh", ">= 1"],
            "--requires sh: given more than once",
        ),
        (
            vec!["--depends", "org.gnu.glibc", "--requires", "sh", ""],
            "2 values required for '--depends <name> <constraint>' but 1 was provided",
        ),
        (
            vec!["--license-class", "gratis"],
            "invalid license class 'gratis': ",
        ),
        (
            vec!["--homepage", "ftp://example.org"],
            "homepage: 'ftp://example.org' is not an http or https URL",
        ),
    ] {
        let content = content_dir();
        let store = tempfile::tempdir().unwrap();
        let mut all = vec![
            "pack",
            "dev.peios.hello@1.0-1",
            path(content.path()),
            "--artifact-dir",
            path(store.path()),
        ];
        all.extend(&args);
        let output = ppk(&all);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&output).starts_with(&format!("ppk: {message}")),
            "{args:?}: {}",
            stderr(&output)
        );
        assert_eq!(fs::read_dir(store.path()).unwrap().count(), 0);
    }
}

#[test]
fn run_needs_packages() {
    for (args, missing) in [
        (vec!["run"], "<profile>... or -p <package>..."),
        (vec!["run", "--", "true"], "<profile>... or -p <package>..."),
    ] {
        let output = ppk(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            stderr(&output),
            format!("ppk: missing {missing}\nTry 'ppk --help' for more information.\n"),
            "{args:?}"
        );
    }
}

#[test]
fn closure_without_packages_is_a_usage_error() {
    let output = ppk(&["closure"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output),
        "ppk: missing argument <package>...\nTry 'ppk --help' for more information.\n"
    );
}

#[test]
fn malformed_package_is_a_usage_error() {
    let content = content_dir();
    let output = ppk(&["pack", "dev.peios.hello", path(content.path())]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output),
        "ppk: dev.peios.hello: missing '@<version>'\n\
         Try 'ppk --help' for more information.\n"
    );
}

#[test]
fn unknown_option_is_a_usage_error() {
    let output = ppk(&["pack", "--jsno"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output),
        "ppk: unknown option '--jsno'\nTry 'ppk --help' for more information.\n"
    );
}

#[test]
fn missing_path_is_a_usage_error() {
    let output = ppk(&["pack", "dev.peios.hello@1.0-1"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output),
        "ppk: missing argument <path>\nTry 'ppk --help' for more information.\n"
    );
}

#[test]
fn no_arguments_prints_help_to_stderr() {
    let output = ppk(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).starts_with("Create and manage Peios packages\n"));
}

#[test]
fn help_command_prints_the_same_text_as_the_flag() {
    for (command, flag) in [
        (vec!["help"], vec!["--help"]),
        (vec!["help", "pack"], vec!["pack", "--help"]),
        (vec!["help", "help"], vec!["help", "--help"]),
    ] {
        let by_command = ppk(&command);
        assert_eq!(by_command.status.code(), Some(0), "{command:?}");
        let text = stdout(&by_command);
        assert_eq!(text, stdout(&ppk(&flag)), "{command:?}");
        assert!(text.ends_with("See ppk(1).\n"), "{command:?}: {text}");
    }
}

#[test]
fn unknown_command_is_a_usage_error() {
    for args in [vec!["stauts"], vec!["help", "stauts"]] {
        let output = ppk(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            stderr(&output),
            "ppk: unknown command 'stauts'\nTry 'ppk --help' for more information.\n",
            "{args:?}"
        );
    }
}

#[test]
fn help_lines_fit_in_80_columns() {
    for args in [
        vec!["--help"],
        vec!["help", "pack"],
        vec!["help", "fetch"],
        vec!["help", "closure"],
        vec!["help", "compose-env"],
        vec!["help", "run"],
        vec!["help", "help"],
    ] {
        let text = stdout(&ppk(&args));
        for line in text.lines() {
            assert!(line.chars().count() <= 80, "{args:?}: {line}");
        }
    }
}

#[test]
fn help_and_version_go_to_stdout() {
    let help = ppk(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout(&help).contains("pack"), "{}", stdout(&help));

    let pack_help = ppk(&["pack", "--help"]);
    assert_eq!(pack_help.status.code(), Some(0));
    let text = stdout(&pack_help);
    assert!(
        text.contains("Usage: ppk pack [options] <package> <path>"),
        "{text}"
    );
    let short_help = ppk(&["pack", "-h"]);
    assert_eq!(
        stdout(&short_help),
        text,
        "-h and --help print the same text"
    );
    assert!(
        text.contains(&format!("(default: {})", ppk::DEFAULT_ARTIFACT_DIR)),
        "{text}"
    );

    let version = ppk(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        stdout(&version),
        format!("ppk {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn failure_is_one_line_without_an_os_error_number() {
    let store = tempfile::tempdir().unwrap();
    let missing = store.path().join("missing");
    let output = ppk(&[
        "pack",
        "dev.peios.hello@1.0-1",
        path(&missing),
        "--artifact-dir",
        path(store.path()),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!("ppk: {}: No such file or directory\n", missing.display())
    );
}

#[test]
fn an_os_error_number_is_removed_from_the_middle_of_a_message() {
    // tempfile's errors carry " at path ..." after the error number.
    let content = content_dir();
    let dir = tempfile::tempdir().unwrap();
    let artifacts = dir.path().join("artifacts");
    let packed = ppk(&[
        "pack",
        "dev.peios.hello@1.0-1",
        path(content.path()),
        "--artifact-dir",
        path(&artifacts),
    ]);
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));
    let missing = dir.path().join("missing");

    let output = ppk(&[
        "compose-env",
        "-p",
        "dev.peios.hello@1.0-1",
        "--artifact-dir",
        path(&artifacts),
        "--out",
        path(&missing.join("root")),
        "--copy-artifacts",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let message = stderr(&output);
    assert!(
        message.starts_with(&format!(
            "ppk: {}: cannot create a temporary directory: No such file or directory at path ",
            missing.display()
        )) && !message.contains("os error"),
        "{message}"
    );
}

#[test]
fn replacing_a_packed_version_fails() {
    let store = tempfile::tempdir().unwrap();
    let pack = |content: &Path| {
        ppk(&[
            "pack",
            "dev.peios.hello@1.0-1?arch=noarch",
            path(content),
            "--artifact-dir",
            path(store.path()),
        ])
    };
    let content = content_dir();
    assert_eq!(pack(content.path()).status.code(), Some(0));
    fs::write(content.path().join("bin/hello"), b"changed").unwrap();
    let output = pack(content.path());
    assert_eq!(output.status.code(), Some(1));
    let message = stderr(&output);
    assert!(
        message.starts_with("ppk: dev.peios.hello@1.0-1?arch=noarch: already packed as sha256:"),
        "{message}"
    );
    assert_eq!(message.lines().count(), 1, "{message}");
}

/// Pack a package holding `bin/<file>`, with the options `extra`.
fn pack_one_with(id: &str, file: &str, artifacts: &Path, extra: &[&str]) {
    let content = tempfile::tempdir().unwrap();
    fs::create_dir(content.path().join("bin")).unwrap();
    fs::write(content.path().join("bin").join(file), file).unwrap();
    let mut args = vec![
        "pack",
        id,
        path(content.path()),
        "--artifact-dir",
        path(artifacts),
    ];
    args.extend_from_slice(extra);
    let packed = ppk(&args);
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));
}

#[test]
fn resolve_writes_a_profile_that_compose_env_lays_out_with_its_claims() {
    let store = tempfile::tempdir().unwrap();
    let artifacts = store.path().join("artifacts");
    pack_one_with("org.gnu.glibc@2.40-1", "libc", &artifacts, &[]);
    pack_one_with("org.gnu.glibc@2.44-1", "libc", &artifacts, &[]);
    pack_one_with(
        "org.git.kernel.dash@0.5.13-1",
        "dash",
        &artifacts,
        &["--provides", "sh", "", "--claims", "/usr/bin/sh", "dash"],
    );
    pack_one_with(
        "org.gnu.make@4.4-1",
        "make",
        &artifacts,
        &[
            "--depends",
            "org.gnu.glibc",
            ">= 2.34",
            "--requires",
            "sh",
            "",
        ],
    );

    let output = ppk(&["resolve", "-p", "org.gnu.make", "--from", path(&artifacts)]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let expected = format!(
        "arch = \"{}\"\n\n[packages]\n\"org.git.kernel.dash\" = \"0.5.13-1\"\n\
         \"org.gnu.glibc\" = \"2.44-1\"\n\"org.gnu.make\" = \"4.4-1\"\n",
        ppk::HOST_ARCH
    );
    assert_eq!(stdout(&output), expected);

    let profile = store.path().join("root.toml");
    let output = ppk(&[
        "resolve",
        "-c",
        "sh",
        "-p",
        "org.gnu.make",
        "org.gnu.glibc < 2.44",
        "--from",
        path(&artifacts),
        "--out",
        path(&profile),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
    assert_eq!(
        fs::read_to_string(&profile).unwrap(),
        expected.replace("2.44-1", "2.40-1")
    );

    let layout = store.path().join("layout.toml");
    fs::write(
        &layout,
        "dirs = [\"tmp\"]\n\n[symlinks]\n\"bin\" = \":bin\"\n",
    )
    .unwrap();
    let out = store.path().join("root");
    let output = ppk(&[
        "compose-env",
        path(&profile),
        path(&layout),
        "--artifact-dir",
        path(&artifacts),
        "--out",
        path(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        fs::read_link(out.join("usr/bin/sh")).unwrap(),
        Path::new("dash")
    );
    assert_eq!(fs::read_to_string(out.join("bin/sh")).unwrap(), "dash");
    assert!(out.join("tmp").is_dir());
}

#[test]
fn resolve_fails_on_a_conflict_and_refuses_bad_requests() {
    let store = tempfile::tempdir().unwrap();
    let artifacts = store.path().join("artifacts");
    pack_one_with("org.gnu.glibc@2.44-1", "libc", &artifacts, &[]);
    let out = store.path().join("root.toml");
    let output = ppk(&[
        "resolve",
        "-p",
        "org.gnu.glibc < 2",
        "--from",
        path(&artifacts),
        "--out",
        path(&out),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "ppk: the request for org.gnu.glibc < 2, but no version satisfies it; there are 2.44-1\n"
    );
    assert!(!out.exists());

    for (args, message) in [
        (
            vec!["resolve", "--from", "x"],
            "missing -p <request>... or -c <request>...",
        ),
        (
            vec!["resolve", "-p", "org.gnu.glibc"],
            "missing option --from <dir>...",
        ),
        (
            vec!["resolve", "-c", "sh >", "--from", "x"],
            "-c: 'sh >': invalid constraint '>': invalid upstream in '': empty upstream version",
        ),
    ] {
        let output = ppk(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            stderr(&output),
            format!("ppk: {message}\nTry 'ppk --help' for more information.\n"),
            "{args:?}"
        );
    }
}
