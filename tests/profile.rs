use ppk::{EnvDir, EnvSymlink, HOST_ARCH, PackageId, Profile};

fn id(s: &str) -> PackageId {
    s.parse().unwrap()
}

fn error(text: &str) -> String {
    format!("{:#}", Profile::parse(text).unwrap_err())
}

#[test]
fn a_profile_names_packages_symlinks_and_dirs() {
    let profile = Profile::parse(
        r#"
dirs = ["proc", "sys"]

[packages]
"net.busybox.busybox" = "1.37.0-1"
"dev.peios.ppk" = "0.1.0-1"

[symlinks]
"bin" = ":bin"
"init" = ":sbin/prelude"
"etc/motd" = "/run/motd"
"#,
    )
    .unwrap();
    let mut packages = profile.packages.clone();
    packages.sort_by(|a, b| a.name().cmp(b.name()));
    assert_eq!(
        packages,
        [
            PackageId::new("dev.peios.ppk", "0.1.0-1", HOST_ARCH).unwrap(),
            PackageId::new("net.busybox.busybox", "1.37.0-1", HOST_ARCH).unwrap(),
        ]
    );
    let links: Vec<(&str, &str)> = profile
        .symlinks
        .iter()
        .map(|link| (link.path(), link.target()))
        .collect();
    // A leading `:` stands for usr, as after `<path>:` in --symlink.
    assert!(links.contains(&("bin", "usr/bin")), "{links:?}");
    assert!(links.contains(&("init", "usr/sbin/prelude")), "{links:?}");
    // Any other target is written as given.
    assert!(links.contains(&("etc/motd", "/run/motd")), "{links:?}");
    let dirs: Vec<&str> = profile.dirs.iter().map(EnvDir::path).collect();
    assert_eq!(dirs, ["proc", "sys"]);
}

#[test]
fn a_rendered_profile_reads_back_as_itself() {
    let mut profile = Profile::parse(
        "arch = \"aarch64\"\ninclude_artifacts = true\ndirs = [\"proc\", \"a \\\"b\"]\n\n\
         [packages]\n\"a.b.c\" = \"1.0-1\"\n\n\
         [symlinks]\n\"init\" = \":sbin/prelude\"\n\"x/y\" = \":bin\"\n\"etc\" = \"/run/etc\"\n",
    )
    .unwrap();
    profile
        .add_packages([id("d.e.f@2-1?arch=aarch64")])
        .unwrap();
    let rendered = profile.render().unwrap();
    assert_eq!(Profile::parse(&rendered).unwrap(), profile, "{rendered}");
    assert_eq!(Profile::default().render().unwrap(), "");

    // A profile names one architecture.
    profile.add_packages([id("g.h.i@3-1?arch=x86_64")]).unwrap();
    assert_eq!(
        format!("{:#}", profile.render().unwrap_err()),
        "a.b.c@1.0-1?arch=aarch64 and g.h.i@3-1?arch=x86_64 are for different architectures"
    );
}

#[test]
fn every_key_is_optional_and_arch_applies_to_every_package() {
    assert_eq!(Profile::parse("").unwrap(), Profile::default());
    let profile = Profile::parse("arch = \"aarch64\"\n[packages]\n\"a.b.c\" = \"1-1\"\n").unwrap();
    assert_eq!(profile.packages, [id("a.b.c@1-1?arch=aarch64")]);
    assert!(!profile.include_artifacts);
    assert!(
        Profile::parse("include_artifacts = true")
            .unwrap()
            .include_artifacts
    );
}

#[test]
fn a_malformed_profile_is_refused() {
    for (text, message) in [
        ("pakages = {}", "unknown key \"pakages\""),
        ("packages = 1", "packages is not a table"),
        (
            "[packages]\n\"a.b.c\" = 1",
            "packages: \"a.b.c\" has a version that is not a string",
        ),
        (
            "[packages]\n\"a b\" = \"1\"",
            "packages: invalid package name 'a b'",
        ),
        ("dirs = \"proc\"", "dirs is not a list"),
        ("dirs = [1]", "dirs: 1 is not a string"),
        (
            "[symlinks]\n\"init\" = \"\"",
            "symlinks: \"init\" has no target",
        ),
        (
            "[symlinks]\n\"init\" = \":\"",
            "symlinks: \"init\" has no target",
        ),
        ("arch = 1", "arch is not a string"),
        (
            "include_artifacts = \"yes\"",
            "include_artifacts is not true or false",
        ),
        ("[packages", "not valid TOML"),
    ] {
        let found = error(text);
        assert!(found.starts_with(message), "{text}: {found}");
    }
}

