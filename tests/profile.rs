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
fn every_key_is_optional_and_arch_applies_to_every_package() {
    assert_eq!(Profile::parse("").unwrap(), Profile::default());
    let profile = Profile::parse("arch = \"aarch64\"\n[packages]\n\"a.b.c\" = \"1\"\n").unwrap();
    assert_eq!(profile.packages, [id("a.b.c@1?arch=aarch64")]);
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
        ("[packages", "not valid TOML"),
    ] {
        let found = error(text);
        assert!(found.starts_with(message), "{text}: {found}");
    }
}

#[test]
fn added_packages_and_extras_must_agree_with_the_profile() {
    let mut profile =
        Profile::parse("[packages]\n\"a.b.c\" = \"1\"\n[symlinks]\n\"bin\" = \":bin\"\n").unwrap();
    let held = PackageId::new("a.b.c", "1", HOST_ARCH).unwrap();
    let other = PackageId::new("d.e.f", "2", HOST_ARCH).unwrap();
    // The same package again is added once; a new one is added.
    profile.add_packages([held.clone(), other.clone()]).unwrap();
    assert_eq!(profile.packages, [held.clone(), other]);
    let newer = PackageId::new("a.b.c", "2", HOST_ARCH).unwrap();
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
