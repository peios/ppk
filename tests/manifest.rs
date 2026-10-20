use ppk::{Entry, Identity, Manifest, PackageId, parse_manifest, render_manifest};

fn hello() -> PackageId {
    PackageId::new("dev.peios.hello", "1.0", "x86_64").unwrap()
}

fn hex(content: &[u8]) -> String {
    Identity::of(content).hex().to_owned()
}

fn problem(manifest: &str) -> String {
    format!("{:#}", parse_manifest(manifest.as_bytes()).unwrap_err())
}

#[test]
fn a_rendered_manifest_parses_back() {
    let entries = vec![
        ("dir/empty".to_owned(), Entry::Dir),
        (
            "tool".to_owned(),
            Entry::Blob {
                hex: hex(b"tool"),
                exec: true,
            },
        ),
        (
            "we\"ird\nname".to_owned(),
            Entry::Symlink("../t ü".to_owned()),
        ),
    ];
    let mut sorted = entries.clone();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let text = render_manifest(&hello(), &sorted);
    assert_eq!(
        parse_manifest(text.as_bytes()).unwrap(),
        Manifest {
            id: hello(),
            entries: sorted,
        }
    );
}

#[test]
fn any_other_spelling_is_refused() {
    let canonical = render_manifest(
        &hello(),
        &[
            (
                "a",
                Entry::Blob {
                    hex: hex(b"a"),
                    exec: true,
                },
            ),
            ("b", Entry::Dir),
        ],
    );
    for variant in [
        canonical.replace(", exec = true", ", exec = true, mode = 1"),
        canonical.replace("dir = true", "dir = false"),
        canonical.replace("\n[files]", "extra = 1\n\n[files]"),
        canonical.replace("\"a\" = ", "a = "),
        canonical.replace(" }\n\"b\"", "}\n\"b\""),
        format!("{canonical}\n"),
    ] {
        assert_eq!(problem(&variant), "not in canonical form", "{variant}");
    }
}

#[test]
fn unsorted_entries_are_refused() {
    let unsorted = render_manifest(&hello(), &[("b", Entry::Dir), ("a", Entry::Dir)]);
    assert_eq!(problem(&unsorted), "not in canonical form");
}

#[test]
fn exec_false_is_not_canonical() {
    let text = format!(
        "{}\"a\" = {{ blob = \"sha256:{}\", exec = false }}\n",
        render_manifest::<&str>(&hello(), &[]),
        hex(b"a")
    );
    assert_eq!(problem(&text), "not in canonical form");
}

#[test]
fn unsafe_paths_are_refused() {
    for path in [
        "",
        "/etc/passwd",
        "..",
        "../x",
        "a/../b",
        "a/./b",
        "a//b",
        "a/",
        "nul\0",
    ] {
        let text = render_manifest(&hello(), &[(path, Entry::Dir)]);
        assert!(
            problem(&text).ends_with(": not a safe relative path"),
            "{path:?}: {}",
            problem(&text)
        );
    }
}

#[test]
fn blob_values_must_be_identities() {
    let base = render_manifest::<&str>(&hello(), &[]);
    for value in [
        "sha256:../../etc/passwd",
        "sha256:ABC",
        &format!("sha512:{}", hex(b"a")),
        &format!("sha256:{}", hex(b"a").to_uppercase()),
    ] {
        let text = format!("{base}\"a\" = {{ blob = \"{value}\" }}\n");
        assert_eq!(
            problem(&text),
            format!("\"a\": '{value}' is not a sha256 identity")
        );
    }
}

#[test]
fn the_header_must_name_a_valid_package() {
    let text = render_manifest::<&str>(&hello(), &[]).replace("1.0", "../1.0");
    assert!(problem(&text).starts_with("invalid version '../1.0'"));
    assert_eq!(problem("[files]\n"), "missing the string 'name'");
    assert!(problem("not = = toml").starts_with("not valid TOML: "));
    assert_eq!(
        format!("{:#}", parse_manifest(b"\xff").unwrap_err()),
        "not valid UTF-8: invalid utf-8 sequence of 1 bytes from index 0"
    );
}