#[test]
fn added_packages_and_extras_must_agree_with_the_profile() {
    let mut profile =
        Profile::parse("[packages]\n\"a.b.c\" = \"1-1\"\n[symlinks]\n\"bin\" = \":bin\"\n")
            .unwrap();
    let held = PackageId::new("a.b.c", "1-1", HOST_ARCH).unwrap();
    let other = PackageId::new("d.e.f", "2-1", HOST_ARCH).unwrap();
    // The same package again is added once; a new one is added.
    profile.add_packages([held.clone(), other.clone()]).unwrap();
    assert_eq!(profile.packages, [held.clone(), other]);
    let newer = PackageId::new("a.b.c", "2-1", HOST_ARCH).unwrap();
    assert_eq!(
        format!("{:#}", profile.add_packages([newer.clone()]).unwrap_err()),
        format!("{newer}: the environment holds {held} already")
    );

    let same: EnvSymlink = "bin::bin".parse().unwrap();
    let elsewhere: EnvSymlink = "bin:/elsewhere".parse().unwrap();
    let dir: EnvDir = "proc".parse().unwrap();
    profile
        .add_extras([same], [dir.clone(), dir.clone()])
        .unwrap();
    assert_eq!(profile.symlinks.len(), 1);
    assert_eq!(profile.dirs, [dir]);
    assert_eq!(
        format!("{:#}", profile.add_extras([elsewhere], []).unwrap_err()),
        "bin: links to /elsewhere, but the environment links it to usr/bin"
    );
}

