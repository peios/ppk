use std::fs;
use std::path::Path;
use std::process::Command;

use ppk::{Identity, PackageId};

fn id(value: &str) -> PackageId {
    value.parse().unwrap()
}

fn closure_of(package: &str, store: &Path) -> Identity {
    let input = tempfile::tempdir().unwrap();
    fs::write(input.path().join("file"), package).unwrap();
    ppk::pack(&id(package), input.path(), store).unwrap();
    let closed = ppk::closure(&[id(package)], store, &store.join("closures")).unwrap();
    closed.identity.parse().unwrap()
}

#[test]
fn a_name_points_at_one_closure_and_can_be_moved_and_removed() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    let closures = store.join("closures");
    let first = closure_of("dev.peios.first@1.0-1?arch=noarch", &store);
    let second = closure_of("dev.peios.second@1.0-1?arch=noarch", &store);

    assert_eq!(ppk::named_closure(&closures, ppk::SYSTEM).unwrap(), None);
    ppk::name_closure(&closures, ppk::SYSTEM, &first).unwrap();
    ppk::name_closure(&closures, ppk::INITRAMFS, &second).unwrap();
    assert_eq!(
        ppk::named_closure(&closures, ppk::SYSTEM).unwrap(),
        Some(first.clone())
    );
    // A name is a relative link to the closure's tree beside it.
    assert_eq!(
        fs::read_link(closures.join("SYSTEM")).unwrap(),
        Path::new(first.hex())
    );

    ppk::name_closure(&closures, ppk::SYSTEM, &second).unwrap();
    assert_eq!(
        ppk::closure_names(&closures).unwrap(),
        [
            ("INITRAMFS".to_owned(), second.clone()),
            ("SYSTEM".to_owned(), second.clone()),
        ]
    );
    // Moving a name leaves nothing else behind.
    let leftovers: Vec<_> = fs::read_dir(&closures)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    ppk::remove_closure_name(&closures, ppk::SYSTEM).unwrap();
    assert_eq!(ppk::named_closure(&closures, ppk::SYSTEM).unwrap(), None);
    assert!(closures.join(first.hex()).is_dir());
    assert!(ppk::remove_closure_name(&closures, ppk::SYSTEM).is_err());
}

#[test]
fn names_that_could_be_mistaken_for_closures_or_unbuilt_ones_are_refused() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    let closures = store.join("closures");
    let closure = closure_of("dev.peios.first@1.0-1?arch=noarch", &store);
    for bad in [
        "",
        ".hidden",
        ".tmp-SYSTEM",
        "a/b",
        "x.manifest",
        closure.hex(),
        &"N".repeat(65),
    ] {
        assert!(
            ppk::name_closure(&closures, bad, &closure).is_err(),
            "{bad}"
        );
    }
    let absent = Identity::of(b"no such closure");
    let err = ppk::name_closure(&closures, "SYSTEM", &absent).unwrap_err();
    assert!(err.to_string().contains("no such closure"), "{err}");
}

#[test]
fn the_name_command_sets_shows_lists_and_removes() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    let closure = closure_of("dev.peios.first@1.0-1?arch=noarch", &store);
    let ppk = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_ppk"))
            .args(args)
            .arg("--artifact-dir")
            .arg(&store)
            .output()
            .unwrap()
    };
    let identity = closure.to_string();
    assert!(ppk(&["name", "SYSTEM", &identity]).status.success());
    let shown = ppk(&["name", "SYSTEM"]);
    assert_eq!(
        String::from_utf8(shown.stdout).unwrap(),
        format!("{identity}\n")
    );
    let listed = ppk(&["name"]);
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        format!("SYSTEM {identity}\n")
    );
    assert!(ppk(&["name", "--remove", "SYSTEM"]).status.success());
    let shown = ppk(&["name", "SYSTEM"]);
    assert_eq!(shown.status.code(), Some(1));
}
