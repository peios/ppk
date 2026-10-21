use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// Build a directory tree from (relative path, content) pairs.
fn tree(files: &[(&str, &[u8])]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    dir
}

fn pack_as(id: &str, input: &Path, out: &Path) -> anyhow::Result<ppk::Packed> {
    ppk::pack(&id.parse().unwrap(), input, out)
}

/// Pack `input` as dev.peios.hello 1.0 for x86_64.
fn pack(input: &Path, out: &Path) -> anyhow::Result<ppk::Packed> {
    pack_as("dev.peios.hello@1.0?arch=x86_64", input, out)
}

fn parse(manifest: &Path) -> toml::Table {
    toml::from_str(&fs::read_to_string(manifest).unwrap()).unwrap()
}

fn parse_files(manifest: &Path) -> toml::Table {
    let mut doc = parse(manifest);
    let keys: Vec<&String> = doc.keys().collect();
    assert_eq!(keys, ["arch", "files", "name", "version"]);
    let files = match doc.remove("files").unwrap() {
        toml::Value::Table(files) => files,
        other => panic!("[files] is not a table: {other:?}"),
    };
    for (path, entry) in &files {
        let entry = entry
            .as_table()
            .unwrap_or_else(|| panic!("{path}: not a table"));
        let keys: Vec<&String> = entry.keys().collect();
        assert!(
            keys == ["blob"] || keys == ["blob", "exec"] || keys == ["symlink"] || keys == ["dir"],
            "{path}: entry has keys {keys:?}"
        );
        for flag in ["exec", "dir"] {
            if let Some(value) = entry.get(flag) {
                assert_eq!(
                    value.as_bool(),
                    Some(true),
                    "{path}: {flag} is only ever true"
                );
            }
        }
    }
    files
}

/// Where the form of the blob that `entry` needs is stored.
fn blob_path(out: &Path, entry: &toml::Value) -> PathBuf {
    let hex = entry["blob"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let suffix = if entry.get("exec").is_some() {
        ".exec"
    } else {
        ""
    };
    out.join("blobs/sha256").join(format!("{hex}{suffix}"))
}

/// Every file under `dir`, relative to it, sorted.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                found.push(path.strip_prefix(root).unwrap().to_owned());
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

#[test]
fn round_trips_every_file() {
    let files: &[(&str, &[u8])] = &[
        ("bin/hello", b"\x7fELF pretend binary"),
        ("share/man/man1/hello.1", b".TH HELLO 1\n"),
        ("share/licenses/hello/COPYING", b"GPL text"),
        ("empty", b""),
    ];
    let input = tree(files);
    let out = tempfile::tempdir().unwrap();

    let packed = pack(input.path(), out.path()).unwrap();
    assert_eq!(packed.entries, files.len());
    assert_eq!(packed.blobs_written, files.len());

    let manifest = parse_files(&packed.manifest_path);
    assert_eq!(manifest.len(), files.len());
    for (path, content) in files {
        let blob = blob_path(out.path(), &manifest[*path]);
        assert_eq!(&fs::read(&blob).unwrap(), content, "{path}");
        let name = blob.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, format!("{:x}", Sha256::digest(content)), "{path}");
    }
}

#[test]
fn artifacts_are_read_only() {
    let input = tree(&[("bin/hello", b"binary"), ("README", b"docs")]);
    set_mode(&input.path().join("bin/hello"), 0o755);
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();
    let manifest = parse_files(&packed.manifest_path);
    for (path, expected) in [
        (blob_path(out.path(), &manifest["README"]), 0o444),
        (blob_path(out.path(), &manifest["bin/hello"]), 0o555),
        (packed.manifest_path, 0o444),
        (packed.ref_path, 0o444),
    ] {
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, expected, "{}", path.display());
    }
}

