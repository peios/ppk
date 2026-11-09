use ppk::{Entry, Identity, Manifest, Metadata, PackageId, parse_manifest, render_manifest};

fn hello() -> PackageId {
    PackageId::new("dev.peios.hello", "1.0-1", "x86_64").unwrap()
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
        ("dir/empty".to_owned(), Entry::dir()),
        ("tool".to_owned(), Entry::file(hex(b"tool"), true)),
        (
            "we\"ird\nname".to_owned(),
            Entry::Symlink("../t ü".to_owned()),
        ),
    ];
    let mut sorted = entries.clone();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let text = render_manifest(&hello(), &Metadata::default(), &sorted);
    assert_eq!(
        parse_manifest(text.as_bytes()).unwrap(),
        Manifest {
            id: hello(),
            metadata: Metadata::default(),
            entries: sorted,
        }
    );
}

#[test]
fn any_other_spelling_is_refused() {
    let canonical = render_manifest(
        &hello(),
        &Metadata::default(),
        &[("a", Entry::file(hex(b"a"), true)), ("b", Entry::dir())],
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
    let unsorted = render_manifest(
        &hello(),
        &Metadata::default(),
        &[("b", Entry::dir()), ("a", Entry::dir())],
    );
    assert_eq!(problem(&unsorted), "not in canonical form");
}

#[test]
fn exec_false_is_not_canonical() {
    let text = format!(
        "{}\"a\" = {{ blob = \"sha256:{}\", exec = false }}\n",
        render_manifest::<&str>(&hello(), &Metadata::default(), &[]),
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
        let text = render_manifest(&hello(), &Metadata::default(), &[(path, Entry::dir())]);
        assert!(
            problem(&text).ends_with(": not a safe relative path"),
            "{path:?}: {}",
            problem(&text)
        );
    }
}

#[test]
fn blob_values_must_be_identities() {
    let base = render_manifest::<&str>(&hello(), &Metadata::default(), &[]);
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
    let text =
        render_manifest::<&str>(&hello(), &Metadata::default(), &[]).replace("1.0-1", "../1.0-1");
    assert!(problem(&text).starts_with("invalid version '../1.0-1'"));
    assert_eq!(problem("[files]\n"), "missing the string 'name'");
    assert!(problem("not = = toml").starts_with("not valid TOML: "));
    assert_eq!(
        format!("{:#}", parse_manifest(b"\xff").unwrap_err()),
        "not valid UTF-8: invalid utf-8 sequence of 1 bytes from index 0"
    );
}

/// eventd's override of `var/state/eventd` from the old repository.
const EVENTD: &str = "010004901400000020000000000000002c000000010100000000000512000000\
    01010000000000051200000002005c0003000000000314000000001001010000\
    0000000512000000000318000000001001020000000000052000000020020000\
    0003280000000010010600000000000550000000d2840e756d1f666dfc67a263\
    8a84d887a4a3e576";

fn described() -> Option<ppk::SecurityDescriptor> {
    Some(EVENTD.parse().unwrap())
}

#[test]
fn security_metadata_parses_back() {
    let entries = vec![
        (
            "a".to_owned(),
            Entry::Blob {
                hex: hex(b"a"),
                exec: true,
                descriptor: described(),
                signature: Some(hex(b"signature")),
            },
        ),
        (
            "b".to_owned(),
            Entry::Dir {
                descriptor: described(),
            },
        ),
    ];
    let text = render_manifest(&hello(), &Metadata::default(), &entries);
    assert!(text.ends_with(&format!(
        "\"a\" = {{ blob = \"sha256:{}\", exec = true, descriptor = \"{EVENTD}\", \
         signature = \"sha256:{}\" }}\n\"b\" = {{ dir = true, descriptor = \"{EVENTD}\" }}\n",
        hex(b"a"),
        hex(b"signature")
    )));
    assert_eq!(parse_manifest(text.as_bytes()).unwrap().entries, entries);
}

#[test]
fn security_metadata_has_one_spelling_and_belongs_where_it_can_apply() {
    let canonical = render_manifest(
        &hello(),
        &Metadata::default(),
        &[
            (
                "a",
                Entry::Blob {
                    hex: hex(b"a"),
                    exec: false,
                    descriptor: described(),
                    signature: Some(hex(b"signature")),
                },
            ),
            ("b", Entry::Symlink("a".to_owned())),
            ("c", Entry::dir()),
        ],
    );
    let descriptor = format!(", descriptor = \"{EVENTD}\"");
    let signature = format!(", signature = \"sha256:{}\"", hex(b"signature"));
    for variant in [
        canonical.replace(EVENTD, &EVENTD.to_uppercase()),
        canonical.replace(
            &format!("{descriptor}{signature}"),
            &format!("{signature}{descriptor}"),
        ),
        canonical.replace("symlink = \"a\"", &format!("symlink = \"a\"{descriptor}")),
        canonical.replace("dir = true", &format!("dir = true{signature}")),
    ] {
        assert_eq!(problem(&variant), "not in canonical form", "{variant}");
    }
    let malformed = canonical.replace(EVENTD, "0100");
    assert_eq!(
        problem(&malformed),
        "\"a\": descriptor: security descriptor is 2 bytes, shorter than its header"
    );
    let unhashed = canonical.replace(&format!("sha256:{}", hex(b"signature")), "sha256:ab");
    assert_eq!(
        problem(&unhashed),
        "\"a\": 'sha256:ab' is not a sha256 identity"
    );
}
