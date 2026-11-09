//! Security metadata from outside: how `pack` takes it and records it, how
//! the store keeps each file with metadata on its own, and what ppk refuses.
//!
//! These run ppk as it is built, which writes `security.` attributes. A host
//! that isn't Peios lets only a privileged process write those, so each test
//! accepts either outcome a host may give: the attribute is there, or ppk
//! counted the file as unstamped. The library's unit tests stamp for real.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ppk::{Identity, PackOptions, PackageId, SecurityDescriptor};
use sha2::{Digest, Sha256};

/// gxwi's override of `usr/bin/gxwi-server` from the old repository.
const GXWI_SERVER: &str = "010004801400000020000000000000002c000000010100000000000512000000\
    01010000000000051200000002007c0004000000000014000000001001010000\
    0000000512000000000018000000001001020000000000052000000020020000\
    00001400000000a001010000000000010000000000003400000000a001090000\
    0000000f03000000f00fd709e0ee21c068cb5e4af60ebf884e359bda5e43af72\
    481c2cfb367febf7";

/// eventd's override of `var/state/eventd` from the old repository.
const EVENTD: &str = "010004901400000020000000000000002c000000010100000000000512000000\
    01010000000000051200000002005c0003000000000314000000001001010000\
    0000000512000000000318000000001001020000000000052000000020020000\
    0003280000000010010600000000000550000000d2840e756d1f666dfc67a263\
    8a84d887a4a3e576";

const SIG_XATTR: &str = "security.peios.sig";

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A signature's shape, filled with `fill`.
fn signature(fill: u8) -> Vec<u8> {
    let mut signature = vec![fill; 3310];
    signature[0] = 1;
    signature
}

fn id() -> PackageId {
    "dev.peios.firmware@1.0-1?arch=x86_64".parse().unwrap()
}

