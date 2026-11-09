//! Security metadata given for real: in unit tests, ppk writes `user.`
//! attributes in place of `security.` ones, which any process may set, so
//! these check on any host what ppk gives each stored and placed file and
//! directory. What they store is writable by its owner, as `user.`
//! attributes need. What only Peios does, setting a descriptor with
//! `kacs_set_sd`, is not covered here.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::security::SIGNATURE_BYTES;
use crate::security::tests::{EVENTD, GXWI_SERVER};
use crate::store::ArtifactDir;
use crate::{
    Entry, Extras, Identity, PackOptions, PackageId, RootArtifacts, SecurityDescriptor, UsrFiles,
    closure, closure_copying, compose_env, fetch, pack_with_options, parse_manifest,
};

const SD: &str = "user.peios.sd";
const SIG: &str = "user.peios.sig";

fn c(s: &[u8]) -> CString {
    CString::new(s).unwrap()
}

/// The attribute `name` of `path`, or `None` if it has none.
fn xattr(path: &Path, name: &str) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 65536];
    let (path_c, name_c) = (c(path.as_os_str().as_encoded_bytes()), c(name.as_bytes()));
    // SAFETY: NUL-terminated strings and a buffer of its length.
    let len = unsafe {
        libc::lgetxattr(
            path_c.as_ptr(),
            name_c.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if len < 0 {
        let err = io::Error::last_os_error();
        assert_eq!(
            err.raw_os_error(),
            Some(libc::ENODATA),
            "{}: {err}",
            path.display()
        );
        return None;
    }
    buf.truncate(len as usize);
    Some(buf)
}

fn set_xattr(path: &Path, name: &str, value: &[u8]) {
    let (path_c, name_c) = (c(path.as_os_str().as_encoded_bytes()), c(name.as_bytes()));
    // SAFETY: NUL-terminated strings and a buffer of its length.
    let ret = unsafe {
        libc::lsetxattr(
            path_c.as_ptr(),
            name_c.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    assert_eq!(ret, 0, "{}: {}", path.display(), io::Error::last_os_error());
}

fn remove_xattr(path: &Path, name: &str) {
    let (path_c, name_c) = (c(path.as_os_str().as_encoded_bytes()), c(name.as_bytes()));
    // SAFETY: NUL-terminated strings.
    let ret = unsafe { libc::lremovexattr(path_c.as_ptr(), name_c.as_ptr()) };
    assert_eq!(ret, 0, "{}: {}", path.display(), io::Error::last_os_error());
}

/// A signature's shape, filled with `fill`.
fn signature(fill: u8) -> Vec<u8> {
    let mut signature = vec![fill; SIGNATURE_BYTES];
    signature[0] = 1;
    signature
}

fn sd(hex: &str) -> SecurityDescriptor {
    hex.parse().unwrap()
}

fn package() -> PackageId {
    PackageId::new("dev.peios.firmware", "1.0-1", "x86_64").unwrap()
}

fn write(root: &Path, path: &str, content: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A package with one content, `fw`, three times: signed, with a descriptor
/// and plain; and a directory with a descriptor.
fn pack_firmware(artifacts: &Path) -> crate::Packed {
    try_pack_firmware(artifacts).unwrap()
}

fn try_pack_firmware(artifacts: &Path) -> anyhow::Result<crate::Packed> {
    let input = tempfile::tempdir().unwrap();
    write(input.path(), "lib/firmware/a.bin", b"fw");
    write(input.path(), "lib/firmware/a.bin.peios.sig", &signature(7));
    write(input.path(), "bin/server", b"fw");
    write(input.path(), "share/plain", b"fw");
    write(input.path(), "libexec/firmware/tool", b"tool");
    let options = PackOptions {
        descriptors: BTreeMap::from([
            ("bin/server".to_owned(), sd(GXWI_SERVER)),
            ("libexec".to_owned(), sd(EVENTD)),
        ]),
        ..PackOptions::default()
    };
    pack_with_options(&package(), &options, input.path(), artifacts)
}

fn entries(artifacts: &Path) -> BTreeMap<String, Entry> {
    let layout = ArtifactDir::new(artifacts);
    let identity = crate::packed(&package(), artifacts).unwrap().unwrap();
    let bytes = fs::read(layout.manifest_path(&identity)).unwrap();
    parse_manifest(&bytes)
        .unwrap()
        .entries
        .into_iter()
        .collect()
}

#[test]
fn each_file_with_metadata_is_stored_on_its_own_carrying_it() {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = dir.path().join("artifacts");
    let packed = pack_firmware(&artifacts);
    assert_eq!(packed.unstamped, 0);

    let entries = entries(&artifacts);
    let fw = Identity::of(b"fw").hex().to_owned();
    let signed_hex = Identity::of(&signature(7)).hex().to_owned();
    assert_eq!(
        entries["lib/firmware/a.bin"],
        Entry::Blob {
            hex: fw.clone(),
            exec: false,
            descriptor: None,
            signature: Some(signed_hex.clone()),
        }
    );
    assert!(!entries.contains_key("lib/firmware/a.bin.peios.sig"));
    assert_eq!(
        entries["libexec"],
        Entry::Dir {
            descriptor: Some(sd(EVENTD))
        }
    );
    assert_eq!(entries["share/plain"], Entry::file(fw.clone(), false));

    let layout = ArtifactDir::new(&artifacts);
    let signed = layout.file_path(&entries["lib/firmware/a.bin"]);
    let server = layout.file_path(&entries["bin/server"]);
    let plain = layout.file_path(&entries["share/plain"]);
    assert_eq!(plain, layout.blob_path(&fw, false));
    let inodes: Vec<u64> = [&signed, &server, &plain]
        .iter()
        .map(|path| fs::metadata(path).unwrap().ino())
        .collect();
    assert!(inodes[0] != inodes[1] && inodes[1] != inodes[2] && inodes[0] != inodes[2]);
    for path in [&signed, &server] {
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(&format!("{fw}.")), "{name}");
    }

    assert_eq!(xattr(&signed, SIG), Some(signature(7)));
    assert_eq!(xattr(&signed, SD), None);
    assert_eq!(
        xattr(&server, SD),
        Some(sd(GXWI_SERVER).as_bytes().to_vec())
    );
    assert_eq!(xattr(&server, SIG), None);
    assert_eq!(xattr(&plain, SIG), None);
    assert_eq!(xattr(&plain, SD), None);
    assert_eq!(
        fs::read(layout.blob_path(&signed_hex, false)).unwrap(),
        signature(7)
    );
}

#[test]
fn a_closure_links_each_file_to_the_stored_file_with_its_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = dir.path().join("artifacts");
    pack_firmware(&artifacts);
    let closed = closure(&[package()], &artifacts, &dir.path().join("closures")).unwrap();
    assert_eq!(closed.unstamped, 0);

    let entries = entries(&artifacts);
    let layout = ArtifactDir::new(&artifacts);
    let tree = &closed.tree_path;
    for path in ["lib/firmware/a.bin", "bin/server", "share/plain"] {
        assert_eq!(
            fs::metadata(tree.join(path)).unwrap().ino(),
            fs::metadata(layout.file_path(&entries[path]))
                .unwrap()
                .ino(),
            "{path}"
        );
    }
    assert_eq!(
        xattr(&tree.join("lib/firmware/a.bin"), SIG),
        Some(signature(7))
    );
    assert_eq!(
        xattr(&tree.join("bin/server"), SD),
        Some(sd(GXWI_SERVER).as_bytes().to_vec())
    );
    assert_eq!(xattr(&tree.join("share/plain"), SIG), None);
    assert_eq!(
        xattr(&tree.join("libexec"), SD),
        Some(sd(EVENTD).as_bytes().to_vec())
    );
    assert_eq!(xattr(&tree.join("libexec/firmware"), SD), None);
    assert!(!tree.join("lib/firmware/a.bin.peios.sig").exists());
}

#[test]
fn a_copy_carries_the_metadata_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = dir.path().join("artifacts");
    pack_firmware(&artifacts);
    let closed = closure_copying(&[package()], &artifacts, &dir.path().join("closures")).unwrap();
    assert_eq!(closed.unstamped, 0);

    let entries = entries(&artifacts);
    let layout = ArtifactDir::new(&artifacts);
    let copy = closed.tree_path.join("lib/firmware/a.bin");
    assert_ne!(
        fs::metadata(&copy).unwrap().ino(),
        fs::metadata(layout.file_path(&entries["lib/firmware/a.bin"]))
            .unwrap()
            .ino()
    );
    assert_eq!(xattr(&copy, SIG), Some(signature(7)));
    assert_eq!(
        xattr(&closed.tree_path.join("bin/server"), SD),
        Some(sd(GXWI_SERVER).as_bytes().to_vec())
    );
}

#[test]
fn fetching_gives_each_stored_file_its_metadata_from_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    pack_firmware(&source);
    // What the source's stored file carries is not what the copy is given.
    let layout = ArtifactDir::new(&source);
    let entries = entries(&source);
    let stored = layout.file_path(&entries["lib/firmware/a.bin"]);
    remove_xattr(&stored, SIG);

    let artifacts = dir.path().join("artifacts");
    let fetched = fetch(&package(), &source, &artifacts).unwrap();
    assert_eq!(fetched.unstamped, 0);
    let to = ArtifactDir::new(&artifacts);
    assert_eq!(
        xattr(&to.file_path(&entries["lib/firmware/a.bin"]), SIG),
        Some(signature(7))
    );
    assert_eq!(
        xattr(&to.file_path(&entries["bin/server"]), SD),
        Some(sd(GXWI_SERVER).as_bytes().to_vec())
    );

    let root = dir.path().join("root");
    let extras = Extras {
        artifacts: RootArtifacts::Include,
        files: UsrFiles::Hardlink,
        ..Extras::default()
    };
    let composed = compose_env(&[package()], &artifacts, &root, &extras).unwrap();
    assert_eq!(composed.unstamped, 0);
    assert_eq!(
        xattr(&root.join("usr/lib/firmware/a.bin"), SIG),
        Some(signature(7))
    );
    assert_eq!(
        xattr(&root.join("usr/libexec"), SD),
        Some(sd(EVENTD).as_bytes().to_vec())
    );
    let signed_hex = Identity::of(&signature(7)).hex().to_owned();
    assert!(
        root.join("system/artifacts/blobs/sha256")
            .join(signed_hex)
            .is_file()
    );
}

#[test]
fn a_stored_file_missing_its_signature_is_given_it_and_a_wrong_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = dir.path().join("artifacts");
    pack_firmware(&artifacts);
    let entries = entries(&artifacts);
    let stored = ArtifactDir::new(&artifacts).file_path(&entries["lib/firmware/a.bin"]);

    remove_xattr(&stored, SIG);
    let closed = closure(&[package()], &artifacts, &dir.path().join("first")).unwrap();
    assert_eq!(closed.unstamped, 0);
    assert_eq!(xattr(&stored, SIG), Some(signature(7)));

    set_xattr(&stored, SIG, &signature(9));
    let err = closure(&[package()], &artifacts, &dir.path().join("second")).unwrap_err();
    assert!(
        format!("{err:#}").ends_with(&format!("{SIG}: holds something else")),
        "{err:#}"
    );
    let err = try_pack_firmware(&artifacts).unwrap_err();
    assert!(
        format!("{err:#}").ends_with(&format!("{SIG}: holds something else")),
        "{err:#}"
    );
}