/// Write each (name, text) into a new directory, and return it.
fn files(contents: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in contents {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

fn read_error(dir: &tempfile::TempDir, name: &str) -> String {
    format!("{:#}", Profile::read(&dir.path().join(name)).unwrap_err())
}

#[test]
fn a_profile_holds_what_its_fragments_hold_found_from_each_file() {
    let dir = files(&[
        (
            "env.toml",
            "include = [\"layout/root.toml\"]\ndirs = [\"tmp\"]\n\n\
             [packages]\n\"a.b.c\" = \"1.0-1\"\n",
        ),
        // Relative to the file that names it, not to the first.
        (
            "layout/root.toml",
            "include = [\"lib64.toml\"]\ndirs = [\"proc\", \"tmp\"]\n\n\
             [symlinks]\n\"bin\" = \":bin\"\n",
        ),
        ("layout/lib64.toml", "[symlinks]\n\"lib64\" = \":lib\"\n"),
    ]);
    let profile = Profile::read(&dir.path().join("env.toml")).unwrap();
    assert_eq!(profile.packages, [id("a.b.c@1.0-1")]);
    let links: Vec<(&str, &str)> = profile
        .symlinks
        .iter()
        .map(|link| (link.path(), link.target()))
        .collect();
    // Fragments come first, and something held twice alike is held once.
    assert_eq!(links, [("lib64", "usr/lib"), ("bin", "usr/bin")]);
    let dirs: Vec<&str> = profile.dirs.iter().map(EnvDir::path).collect();
    assert_eq!(dirs, ["proc", "tmp"]);
    // The answer is written whole, without the fragments it came from.
    assert!(!profile.render().unwrap().contains("include"));
}

#[test]
fn a_fragment_included_twice_by_different_routes_is_merged_once() {
    let dir = files(&[
        ("env.toml", "include = [\"a.toml\", \"b.toml\"]\n"),
        ("a.toml", "include = [\"base.toml\"]\n"),
        ("b.toml", "include = [\"base.toml\"]\n"),
        (
            "base.toml",
            "dirs = [\"proc\"]\n\n[packages]\n\"a.b.c\" = \"1.0-1\"\n",
        ),
    ]);
    let profile = Profile::read(&dir.path().join("env.toml")).unwrap();
    assert_eq!(profile.packages, [id("a.b.c@1.0-1")]);
    assert_eq!(profile.dirs.len(), 1);
}

#[test]
fn fragments_that_disagree_are_refused_naming_both() {
    let dir = files(&[
        ("versions.toml", "include = [\"one.toml\", \"two.toml\"]\n"),
        ("one.toml", "[packages]\n\"a.b.c\" = \"1.0-1\"\n"),
        ("two.toml", "[packages]\n\"a.b.c\" = \"2.0-1\"\n"),
        (
            "links.toml",
            "include = [\"bin.toml\"]\n\n[symlinks]\n\"bin\" = \"usr/sbin\"\n",
        ),
        ("bin.toml", "[symlinks]\n\"bin\" = \":bin\"\n"),
        ("kinds.toml", "include = [\"bin.toml\"]\ndirs = [\"bin\"]\n"),
        ("arches.toml", "include = [\"one.toml\", \"arm.toml\"]\n"),
        (
            "arm.toml",
            "arch = \"aarch64\"\n\n[packages]\n\"d.e.f\" = \"1.0-1\"\n",
        ),
    ]);
    let err = read_error(&dir, "versions.toml");
    assert!(err.contains("two.toml has a.b.c@2.0-1"), "{err}");
    assert!(err.contains("one.toml has a.b.c@1.0-1"), "{err}");
    let err = read_error(&dir, "links.toml");
    assert!(
        err.contains("links.toml links \"bin\" to \"usr/sbin\""),
        "{err}"
    );
    assert!(err.contains("bin.toml links it to \"usr/bin\""), "{err}");
    let err = read_error(&dir, "kinds.toml");
    assert!(
        err.contains("kinds.toml makes \"bin\" a directory"),
        "{err}"
    );
    assert!(err.contains("bin.toml links it"), "{err}");
    let err = read_error(&dir, "arches.toml");
    assert!(err.contains("for another architecture"), "{err}");
    assert!(
        err.contains("arm.toml") && err.contains("one.toml"),
        "{err}"
    );
}

#[test]
fn a_profile_that_includes_itself_is_refused() {
    let dir = files(&[
        ("a.toml", "include = [\"b.toml\"]\n"),
        ("b.toml", "include = [\"a.toml\"]\n"),
        ("self.toml", "include = [\"./self.toml\"]\n"),
    ]);
    let err = read_error(&dir, "a.toml");
    assert!(err.contains("include cycle"), "{err}");
    assert!(
        err.contains("a.toml includes ") && err.contains("b.toml includes "),
        "{err}"
    );
    assert!(read_error(&dir, "self.toml").contains("include cycle"));
}

#[test]
fn a_missing_or_malformed_include_is_refused() {
    let dir = files(&[
        ("missing.toml", "include = [\"nowhere.toml\"]\n"),
        ("string.toml", "include = \"a.toml\"\n"),
        ("empty.toml", "include = [\"\"]\n"),
    ]);
    let err = read_error(&dir, "missing.toml");
    assert!(err.contains("nowhere.toml"), "{err}");
    assert!(read_error(&dir, "string.toml").contains("include is not a list"));
    assert!(read_error(&dir, "empty.toml").contains("an empty path"));
}

#[test]
fn text_includes_only_when_given_a_directory() {
    let dir = files(&[("base.toml", "dirs = [\"proc\"]\n")]);
    let text = "include = [\"base.toml\"]\n";
    assert!(error(text).contains("no directory to find fragments in"));
    let profile = Profile::parse_in(text, dir.path()).unwrap();
    assert_eq!(profile.dirs.len(), 1);
}

#[test]
fn several_profiles_merge_as_fragments_do() {
    let dir = files(&[
        ("root.toml", "[packages]\n\"a.b.c\" = \"1.0-1\"\n"),
        (
            "layout.toml",
            "include_artifacts = true\ndirs = [\"proc\"]\n",
        ),
        ("other.toml", "[packages]\n\"a.b.c\" = \"1.1-1\"\n"),
    ]);
    let path = |name: &str| dir.path().join(name);
    let profile = Profile::read_all(&[path("root.toml"), path("layout.toml")]).unwrap();
    assert_eq!(profile.packages, [id("a.b.c@1.0-1")]);
    // Only true can be said, so true in any is true.
    assert!(profile.include_artifacts);
    let err = format!(
        "{:#}",
        Profile::read_all(&[path("root.toml"), path("other.toml")]).unwrap_err()
    );
    assert!(err.contains("other.toml has a.b.c@1.1-1"), "{err}");

    let mut merged = Profile::read(&path("root.toml")).unwrap();
    merged
        .merge(&Profile::read(&path("layout.toml")).unwrap())
        .unwrap();
    assert_eq!(merged, profile);
    assert!(
        merged
            .merge(&Profile::read(&path("other.toml")).unwrap())
            .is_err()
    );
}