fn write(root: &Path, path: &str, content: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn descriptors(pairs: &[(&str, &str)]) -> BTreeMap<String, SecurityDescriptor> {
    pairs
        .iter()
        .map(|(path, sd)| ((*path).to_owned(), sd.parse().unwrap()))
        .collect()
}

/// A signed firmware file, an executable with a descriptor, both with the
/// same content as a plain file, and a directory with a descriptor.
fn firmware() -> tempfile::TempDir {
    let input = tempfile::tempdir().unwrap();
    write(input.path(), "lib/firmware/a.bin", b"fw");
    write(input.path(), "lib/firmware/a.bin.peios.sig", &signature(7));
    write(input.path(), "bin/server", b"fw");
    write(input.path(), "share/plain", b"fw");
    write(input.path(), "libexec/tool", b"tool");
    input
}

fn firmware_options() -> PackOptions {
    PackOptions {
        descriptors: descriptors(&[("bin/server", GXWI_SERVER), ("libexec", EVENTD)]),
        ..PackOptions::default()
    }
}

/// Whether `path` has the attribute `name`.
fn has_xattr(path: &Path, name: &str) -> bool {
    let path_c = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let name_c = CString::new(name).unwrap();
    // SAFETY: NUL-terminated strings, and no buffer: this asks for the size.
    let len = unsafe { libc::lgetxattr(path_c.as_ptr(), name_c.as_ptr(), std::ptr::null_mut(), 0) };
    len >= 0
}

/// The stored file of a file entry with security metadata, `entry` as the
/// manifest writes it.
fn form_path(artifacts: &Path, content: &[u8], entry: &str) -> PathBuf {
    artifacts
        .join("blobs/sha256")
        .join(format!("{}.{}", hex(content), hex(entry.as_bytes())))
}

#[test]
fn metadata_is_written_in_its_entry_in_one_spelling() {
    let input = firmware();
    let out = tempfile::tempdir().unwrap();
    let packed =
        ppk::pack_with_options(&id(), &firmware_options(), input.path(), out.path()).unwrap();
    let text = fs::read_to_string(&packed.manifest_path).unwrap();
    let files: Vec<&str> = text.lines().skip_while(|l| *l != "[files]").collect();
    let (fw, tool, sig) = (hex(b"fw"), hex(b"tool"), hex(&signature(7)));
    assert_eq!(
        files,
        [
            "[files]".to_owned(),
            format!(
                "\"bin/server\" = {{ blob = \"sha256:{fw}\", descriptor = \"{GXWI_SERVER}\" }}"
            ),
            format!(
                "\"lib/firmware/a.bin\" = {{ blob = \"sha256:{fw}\", signature = \"sha256:{sig}\" }}"
            ),
            format!("\"libexec\" = {{ dir = true, descriptor = \"{EVENTD}\" }}"),
            format!("\"libexec/tool\" = {{ blob = \"sha256:{tool}\" }}"),
            format!("\"share/plain\" = {{ blob = \"sha256:{fw}\" }}"),
        ]
    );
    assert!(ppk::parse_manifest(text.as_bytes()).is_ok());
}

#[test]
fn metadata_is_part_of_the_identity() {
    let out = tempfile::tempdir().unwrap();
    let with = firmware();
    let without = firmware();
    fs::remove_file(without.path().join("lib/firmware/a.bin.peios.sig")).unwrap();
    let as_version = |version: &str| PackageId::new("dev.peios.firmware", version, "x86_64");
    let a = ppk::pack(&as_version("1-1").unwrap(), without.path(), out.path()).unwrap();
    let b = ppk::pack(&as_version("2-1").unwrap(), with.path(), out.path()).unwrap();
    let c = ppk::pack_with_options(
        &as_version("3-1").unwrap(),
        &firmware_options(),
        without.path(),
        out.path(),
    )
    .unwrap();
    let manifest = |packed: &ppk::Packed| fs::read_to_string(&packed.manifest_path).unwrap();
    let files = |packed: &ppk::Packed| {
        let text = manifest(packed);
        text[text.find("[files]").unwrap()..].to_owned()
    };
    assert_ne!(files(&a), files(&b));
    assert_ne!(files(&a), files(&c));
    assert_ne!(files(&b), files(&c));
}

#[test]
fn each_file_with_metadata_is_a_stored_file_of_its_own() {
    let input = firmware();
    let out = tempfile::tempdir().unwrap();
    let packed =
        ppk::pack_with_options(&id(), &firmware_options(), input.path(), out.path()).unwrap();
    let (fw, sig) = (hex(b"fw"), hex(&signature(7)));
    let signed = form_path(
        out.path(),
        b"fw",
        &format!("{{ blob = \"sha256:{fw}\", signature = \"sha256:{sig}\" }}"),
    );
    let described = form_path(
        out.path(),
        b"fw",
        &format!("{{ blob = \"sha256:{fw}\", descriptor = \"{GXWI_SERVER}\" }}"),
    );
    let plain = out.path().join("blobs/sha256").join(&fw);
    let inodes: Vec<u64> = [&signed, &described, &plain]
        .iter()
        .map(|path| fs::metadata(path).unwrap().ino())
        .collect();
    assert!(inodes[0] != inodes[1] && inodes[1] != inodes[2] && inodes[0] != inodes[2]);
    for path in [&signed, &described, &plain] {
        assert_eq!(fs::read(path).unwrap(), b"fw");
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o444);
    }
    // The signature itself is an ordinary blob.
    assert_eq!(
        fs::read(out.path().join("blobs/sha256").join(&sig)).unwrap(),
        signature(7)
    );
    // Either this host let ppk write the attribute, or ppk said it didn't.
    let stamped = has_xattr(&signed, SIG_XATTR);
    assert!(!has_xattr(&plain, SIG_XATTR));
    assert!(
        stamped || packed.unstamped > 0,
        "neither stamped nor counted"
    );
}

#[test]
fn a_closure_and_a_fetch_place_each_file_from_its_own_stored_file() {
    let input = firmware();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    ppk::pack_with_options(&id(), &firmware_options(), input.path(), &source).unwrap();
    let artifacts = dir.path().join("artifacts");
    ppk::fetch(&id(), &source, &artifacts).unwrap();
    let closed = ppk::closure(&[id()], &artifacts, &dir.path().join("closures")).unwrap();

    let (fw, sig) = (hex(b"fw"), hex(&signature(7)));
    let signed = form_path(
        &artifacts,
        b"fw",
        &format!("{{ blob = \"sha256:{fw}\", signature = \"sha256:{sig}\" }}"),
    );
    let ino = |path: &Path| fs::metadata(path).unwrap().ino();
    assert_eq!(
        ino(&closed.tree_path.join("lib/firmware/a.bin")),
        ino(&signed)
    );
    assert_eq!(
        ino(&closed.tree_path.join("share/plain")),
        ino(&artifacts.join("blobs/sha256").join(&fw))
    );
    assert!(
        !closed
            .tree_path
            .join("lib/firmware/a.bin.peios.sig")
            .exists()
    );
    assert!(closed.tree_path.join("libexec").is_dir());
    let stamped = has_xattr(&signed, SIG_XATTR);
    assert!(
        stamped || closed.unstamped > 0,
        "neither stamped nor counted"
    );
}

