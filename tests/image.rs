//! A closure listed for an image that carries its own security metadata.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use ppk::{DESCRIPTOR_ATTRIBUTE, ImageKind, PackageId, SecurityDescriptor, UsrFiles};

const HELLO: &str = "dev.peios.hello@1.0-1?arch=x86_64";

fn hello(artifacts: &std::path::Path) -> PackageId {
    let content = tempfile::tempdir().unwrap();
    fs::create_dir_all(content.path().join("bin")).unwrap();
    fs::create_dir_all(content.path().join("share/doc/hello")).unwrap();
    fs::write(content.path().join("bin/hello"), b"binary").unwrap();
    fs::set_permissions(
        content.path().join("bin/hello"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::write(content.path().join("share/doc/hello/README"), b"docs").unwrap();
    symlink("hello", content.path().join("bin/hi")).unwrap();
    let id: PackageId = HELLO.parse().unwrap();
    ppk::pack(&id, content.path(), artifacts).unwrap();
    id
}

#[test]
fn an_image_lists_the_tree_a_closure_composes() {
    let artifacts = tempfile::tempdir().unwrap();
    let id = hello(artifacts.path());
    let requests = ppk::exact_requests(std::slice::from_ref(&id)).unwrap();

    let image = ppk::closure_image(std::slice::from_ref(&id), &requests, artifacts.path()).unwrap();

    let listed: Vec<(&str, u32)> = image
        .entries
        .iter()
        .map(|each| (each.path.as_str(), each.mode))
        .collect();
    assert_eq!(
        listed,
        [
            ("bin", 0o755),
            ("bin/hello", 0o555),
            ("bin/hi", 0o777),
            ("inventory", 0o444),
            ("share", 0o755),
            ("share/doc", 0o755),
            ("share/doc/hello", 0o755),
            ("share/doc/hello/README", 0o444),
        ]
    );
    assert!(matches!(&image.entries[2].kind, ImageKind::Symlink(target) if target == "hello"));
    let ImageKind::Blob(blob) = &image.entries[1].kind else {
        panic!("bin/hello is a file");
    };
    assert_eq!(fs::read(blob).unwrap(), b"binary");

    // The same closure as one composed, whose inventory it carries.
    let closures = tempfile::tempdir().unwrap();
    let closed = ppk::closure_for(
        std::slice::from_ref(&id),
        &requests,
        artifacts.path(),
        closures.path(),
        UsrFiles::Copy,
    )
    .unwrap();
    assert_eq!(image.identity.to_string(), closed.identity);
    let ImageKind::Bytes(inventory) = &image.entries[3].kind else {
        panic!("the inventory is the closure's own");
    };
    assert_eq!(
        *inventory,
        fs::read(closed.tree_path.join("inventory")).unwrap()
    );
}

#[test]
fn everything_in_an_image_carries_a_whole_descriptor() {
    let artifacts = tempfile::tempdir().unwrap();
    let id = hello(artifacts.path());
    let requests = ppk::exact_requests(std::slice::from_ref(&id)).unwrap();

    let image = ppk::closure_image(std::slice::from_ref(&id), &requests, artifacts.path()).unwrap();

    for each in std::iter::once(&image.root).chain(&image.entries) {
        let descriptors: Vec<_> = each
            .attributes
            .iter()
            .filter(|(name, _)| *name == DESCRIPTOR_ATTRIBUTE)
            .collect();
        assert_eq!(descriptors.len(), 1, "{}", each.path);
        // An owner, a group and a DACL, as the kernel requires of a stored
        // descriptor.
        SecurityDescriptor::from_bytes(descriptors[0].1.clone())
            .unwrap_or_else(|err| panic!("{}: {err:#}", each.path));
    }
}
