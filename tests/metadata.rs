use std::collections::BTreeMap;
use std::fs;

use ppk::{
    Constraint, Entry, LicenseClass, Metadata, PackageId, Version, parse_manifest, render_manifest,
};

fn hello() -> PackageId {
    PackageId::new("dev.peios.hello", "1.0-1", "x86_64").unwrap()
}

fn constraint(s: &str) -> Constraint {
    Constraint::from_manifest_string(s).unwrap()
}

/// Everything a manifest can say, once each.
fn full() -> Metadata {
    Metadata {
        description: Some("A friendly greeting".to_owned()),
        homepage: Some("https://peios.dev/hello".to_owned()),
        license: Some("MIT".to_owned()),
        license_class: LicenseClass::Free,
        source_license: Some("MIT AND BSD-3-Clause".to_owned()),
        dependencies: BTreeMap::from([
            ("org.gnu.glibc".to_owned(), constraint(">=2.44")),
            ("dev.peios.libgreet".to_owned(), constraint("= 1.0-1")),
        ]),
        requires: BTreeMap::from([
            ("sh".to_owned(), Constraint::default()),
            ("libc.so.6".to_owned(), constraint(">= 2.34")),
        ]),
        provides: BTreeMap::from([
            ("hello".to_owned(), None),
            (
                "pkgconfig(hello)".to_owned(),
                Some(Version::parse_relaxed("1.0").unwrap()),
            ),
        ]),
        claims: BTreeMap::from([("/usr/bin/hi".to_owned(), "hello".to_owned())]),
    }
}

const FULL: &str = "\
name = \"dev.peios.hello\"
version = \"1.0-1\"
arch = \"x86_64\"
description = \"A friendly greeting\"
homepage = \"https://peios.dev/hello\"
license = \"MIT\"
license_class = \"free\"
source_license = \"MIT AND BSD-3-Clause\"

[claims]
\"/usr/bin/hi\" = \"hello\"

[dependencies]
\"dev.peios.libgreet\" = \"= 1.0-1\"
\"org.gnu.glibc\" = \">= 2.44\"

[provides]
\"hello\" = \"\"
\"pkgconfig(hello)\" = \"1.0\"

[requires]
\"libc.so.6\" = \">= 2.34\"
\"sh\" = \"\"

[files]
\"bin/hello\" = { dir = true }
";

/// The error parsing `text` as a manifest gives, with its causes.
fn problem(text: &str) -> String {
    format!("{:#}", parse_manifest(text.as_bytes()).unwrap_err())
}

#[test]
fn no_metadata_leaves_the_manifest_as_it_was() {
    let metadata = Metadata::default();
    assert!(metadata.is_empty());
    assert_eq!(
        render_manifest(&hello(), &metadata, &[("bin/hello", Entry::Dir)]),
        "name = \"dev.peios.hello\"\nversion = \"1.0-1\"\narch = \"x86_64\"\n\n\
         [files]\n\"bin/hello\" = { dir = true }\n"
    );
    // Saying the class is unknown is saying nothing.
    let unknown = Metadata {
        license_class: LicenseClass::Unknown,
        ..Metadata::default()
    };
    assert!(unknown.is_empty());
}

#[test]
fn metadata_has_one_canonical_form_that_reads_back() {
    let text = render_manifest(&hello(), &full(), &[("bin/hello", Entry::Dir)]);
    assert_eq!(text, FULL);
    let manifest = parse_manifest(text.as_bytes()).unwrap();
    assert_eq!(manifest.metadata, full());
    assert!(!manifest.metadata.is_empty());
    let glibc = &manifest.metadata.dependencies["org.gnu.glibc"];
    assert!(glibc.matches(&Version::parse("2.44-3").unwrap()));
    assert!(!glibc.matches(&Version::parse("2.43-1").unwrap()));
    assert!(manifest.metadata.requires["sh"].is_any());
    assert_eq!(manifest.metadata.provides["hello"], None);
}