#[test]
fn executables_are_stored_in_their_executable_form() {
    let input = tree(&[("bin/hello", b"binary")]);
    set_mode(&input.path().join("bin/hello"), 0o755);
    let out = tempfile::tempdir().unwrap();
    pack(input.path(), out.path()).unwrap();
    let hex = format!("{:x}", Sha256::digest(b"binary"));
    let blobs = out.path().join("blobs/sha256");
    assert!(blobs.join(format!("{hex}.exec")).exists());
    assert!(!blobs.join(&hex).exists(), "no plain form is needed");
}

#[test]
fn manifest_is_filed_by_its_hash() {
    let input = tree(&[("a", b"a")]);
    let out = tempfile::tempdir().unwrap();
    let packed = pack_as(
        "org.gnu.hello@2.12-1?arch=aarch64",
        input.path(),
        out.path(),
    )
    .unwrap();
    let hex = packed.identity.strip_prefix("sha256:").unwrap();
    assert_eq!(
        packed.manifest_path,
        out.path().join("manifests/sha256").join(hex)
    );
    assert_eq!(
        fs::read_to_string(&packed.manifest_path).unwrap(),
        "name = \"org.gnu.hello\"\n\
         version = \"2.12-1\"\n\
         arch = \"aarch64\"\n\
         \n\
         [files]\n\
         \"a\" = { blob = \"sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb\" }\n"
    );
}

#[test]
fn ref_names_the_manifest_by_name_version_and_arch() {
    let input = tree(&[("a", b"a")]);
    let out = tempfile::tempdir().unwrap();
    let packed = pack_as(
        "org.gnu.hello@2.12-1?arch=aarch64",
        input.path(),
        out.path(),
    )
    .unwrap();
    assert_eq!(
        packed.ref_path,
        out.path().join("refs/org.gnu.hello/2.12-1/aarch64")
    );
    assert_eq!(
        fs::read_to_string(&packed.ref_path).unwrap(),
        format!("{}\n", packed.identity)
    );
}

#[test]
fn every_kind_of_entry_has_one_spelling() {
    let input = tree(&[("file", b"a"), ("tool", b"a")]);
    set_mode(&input.path().join("tool"), 0o755);
    std::os::unix::fs::symlink("file", input.path().join("link")).unwrap();
    fs::create_dir(input.path().join("empty")).unwrap();
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();
    let text = fs::read_to_string(&packed.manifest_path).unwrap();
    let files: Vec<&str> = text.lines().skip_while(|l| *l != "[files]").collect();
    assert_eq!(
        files,
        [
            "[files]",
            "\"empty\" = { dir = true }",
            "\"file\" = { blob = \"sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb\" }",
            "\"link\" = { symlink = \"file\" }",
            "\"tool\" = { blob = \"sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb\", exec = true }",
        ]
    );
}

#[test]
fn identity_is_hash_of_manifest_bytes() {
    let input = tree(&[("a", b"a")]);
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();
    let bytes = fs::read(&packed.manifest_path).unwrap();
    assert_eq!(
        packed.identity,
        format!("sha256:{:x}", Sha256::digest(&bytes))
    );
}

#[test]
fn identical_trees_give_identical_manifests() {
    // Created in different orders so directory iteration order differs.
    let a = tree(&[("z", b"1"), ("a/b", b"2"), ("m", b"3")]);
    let b = tree(&[("m", b"3"), ("a/b", b"2"), ("z", b"1")]);
    let out_a = tempfile::tempdir().unwrap();
    let out_b = tempfile::tempdir().unwrap();

    let pa = pack(a.path(), out_a.path()).unwrap();
    let pb = pack(b.path(), out_b.path()).unwrap();
    assert_eq!(pa.identity, pb.identity);
    assert_eq!(
        fs::read(&pa.manifest_path).unwrap(),
        fs::read(&pb.manifest_path).unwrap()
    );
}