#[test]
fn two_packages_may_not_give_one_path_different_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = dir.path().join("artifacts");
    let plain = tempfile::tempdir().unwrap();
    write(plain.path(), "bin/server", b"fw");
    let a: PackageId = "dev.peios.a@1-1?arch=x86_64".parse().unwrap();
    let b: PackageId = "dev.peios.b@1-1?arch=x86_64".parse().unwrap();
    ppk::pack(&a, plain.path(), &artifacts).unwrap();
    let options = PackOptions {
        descriptors: descriptors(&[("bin/server", GXWI_SERVER)]),
        ..PackOptions::default()
    };
    ppk::pack_with_options(&b, &options, plain.path(), &artifacts).unwrap();
    let err = ppk::closure(&[a, b], &artifacts, &dir.path().join("closures")).unwrap_err();
    // The packages are named in the order of their identities.
    let message = format!("{err:#}");
    assert!(
        [
            "\"bin/server\": dev.peios.a@1-1?arch=x86_64 and dev.peios.b@1-1?arch=x86_64 \
             provide different entries",
            "\"bin/server\": dev.peios.b@1-1?arch=x86_64 and dev.peios.a@1-1?arch=x86_64 \
             provide different entries",
        ]
        .contains(&message.as_str()),
        "{message}"
    );
}

/// What a test adds to the tree it packs.
type Setup = Box<dyn Fn(&Path)>;

/// The error packing the tree `setup` makes gives, with `descriptors`,
/// after checking that nothing was written.
fn refusal(setup: impl Fn(&Path), descriptors: &[(&str, &str)]) -> String {
    let input = tempfile::tempdir().unwrap();
    write(input.path(), "lib/firmware/a.bin", b"fw");
    setup(input.path());
    let out = tempfile::tempdir().unwrap();
    let options = PackOptions {
        descriptors: self::descriptors(descriptors),
        ..PackOptions::default()
    };
    let err = ppk::pack_with_options(&id(), &options, input.path(), out.path()).unwrap_err();
    assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0, "{err:#}");
    format!("{err:#}")
}

#[test]
fn a_signature_must_have_a_signatures_shape_and_sign_a_file_that_is_not_elf() {
    let cases: [(Setup, &str); 7] = [
        (
            Box::new(|root| write(root, "lib/firmware/a.bin.peios.sig", &[1; 100])),
            "\"lib/firmware/a.bin.peios.sig\": a signature is 3310 bytes, but this is 100",
        ),
        (
            Box::new(|root| write(root, "lib/firmware/a.bin.peios.sig", &[2; 3310])),
            "\"lib/firmware/a.bin.peios.sig\": signature version is 0x02, not 0x01",
        ),
        (
            Box::new(|root| write(root, "lib/firmware/b.bin.peios.sig", &signature(7))),
            "\"lib/firmware/b.bin.peios.sig\": signs \"lib/firmware/b.bin\", which is not a \
             regular file of the package",
        ),
        (
            Box::new(|root| {
                symlink("a.bin", root.join("lib/firmware/link")).unwrap();
                write(root, "lib/firmware/link.peios.sig", &signature(7));
            }),
            "\"lib/firmware/link.peios.sig\": signs \"lib/firmware/link\", which is not a \
             regular file of the package",
        ),
        (
            Box::new(|root| {
                write(root, "bin/tool", b"\x7fELF\x02\x01\x01");
                write(root, "bin/tool.peios.sig", &signature(7));
            }),
            "\"bin/tool.peios.sig\": signs \"bin/tool\", an ELF file, which carries its \
             signature in its .peios.sig section",
        ),
        (
            Box::new(|root| symlink("a.bin", root.join("lib/firmware/b.peios.sig")).unwrap()),
            "\"lib/firmware/b.peios.sig\": a name ending in .peios.sig is for a file's signature",
        ),
        (
            Box::new(|root| write(root, "lib/.peios.sig", &signature(7))),
            "\"lib/.peios.sig\": a name ending in .peios.sig is for a file's signature",
        ),
    ];
    for (setup, expected) in cases {
        assert_eq!(refusal(setup, &[]), expected);
    }
}