#[test]
fn any_other_spelling_of_metadata_is_refused() {
    for (from, to) in [
        // Constraints in another form than the canonical one.
        ("\">= 2.44\"", "\">=2.44\""),
        ("\"= 1.0-1\"", "\"1.0-1\""),
        // An explicit unknown class, which is the same as none.
        ("license_class = \"free\"", "license_class = \"unknown\""),
        // Fields or tables out of order.
        (
            "homepage = \"https://peios.dev/hello\"\nlicense = \"MIT\"\n",
            "license = \"MIT\"\nhomepage = \"https://peios.dev/hello\"\n",
        ),
        (
            "\n[claims]\n\"/usr/bin/hi\" = \"hello\"\n",
            "\n[zzz]\n\"/usr/bin/hi\" = \"hello\"\n",
        ),
        // An empty table.
        ("\"/usr/bin/hi\" = \"hello\"\n", ""),
        // A literal string, or a bare key.
        ("\"sh\" = \"\"", "sh = \"\""),
        (
            "source_license = \"MIT AND BSD-3-Clause\"",
            "source_license = 'MIT AND BSD-3-Clause'",
        ),
    ] {
        assert!(FULL.contains(from), "{from}");
        let text = FULL.replacen(from, to, 1);
        assert!(
            parse_manifest(text.as_bytes()).is_err(),
            "{from:?} as {to:?} was accepted"
        );
    }
}

#[test]
fn metadata_that_breaks_a_rule_is_refused_when_read() {
    for (from, to, message) in [
        (
            "\"org.gnu.glibc\" = ",
            "\"glibc\" = ",
            "dependencies: invalid package name 'glibc': ",
        ),
        (
            "\"libc.so.6\" = ",
            "\"-libc\" = ",
            "requires: invalid capability '-libc': ",
        ),
        (
            "\"hello\" = \"\"",
            "\"hello\" = \"1.0-0\"",
            "provides: \"hello\": revision in '1.0-0' must be a positive integer",
        ),
        (
            "\">= 2.34\"",
            "\">= 2.34,\"",
            "requires: \"libc.so.6\": constraint '>= 2.34,' has an empty expression",
        ),
        (
            "\"/usr/bin/hi\" = ",
            "\"usr/bin/hi\" = ",
            "claims: \"usr/bin/hi\": not a normal absolute path",
        ),
        (
            "license_class = \"free\"",
            "license_class = \"gratis\"",
            "invalid license class 'gratis': ",
        ),
        (
            "description = \"A friendly greeting\"",
            "description = \"A friendly\\tgreeting\"",
            "description: byte 0x09 at offset 10 is not printable ASCII",
        ),
        (
            "license = \"MIT\"",
            "license = 1",
            "'license' is not a string",
        ),
        (
            "\"/usr/bin/hi\" = \"hello\"",
            "\"/usr/bin/hi\" = true",
            "claims: \"/usr/bin/hi\" is not a string",
        ),
    ] {
        assert!(FULL.contains(from), "{from}");
        let got = problem(&FULL.replacen(from, to, 1));
        assert!(got.starts_with(message), "{to}: {got}");
    }
}

#[test]
fn names_follow_their_own_rules() {
    let check = |metadata: Metadata| metadata.check().map_err(|err| format!("{err:#}"));
    for name in [
        "sh",
        "libc.so.6",
        "libGL.so.1",
        "libstdc++.so.6",
        "libgcc_s.so.1",
        "g++",
        "pkgconfig(gtk+-3.0)",
        "perl(Foo::Bar)",
        "python3dist(ruamel.yaml)",
        "dev.peios.hello",
    ] {
        let metadata = Metadata {
            requires: BTreeMap::from([(name.to_owned(), Constraint::default())]),
            provides: BTreeMap::from([(name.to_owned(), None)]),
            ..Metadata::default()
        };
        assert_eq!(check(metadata), Ok(()), "{name}");
    }
    for name in [
        "",
        "a",
        "bad name",
        "-leading",
        "trailing-",
        "trailing.",
        "_leading",
        "Pkgconfig(x)",
        "pkgconfig()",
        "pkgconfig(x",
        "pkgconfig(x y)",
        "(x)",
        &"a".repeat(129),
    ] {
        let metadata = Metadata {
            provides: BTreeMap::from([(name.to_owned(), None)]),
            ..Metadata::default()
        };
        assert!(check(metadata).is_err(), "{name:?}");
    }
    // A dependency is on a package, so capabilities are not names for it.
    for name in ["sh", "libGL.so.1", "pkgconfig(zlib)"] {
        let metadata = Metadata {
            dependencies: BTreeMap::from([(name.to_owned(), Constraint::default())]),
            ..Metadata::default()
        };
        assert!(check(metadata).is_err(), "{name}");
    }
}

