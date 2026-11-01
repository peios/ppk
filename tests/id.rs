use ppk::{HOST_ARCH, PackageId};

fn parse(s: &str) -> anyhow::Result<PackageId> {
    s.parse()
}

#[test]
fn arch_defaults_to_the_host() {
    let id = parse("dev.peios.hello@1.0").unwrap();
    assert_eq!(id.name(), "dev.peios.hello");
    assert_eq!(id.version(), "1.0");
    assert_eq!(id.arch(), HOST_ARCH);
}

#[test]
fn arch_qualifier_sets_the_arch() {
    for arch in ["aarch64", "x86_64", "noarch"] {
        let id = parse(&format!("dev.peios.hello@1.0?arch={arch}")).unwrap();
        assert_eq!(id.arch(), arch);
    }
}

#[test]
fn display_always_spells_out_the_arch() {
    let id = parse("dev.peios.hello@2.12-1").unwrap();
    let canonical = format!("dev.peios.hello@2.12-1?arch={HOST_ARCH}");
    assert_eq!(id.to_string(), canonical);
    assert_eq!(parse(&canonical).unwrap(), id);
}

#[test]
fn malformed_identifiers_are_rejected() {
    for (input, message) in [
        ("dev.peios.hello", "dev.peios.hello: missing '@<version>'"),
        (
            "dev.peios.hello?arch=x86_64",
            "dev.peios.hello?arch=x86_64: missing '@<version>'",
        ),
        (
            "dev.peios.hello@1.0?os=peios",
            "dev.peios.hello@1.0?os=peios: unknown qualifier 'os'",
        ),
        (
            "dev.peios.hello@1.0?arch=x86_64&arch=aarch64",
            "dev.peios.hello@1.0?arch=x86_64&arch=aarch64: arch is given more than once",
        ),
        (
            "dev.peios.hello@1.0?arch",
            "dev.peios.hello@1.0?arch: qualifier 'arch' is not of the form <key>=<value>",
        ),
        (
            "dev.peios.hello@1.0?",
            "dev.peios.hello@1.0?: qualifier '' is not of the form <key>=<value>",
        ),
    ] {
        let err = parse(input).unwrap_err();
        assert_eq!(err.to_string(), message, "{input}");
    }
}

#[test]
fn reverse_dns_names_are_accepted() {
    for name in [
        "dev.peios.hello",
        "dev.peios.testing.fixture",
        "com.amd.amd-ucode",
        "io.github.eudev-project.eudev",
        "org.gnu.gcc-c++",
        "org.gnu.gcc-libstdc++-devel",
        // A version in the name makes a label of digits.
        "org.rust-lang.rust-1.83",
        "org.golang.go-stage0-1.24",
        "org.7-zip.7zip",
        // The longest there can be.
        &format!("dev.peios.{}", "a".repeat(54)),
    ] {
        let id = parse(&format!("{name}@1.0")).unwrap();
        assert_eq!(id.name(), name);
    }
}

#[test]
fn names_that_are_not_reverse_dns_are_rejected() {
    for name in [
        // Too few labels: a domain's two and a product.
        "hello",
        "peios.hello",
        // Outside PSPU 5.3's characters, though safe as a path.
        "Dev.peios.hello",
        "dev.peios.Hello",
        "dev.peios.hello_world",
        "dev.peios.hello~1",
        // Empty labels.
        "dev..peios.hello",
        "dev.peios.hello.",
        ".dev.peios.hello",
        // Separators together or at a label's edge.
        "dev.peios.hello--world",
        "dev.peios.-hello",
        "dev.peios.hello-",
        "dev.peios-.hello",
        // A plus cannot start a label or be part of the domain.
        "dev.peios.+hello",
        "dev.pe+ios.hello",
        "de+v.peios.hello",
        // No top-level domain has a digit.
        "1dev.peios.hello",
        "de1v.peios.hello",
        // One too long.
        &format!("dev.peios.{}", "a".repeat(55)),
    ] {
        let input = format!("{name}@1.0");
        let message = parse(&input).unwrap_err().to_string();
        assert!(
            message.starts_with(&format!("{input}: invalid package name '{name}': ")),
            "{input:?}: {message}"
        );
    }
}

#[test]
fn the_name_rule_is_for_names_alone() {
    let id = parse("dev.peios.hello@1.0_rc~1+git.A?arch=x86_64").unwrap();
    assert_eq!(id.version(), "1.0_rc~1+git.A");
}

#[test]
fn unsafe_names_versions_and_arches_are_rejected() {
    for bad in [
        "",
        ".",
        "..",
        "../escape",
        "a/b",
        ".hidden",
        "-flag",
        "sp ace",
        "Ünï",
        "a@b",
        "a&b",
        "a=b",
    ] {
        for (input, what) in [
            (format!("{bad}@1.0?arch=x86_64"), "package name"),
            (format!("dev.peios.hello@{bad}?arch=x86_64"), "version"),
            (format!("dev.peios.hello@1.0?arch={bad}"), "arch"),
        ] {
            let message = parse(&input).unwrap_err().to_string();
            assert!(
                message.starts_with(&format!("{input}: ")),
                "{input:?}: {message}"
            );
            // A separator character moves where the identifier splits, so
            // the error can be about a different part; it is still refused.
            if !bad.contains(['@', '&', '=']) {
                assert!(
                    message.contains(&format!("invalid {what}")),
                    "{input:?}: {message}"
                );
            }
        }
    }
}