#[test]
fn a_descriptor_is_for_a_file_or_directory_of_the_package() {
    let with_link = |root: &Path| symlink("a.bin", root.join("lib/firmware/link")).unwrap();
    let with_signature = |root: &Path| write(root, "lib/firmware/a.bin.peios.sig", &signature(7));
    let cases: [(Setup, &str, &str); 4] = [
        (
            Box::new(with_link),
            "lib/firmware/link",
            "\"lib/firmware/link\": a symlink has no security descriptor of its own",
        ),
        (
            Box::new(|_: &Path| {}),
            "lib/firmware/b.bin",
            "\"lib/firmware/b.bin\": given a security descriptor, but not in the package",
        ),
        (
            Box::new(with_signature),
            "lib/firmware/a.bin.peios.sig",
            "\"lib/firmware/a.bin.peios.sig\": given a security descriptor, but not in the \
             package",
        ),
        (
            Box::new(|_: &Path| {}),
            "",
            "\"\": given a security descriptor, but not in the package",
        ),
    ];
    for (setup, path, expected) in cases {
        assert_eq!(refusal(setup, &[(path, EVENTD)]), expected);
    }
    // Both a file and a directory that isn't empty can have one.
    let input = firmware();
    let out = tempfile::tempdir().unwrap();
    let options = PackOptions {
        descriptors: descriptors(&[("lib/firmware/a.bin", EVENTD), ("lib", EVENTD)]),
        ..PackOptions::default()
    };
    ppk::pack_with_options(&id(), &options, input.path(), out.path()).unwrap();
}

fn ppk_command(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ppk"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn pack_takes_descriptors_and_warns_of_what_it_could_not_stamp() {
    let input = firmware();
    let out = tempfile::tempdir().unwrap();
    let output = ppk_command(&[
        "pack",
        "dev.peios.firmware@1.0-1?arch=x86_64",
        input.path().to_str().unwrap(),
        "--artifact-dir",
        out.path().to_str().unwrap(),
        "--descriptor",
        "bin/server",
        &GXWI_SERVER.to_uppercase(),
    ]);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let identity: Identity = String::from_utf8(output.stdout)
        .unwrap()
        .trim_end()
        .parse()
        .unwrap();
    let manifest =
        fs::read_to_string(out.path().join("manifests/sha256").join(identity.hex())).unwrap();
    assert!(manifest.contains(&format!("descriptor = \"{GXWI_SERVER}\"")));
    let (fw, sig) = (hex(b"fw"), hex(&signature(7)));
    let signed = form_path(
        out.path(),
        b"fw",
        &format!("{{ blob = \"sha256:{fw}\", signature = \"sha256:{sig}\" }}"),
    );
    if !has_xattr(&signed, SIG_XATTR) {
        assert!(
            stderr.contains(
                "ppk: warning: 2 files and directories lack security metadata this system \
                 would not let ppk give; their manifests record it"
            ),
            "{stderr}"
        );
    }
}

#[test]
fn a_malformed_descriptor_is_a_usage_error() {
    let input = firmware();
    let out = tempfile::tempdir().unwrap();
    for (descriptor, message) in [
        ("01zz", "not a security descriptor in hexadecimal"),
        (
            "0100",
            "security descriptor is 2 bytes, shorter than its header",
        ),
    ] {
        let output = ppk_command(&[
            "pack",
            "dev.peios.firmware@1.0-1?arch=x86_64",
            input.path().to_str().unwrap(),
            "--artifact-dir",
            out.path().to_str().unwrap(),
            "--descriptor",
            "bin/server",
            descriptor,
        ]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!(
                "ppk: --descriptor bin/server: {message}\n\
                 Try 'ppk --help' for more information.\n"
            )
        );
    }
}