#[test]
fn claims_are_normal_absolute_paths_with_a_target() {
    let check = |path: &str, target: &str| {
        Metadata {
            claims: BTreeMap::from([(path.to_owned(), target.to_owned())]),
            ..Metadata::default()
        }
        .check()
        .is_ok()
    };
    for (path, target) in [
        ("/usr/bin/sh", "dash"),
        ("/init", "/usr/sbin/peinit"),
        ("/sbin/registryd", "../usr/sbin/loregd"),
        ("/usr/bin/a b", "a b"),
    ] {
        assert!(check(path, target), "{path} -> {target}");
    }
    for (path, target) in [
        ("usr/bin/sh", "dash"),
        ("/", "dash"),
        ("/usr/bin/", "dash"),
        ("//usr/bin/sh", "dash"),
        ("/usr/./bin/sh", "dash"),
        ("/usr/../bin/sh", "dash"),
        ("/usr/bin/sh\0", "dash"),
        ("/usr/bin/sh", ""),
        ("/usr/bin/sh", "da\0sh"),
    ] {
        assert!(!check(path, target), "{path:?} -> {target:?}");
    }
}

#[test]
fn descriptive_fields_are_checked() {
    let check = |metadata: Metadata| metadata.check().is_ok();
    let with = |f: fn(&mut Metadata)| {
        let mut metadata = Metadata::default();
        f(&mut metadata);
        metadata
    };
    assert!(check(with(
        |m| m.homepage = Some("http://example.org".to_owned())
    )));
    assert!(!check(with(|m| m.homepage = Some(String::new()))));
    for homepage in [
        "ftp://example.org",
        "https://",
        "https:///x",
        "https://a b",
        "example.org",
    ] {
        let metadata = Metadata {
            homepage: Some(homepage.to_owned()),
            ..Metadata::default()
        };
        assert!(!check(metadata), "{homepage}");
    }
    assert!(!check(with(
        |m| m.description = Some("caf\u{e9}".to_owned())
    )));
    assert!(!check(with(|m| m.license = Some(String::new()))));
    assert!(!check(with(|m| m.license = Some("MIT\n".to_owned()))));
    assert!(!check(with(|m| m.source_license = Some(String::new()))));
    for class in ["unknown", "free", "firmware", "proprietary"] {
        assert_eq!(class.parse::<LicenseClass>().unwrap().to_string(), class);
    }
}

#[test]
fn metadata_is_part_of_identity_and_checked_when_packing() {
    let input = tempfile::tempdir().unwrap();
    fs::write(input.path().join("hello"), "hi").unwrap();
    let out = tempfile::tempdir().unwrap();

    let bare = ppk::pack(&hello(), input.path(), out.path()).unwrap();
    let same = ppk::pack_with(&hello(), &Metadata::default(), input.path(), out.path()).unwrap();
    assert_eq!(same.identity, bare.identity);

    // full() writes a floor on libc.so.6, which packing refuses because
    // derivation sets it, so require it for any version here instead.
    let mut full = full();
    full.requires
        .insert("libc.so.6".to_owned(), Constraint::default());
    let refused = ppk::pack_with(&hello(), &full, input.path(), out.path()).unwrap_err();
    assert!(
        refused.to_string().contains("already packed as"),
        "{refused}"
    );
    let replaced = ppk::pack_replacing_with(&hello(), &full, input.path(), out.path()).unwrap();
    assert_ne!(replaced.identity, bare.identity);
    let manifest = parse_manifest(&fs::read(&replaced.manifest_path).unwrap()).unwrap();
    assert_eq!(manifest.metadata, full);

    let empty = tempfile::tempdir().unwrap();
    let bad = Metadata {
        description: Some(String::new()),
        ..Metadata::default()
    };
    assert!(ppk::pack_with(&hello(), &bad, input.path(), empty.path()).is_err());
    assert_eq!(fs::read_dir(empty.path()).unwrap().count(), 0);
}