#[test]
fn name_version_and_arch_are_part_of_identity() {
    let input = tree(&[("a", b"a")]);
    let out = tempfile::tempdir().unwrap();
    let identities: Vec<String> = [
        "dev.peios.hello@1.0?arch=x86_64",
        "dev.peios.hello@2.0?arch=x86_64",
        "dev.peios.goodbye@1.0?arch=x86_64",
        "dev.peios.hello@1.0?arch=aarch64",
    ]
    .iter()
    .map(|id| pack_as(id, input.path(), out.path()).unwrap().identity)
    .collect();
    for (i, a) in identities.iter().enumerate() {
        for b in &identities[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn entries_are_sorted_by_bytes() {
    let input = tree(&[
        ("b", b""),
        ("a/z", b""),
        ("B", b""),
        ("a.txt", b""),
        ("é", b""),
    ]);
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();
    let text = fs::read_to_string(&packed.manifest_path).unwrap();
    let keys: Vec<&str> = text
        .lines()
        .skip_while(|line| *line != "[files]")
        .skip(1)
        .map(|line| line.split(" = ").next().unwrap())
        .collect();
    assert_eq!(keys, ["\"B\"", "\"a.txt\"", "\"a/z\"", "\"b\"", "\"é\""]);
}

#[test]
fn identical_content_shares_one_blob() {
    let input = tree(&[("one", b"same"), ("two", b"same"), ("three", b"different")]);
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();
    assert_eq!(packed.entries, 3);
    assert_eq!(packed.blobs_written, 2);
    assert_eq!(
        fs::read_dir(out.path().join("blobs/sha256"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn versions_coexist_and_share_unchanged_blobs() {
    let v1 = tree(&[("bin/hello", b"v1 binary"), ("COPYING", b"license")]);
    let v2 = tree(&[("bin/hello", b"v2 binary"), ("COPYING", b"license")]);
    let out = tempfile::tempdir().unwrap();

    let p1 = pack_as("dev.peios.hello@1.0?arch=x86_64", v1.path(), out.path()).unwrap();
    let p2 = pack_as("dev.peios.hello@2.0?arch=x86_64", v2.path(), out.path()).unwrap();
    assert_eq!(p1.blobs_written, 2);
    assert_eq!(p2.blobs_written, 1, "COPYING is already stored");
    assert!(p1.manifest_path.exists());
    assert!(p2.manifest_path.exists());
    assert_ne!(p1.ref_path, p2.ref_path);
}

#[test]
fn repacking_an_identical_version_succeeds() {
    let input = tree(&[("a", b"a")]);
    let out = tempfile::tempdir().unwrap();
    let first = pack(input.path(), out.path()).unwrap();
    let again = pack(input.path(), out.path()).unwrap();
    assert_eq!(again.blobs_written, 0);
    assert_eq!(again.identity, first.identity);
}

#[test]
fn a_packed_version_cannot_be_replaced() {
    let out = tempfile::tempdir().unwrap();
    let original = pack(tree(&[("a", b"original")]).path(), out.path()).unwrap();
    let before = files_under(out.path());
    let ref_before = fs::read(&original.ref_path).unwrap();

    let err = pack(tree(&[("a", b"changed")]).path(), out.path()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "dev.peios.hello@1.0?arch=x86_64: already packed as {}; \
             a packed version cannot be replaced",
            original.identity
        )
    );
    assert_eq!(fs::read(&original.ref_path).unwrap(), ref_before);
    assert_eq!(
        files_under(out.path()),
        before,
        "nothing written, and no temporary files left behind"
    );
}

#[test]
fn arches_of_one_version_coexist() {
    let x86 = tree(&[("bin/hello", b"x86 binary")]);
    let arm = tree(&[("bin/hello", b"arm binary")]);
    let out = tempfile::tempdir().unwrap();
    let a = pack_as("dev.peios.hello@1.0?arch=x86_64", x86.path(), out.path()).unwrap();
    let b = pack_as("dev.peios.hello@1.0?arch=aarch64", arm.path(), out.path()).unwrap();
    assert!(a.ref_path.exists());
    assert!(b.ref_path.exists());
    assert_ne!(a.ref_path, b.ref_path);
    assert_ne!(a.identity, b.identity);
}

#[test]
fn awkward_names_are_escaped_and_parse_back() {
    let names = [
        "quote\"d",
        "back\\slash",
        "tab\there",
        "new\nline",
        "del\x7f",
        "ünïcödé",
    ];
    let files: Vec<(&str, &[u8])> = names.iter().map(|n| (*n, n.as_bytes())).collect();
    let input = tree(&files);
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();

    let text = fs::read_to_string(&packed.manifest_path).unwrap();
    assert!(text.contains(r#""quote\"d""#));
    assert!(text.contains(r#""back\\slash""#));
    assert!(text.contains(r#""tab\u0009here""#));
    assert!(text.contains(r#""new\u000Aline""#));
    assert!(text.contains(r#""del\u007F""#));
    assert!(text.contains("\"ünïcödé\""));

    let manifest = parse_files(&packed.manifest_path);
    for name in names {
        let blob = blob_path(out.path(), &manifest[name]);
        assert_eq!(fs::read(blob).unwrap(), name.as_bytes());
    }
}

#[test]
fn empty_content_gives_empty_files_table() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();
    assert_eq!(
        fs::read_to_string(&packed.manifest_path).unwrap(),
        "name = \"dev.peios.hello\"\nversion = \"1.0\"\narch = \"x86_64\"\n\n[files]\n"
    );
    assert_eq!(packed.entries, 0);
}

#[test]
fn only_empty_directories_are_listed() {
    let input = tree(&[("usr/bin/hello", b"x")]);
    fs::create_dir_all(input.path().join("var/lib/hello")).unwrap();
    fs::create_dir_all(input.path().join("var/cache/hello/nested")).unwrap();
    fs::create_dir_all(input.path().join("etc/links")).unwrap();
    std::os::unix::fs::symlink("x", input.path().join("etc/links/only-a-link")).unwrap();
    let out = tempfile::tempdir().unwrap();

    let packed = pack(input.path(), out.path()).unwrap();
    let manifest = parse_files(&packed.manifest_path);
    let paths: Vec<&String> = manifest.keys().collect();
    assert_eq!(
        paths,
        [
            "etc/links/only-a-link",
            "usr/bin/hello",
            "var/cache/hello/nested",
            "var/lib/hello",
        ],
        "directories with contents are implied, not listed"
    );
    assert_eq!(manifest["var/lib/hello"]["dir"].as_bool(), Some(true));
    assert_eq!(
        manifest["var/cache/hello/nested"]["dir"].as_bool(),
        Some(true)
    );
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn any_execute_bit_marks_a_file_exec() {
    let modes = [
        ("rw", 0o644, false),
        ("rwx", 0o755, true),
        ("owner-only", 0o700, true),
        ("group-only", 0o610, true),
        ("other-only", 0o601, true),
        ("read-only", 0o444, false),
        ("sticky", 0o1644, false),
    ];
    let files: Vec<(&str, &[u8])> = modes.iter().map(|(n, _, _)| (*n, &b"x"[..])).collect();
    let input = tree(&files);
    for (name, mode, _) in modes {
        set_mode(&input.path().join(name), mode);
    }
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();
    assert_eq!(
        packed.blobs_written, 2,
        "one plain and one executable form of the same content"
    );
    let manifest = parse_files(&packed.manifest_path);
    for (name, mode, exec) in modes {
        let recorded = manifest[name].get("exec").is_some();
        assert_eq!(recorded, exec, "{name} ({mode:o})");
    }
}

#[test]
fn exec_bit_is_part_of_identity_and_selects_the_stored_form() {
    let input = tree(&[("bin/hello", b"binary")]);
    let out = tempfile::tempdir().unwrap();
    let plain = pack_as("dev.peios.hello@1.0?arch=x86_64", input.path(), out.path()).unwrap();
    set_mode(&input.path().join("bin/hello"), 0o755);
    let exec = pack_as("dev.peios.hello@1.1?arch=x86_64", input.path(), out.path()).unwrap();
    assert_eq!(exec.blobs_written, 1, "the executable form is stored too");
    assert_ne!(plain.identity, exec.identity);
    let again = pack_as("dev.peios.hello@1.2?arch=x86_64", input.path(), out.path()).unwrap();
    assert_eq!(again.blobs_written, 0, "each form is stored once");
}

#[test]
fn setuid_and_setgid_files_are_rejected() {
    for mode in [0o4755, 0o2755] {
        let input = tree(&[("bin/su", b"x")]);
        set_mode(&input.path().join("bin/su"), mode);
        let out = tempfile::tempdir().unwrap();
        let err = pack(input.path(), out.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("setuid and setgid bits are not supported"),
            "{mode:o}: {err:#}"
        );
    }
}

#[test]
fn symlink_targets_are_recorded_verbatim() {
    use std::os::unix::fs::symlink;
    let input = tree(&[("usr/lib/libz.so.1.3", b"lib")]);
    let lib = input.path().join("usr/lib");
    symlink("libz.so.1.3", lib.join("libz.so")).unwrap();
    symlink("/usr/share/zoneinfo/UTC", input.path().join("localtime")).unwrap();
    symlink("../lib/./libz.so.1.3", lib.join("unnormalised")).unwrap();
    symlink("nowhere/at/all", lib.join("dangling")).unwrap();
    let out = tempfile::tempdir().unwrap();

    let packed = pack(input.path(), out.path()).unwrap();
    assert_eq!(packed.entries, 5);
    assert_eq!(packed.blobs_written, 1, "symlinks have no blobs");
    let manifest = parse_files(&packed.manifest_path);
    for (path, target) in [
        ("usr/lib/libz.so", "libz.so.1.3"),
        ("localtime", "/usr/share/zoneinfo/UTC"),
        ("usr/lib/unnormalised", "../lib/./libz.so.1.3"),
        ("usr/lib/dangling", "nowhere/at/all"),
    ] {
        assert_eq!(manifest[path]["symlink"].as_str(), Some(target), "{path}");
    }
}

#[test]
fn symlinked_directories_are_not_followed() {
    let input = tree(&[("real/file", b"x")]);
    std::os::unix::fs::symlink("real", input.path().join("alias")).unwrap();
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();
    let manifest = parse_files(&packed.manifest_path);
    let paths: Vec<&String> = manifest.keys().collect();
    assert_eq!(paths, ["alias", "real/file"]);
    assert_eq!(manifest["alias"]["symlink"].as_str(), Some("real"));
}

#[test]
fn awkward_symlink_targets_are_escaped_and_parse_back() {
    let input = tempfile::tempdir().unwrap();
    let target = "we\"ird\\tar\nget ü";
    std::os::unix::fs::symlink(target, input.path().join("link")).unwrap();
    let out = tempfile::tempdir().unwrap();
    let packed = pack(input.path(), out.path()).unwrap();
    let text = fs::read_to_string(&packed.manifest_path).unwrap();
    assert!(
        text.contains(r#"{ symlink = "we\"ird\\tar\u000Aget ü" }"#),
        "{text}"
    );
    let manifest = parse_files(&packed.manifest_path);
    assert_eq!(manifest["link"]["symlink"].as_str(), Some(target));
}

#[test]
fn non_utf8_symlink_targets_are_rejected() {
    use std::os::unix::ffi::OsStrExt;
    let input = tempfile::tempdir().unwrap();
    let target = std::ffi::OsStr::from_bytes(b"bad\xff");
    std::os::unix::fs::symlink(target, input.path().join("link")).unwrap();
    let out = tempfile::tempdir().unwrap();
    let err = pack(input.path(), out.path()).unwrap_err();
    assert!(
        err.to_string()
            .contains("symlink target is not valid UTF-8"),
        "{err:#}"
    );
}

#[test]
fn special_files_are_rejected() {
    let input = tempfile::tempdir().unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(input.path().join("sock")).unwrap();
    let out = tempfile::tempdir().unwrap();
    let err = pack(input.path(), out.path()).unwrap_err();
    assert!(err.to_string().contains("not a regular file"), "{err:#}");
}

#[test]
fn non_utf8_paths_are_rejected() {
    use std::os::unix::ffi::OsStrExt;
    let input = tempfile::tempdir().unwrap();
    let name = std::ffi::OsStr::from_bytes(b"bad\xff");
    fs::write(input.path().join(name), b"x").unwrap();
    let out = tempfile::tempdir().unwrap();
    let err = pack(input.path(), out.path()).unwrap_err();
    assert!(err.to_string().contains("not valid UTF-8"), "{err:#}");
}

#[test]
fn artifact_dir_inside_input_is_rejected() {
    let input = tree(&[("a", b"a")]);
    let out = input.path().join("not/yet/created");
    let err = pack(input.path(), &out).unwrap_err();
    assert!(err.to_string().contains("inside the input"), "{err:#}");
    assert!(!input.path().join("not").exists());
}

#[test]
fn input_must_be_a_directory() {
    let input = tree(&[("file", b"x")]);
    let out = tempfile::tempdir().unwrap();
    let err = pack(&input.path().join("file"), out.path()).unwrap_err();
    assert!(err.to_string().contains("not a directory"), "{err:#}");
}

#[test]
fn packed_names_the_identity_a_version_is_packed_as() {
    let input = tree(&[("a", b"a")]);
    let out = tempfile::tempdir().unwrap();
    let id: ppk::PackageId = "dev.peios.hello@1.0?arch=x86_64".parse().unwrap();
    assert_eq!(ppk::packed(&id, out.path()).unwrap(), None);
    let packed = pack(input.path(), out.path()).unwrap();
    let found = ppk::packed(&id, out.path()).unwrap().unwrap();
    assert_eq!(found.to_string(), packed.identity);
    let other: ppk::PackageId = "dev.peios.hello@1.0?arch=aarch64".parse().unwrap();
    assert_eq!(ppk::packed(&other, out.path()).unwrap(), None);
}

#[test]
fn versions_lists_what_is_packed_for_an_arch() {
    let input = tree(&[("a", b"a")]);
    let out = tempfile::tempdir().unwrap();
    assert!(
        ppk::versions("dev.peios.hello", "x86_64", out.path())
            .unwrap()
            .is_empty()
    );
    for id in [
        "dev.peios.hello@2.0?arch=x86_64",
        "dev.peios.hello@1.0?arch=x86_64",
        "dev.peios.hello@3.0?arch=aarch64",
    ] {
        pack_as(id, input.path(), out.path()).unwrap();
    }
    assert_eq!(
        ppk::versions("dev.peios.hello", "x86_64", out.path()).unwrap(),
        ["1.0", "2.0"]
    );
    assert_eq!(
        ppk::versions("dev.peios.hello", "aarch64", out.path()).unwrap(),
        ["3.0"]
    );
}

#[test]
fn preparing_creates_a_missing_artifact_dir_and_leaves_it_empty() {
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("nested/artifacts");
    ppk::prepare_artifact_dir(&out).unwrap();
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    // Already there, it is still fine.
    ppk::prepare_artifact_dir(&out).unwrap();
}

#[test]
fn preparing_an_artifact_dir_that_cannot_be_written_fails() {
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("artifacts");
    fs::create_dir(&out).unwrap();
    set_mode(&out, 0o555);
    assert!(ppk::prepare_artifact_dir(&out).is_err());
    // Nor can one be made where its parent cannot be written.
    assert!(ppk::prepare_artifact_dir(&out.join("inner")).is_err());
    set_mode(&out, 0o755);
}
