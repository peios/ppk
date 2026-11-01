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
        "dev.peios.hello@1.0?arch=noarch",
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
    let ref_path = store.path().join("refs/dev.peios.hello/1.0/noarch");
    assert_eq!(fs::read_to_string(ref_path).unwrap(), identity);

    let local = tempfile::tempdir().unwrap();
    let fetched = ppk(&[
        "fetch",
        "dev.peios.hello@1.0?arch=noarch",
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
    let output = ppk(&["fetch", "dev.peios.hello@1.0"]);
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
        "dev.peios.hello@1.0",
        path(content.path()),
        "--artifact-dir",
        path(&artifacts),
    ]);
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));

    let out = store.path().join("root");
    let output = ppk(&[
        "compose-env",
        "-p",
        "dev.peios.hello@1.0",
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
            vec!["compose-env", "-p", "dev.peios.hello@1.0"],
            "missing option --out <dir>",
        ),
        (
            vec![
                "compose-env",
                "-p",
                "dev.peios.hello@1.0",
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
                "dev.peios.hello@1.0",
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
        "dev.peios.hello@1.0",
        path(content.path()),
        "--artifact-dir",
        path(store.path()),
    ]);
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));

    let output = ppk(&[
        "closure",
        "dev.peios.hello@1.0",
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
    pack_one("dev.peios.hello@1.0", "hello", &artifacts);
    pack_one("dev.peios.greet@2.0", "greet", &artifacts);
    pack_one("dev.peios.wave@3.0", "wave", &artifacts);
    let profile = store.path().join("env.toml");
    fs::write(
        &profile,
        "dirs = [\"proc\"]\n\n[packages]\n\"dev.peios.hello\" = \"1.0\"\n\n\
         [symlinks]\n\"init\" = \":bin/hello\"\n",
    )
    .unwrap();

    // One -p takes every package after it.
    let out = store.path().join("root");
    let output = ppk(&[
        "compose-env",
        path(&profile),
        "-p",
        "dev.peios.greet@2.0",
        "dev.peios.wave@3.0",
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
        "dev.peios.hello@2.0",
        "--artifact-dir",
        path(&artifacts),
        "--out",
        path(&store.path().join("other")),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "ppk: dev.peios.hello@2.0?arch={arch}: the environment holds \
             dev.peios.hello@1.0?arch={arch} already\n",
            arch = ppk::HOST_ARCH
        )
    );
}

#[test]
fn a_profile_can_include_its_artifacts_and_copying_wins() {
    use std::os::unix::fs::MetadataExt;

    let store = tempfile::tempdir().unwrap();
    let artifacts = store.path().join("artifacts");
    pack_one("dev.peios.hello@1.0", "hello", &artifacts);
    let profile = store.path().join("env.toml");
    fs::write(
        &profile,
        "include_artifacts = true\n\n[packages]\n\"dev.peios.hello\" = \"1.0\"\n",
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
    pack_one("dev.peios.hello@1.0", "hello", &artifacts);
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
            "dev.peios.hello@1.0",
            "--artifact-dir",
            path(&artifacts),
            "--closure-dir",
            path(&closures),
            "--copy-files",
        ],
        vec![
            "compose-env",
            "-p",
            "dev.peios.hello@1.0",
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
        "hello@1.0",
        path(content.path()),
        "--artifact-dir",
        path(store.path()),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).starts_with("ppk: hello@1.0: invalid package name 'hello': "),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read_dir(store.path()).unwrap().count(), 0);
}

#[test]
fn run_needs_packages() {
    for (args, missing) in [
        (vec!["run"], "<profile> or -p <package>..."),
        (vec!["run", "--", "true"], "<profile> or -p <package>..."),
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
    let output = ppk(&["pack", "dev.peios.hello@1.0"]);
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
        "dev.peios.hello@1.0",
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
        "dev.peios.hello@1.0",
        path(content.path()),
        "--artifact-dir",
        path(&artifacts),
    ]);
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));
    let missing = dir.path().join("missing");

    let output = ppk(&[
        "compose-env",
        "-p",
        "dev.peios.hello@1.0",
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
            "dev.peios.hello@1.0?arch=noarch",
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
        message.starts_with("ppk: dev.peios.hello@1.0?arch=noarch: already packed as sha256:"),
        "{message}"
    );
    assert_eq!(message.lines().count(), 1, "{message}");
}
