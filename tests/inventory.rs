use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output};

use ppk::{Extras, Identity, PackageId, UsrFiles};

const FIRST: &str = "dev.peios.first@1.0-1?arch=noarch";
const SECOND: &str = "dev.peios.second@2.0-1?arch=x86_64";

fn id(value: &str) -> PackageId {
    value.parse().unwrap()
}

fn pack(package: &str, store: &Path) -> String {
    let input = tempfile::tempdir().unwrap();
    ppk::pack(&id(package), input.path(), store)
        .unwrap()
        .identity
}

fn list(root: &Path, store: &Path, all: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ppk"));
    command
        .arg("list")
        .arg("--root")
        .arg(root)
        .arg("--artifact-dir")
        .arg(store);
    if all {
        command.arg("--all");
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).unwrap()
}

fn check_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn replace(path: &Path, text: &str) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn inventories_preserve_closure_identity_for_both_file_modes() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    let first = pack(FIRST, &store);
    let second = pack(SECOND, &store);
    let mut entries = [(first.as_str(), FIRST), (second.as_str(), SECOND)];
    entries.sort();
    let manifest = format!(
        "packages = [\n    \"{}\",\n    \"{}\",\n]\n",
        entries[0].0, entries[1].0
    );
    let expected = Identity::of(manifest.as_bytes()).to_string();
    for (number, files) in [UsrFiles::Hardlink, UsrFiles::Copy].into_iter().enumerate() {
        let closures = work.path().join(format!("closures-{number}"));
        let packages = [id(SECOND), id(FIRST), id(FIRST)];
        let closed = match files {
            UsrFiles::Hardlink => ppk::closure(&packages, &store, &closures),
            UsrFiles::Copy => ppk::closure_copying(&packages, &store, &closures),
        }
        .unwrap();
        assert_eq!(closed.identity, expected);
        let root = work.path().join(format!("root-{number}"));
        ppk::compose_env(
            &packages,
            &store,
            &root,
            &Extras {
                files,
                ..Extras::default()
            },
        )
        .unwrap();
        let text = fs::read_to_string(root.join("usr/inventory")).unwrap();
        assert_eq!(
            text,
            fs::read_to_string(closed.tree_path.join("inventory")).unwrap()
        );
        assert_eq!(
            text,
            format!(
                "closure = \"{expected}\"\n\n[packages]\n\"{}\" = \"{}\"\n\"{}\" = \"{}\"\n",
                entries[0].0, entries[0].1, entries[1].0, entries[1].1
            )
        );
        assert_eq!(
            fs::metadata(root.join("usr/inventory"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
        let read = ppk::read_inventory(&root.join("usr")).unwrap().unwrap();
        assert_eq!(read.identity.to_string(), expected);
        assert_eq!(read.packages.len(), 2);
    }
}

#[test]
fn list_needs_no_store_and_all_marks_the_exact_package_identity() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    pack(FIRST, &store);
    pack(SECOND, &store);
    let root = work.path().join("root");
    ppk::compose_env(&[id(FIRST)], &store, &root, &Extras::default()).unwrap();
    let output = list(&root, &work.path().join("absent"), false);
    check_success(&output);
    assert_eq!(stdout(&output), format!("{FIRST}\n"));
    let output = list(&root, &store, true);
    check_success(&output);
    assert_eq!(stdout(&output), format!("* {FIRST}\n  {SECOND}\n"));
    // A replaced ref for the same full ID is not the running package.
    let input = tempfile::tempdir().unwrap();
    fs::write(input.path().join("changed"), "changed").unwrap();
    ppk::pack_replacing(&id(FIRST), input.path(), &store).unwrap();
    let output = list(&root, &store, true);
    check_success(&output);
    assert_eq!(stdout(&output), format!("  {FIRST}\n  {SECOND}\n"));
    // Listing the store reads refs alone, including every architecture.
    fs::remove_dir_all(store.join("manifests")).unwrap();
    let output = list(work.path(), &store, true);
    check_success(&output);
    assert_eq!(stdout(&output), format!("  {FIRST}\n  {SECOND}\n"));
    let output = list(work.path(), &store, false);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("wasn't composed by a ppk that records an inventory")
    );
}

#[test]
fn list_sorts_full_ids_instead_of_inventory_order() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    pack(FIRST, &store);
    pack(SECOND, &store);
    let root = work.path().join("root");
    ppk::compose_env(&[id(SECOND), id(FIRST)], &store, &root, &Extras::default()).unwrap();
    let output = list(&root, &store, false);
    check_success(&output);
    assert_eq!(stdout(&output), format!("{FIRST}\n{SECOND}\n"));
}

#[test]
fn malformed_inventories_are_errors_even_with_all() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    let hash = pack(FIRST, &store);
    let root = work.path().join("root");
    ppk::compose_env(&[id(FIRST)], &store, &root, &Extras::default()).unwrap();
    let path = root.join("usr/inventory");
    let valid = fs::read_to_string(&path).unwrap();
    for bad in [
        String::from("not TOML"),
        valid.replace(&hash, &Identity::of(b"wrong package").to_string()),
        valid.replace(FIRST, "dev.peios.first@1.0-1"),
        valid.replace(FIRST, "invalid"),
        valid.replace("[packages]", "[other]"),
        format!("{valid}\"{hash}\" = \"{FIRST}\"\n"),
        valid.replace("\n\n", "\n"),
        valid.replace("[packages]", "unknown = true\n[packages]"),
        valid.lines().take(3).collect::<Vec<_>>().join("\n"),
    ] {
        replace(&path, &bad);
        assert!(ppk::read_inventory(&root.join("usr")).is_err(), "{bad}");
        for all in [false, true] {
            let output = list(&root, &store, all);
            assert_eq!(output.status.code(), Some(1), "{bad}");
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
fn cached_closures_are_read_only_and_older_ones_stay_usable() {
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    pack(FIRST, &store);
    let closures = store.join("closures");
    let packages = [id(FIRST)];
    let closed = ppk::closure(&packages, &store, &closures).unwrap();
    assert!(ppk::closure(&packages, &store, &closures).unwrap().reused);
    let path = closed.tree_path.join("inventory");
    replace(&path, "malformed");
    assert!(ppk::closure(&packages, &store, &closures).is_err());
    fs::remove_file(&path).unwrap();
    assert!(ppk::closure(&packages, &store, &closures).unwrap().reused);
    assert!(!path.exists(), "older closures are reused without mutation");
}

#[test]
fn pack_reserves_inventory_for_every_kind_of_entry() {
    for kind in ["file", "directory", "nonempty directory", "symlink"] {
        let work = tempfile::tempdir().unwrap();
        let input = work.path().join("input");
        fs::create_dir(&input).unwrap();
        let inventory = input.join("inventory");
        match kind {
            "file" => fs::write(&inventory, "forged").unwrap(),
            "directory" => fs::create_dir(&inventory).unwrap(),
            "nonempty directory" => {
                fs::create_dir(&inventory).unwrap();
                fs::write(inventory.join("child"), "forged").unwrap();
            }
            _ => symlink("missing", &inventory).unwrap(),
        }
        let store = work.path().join("store");
        let error = ppk::pack(&id(FIRST), &input, &store).unwrap_err();
        assert!(
            error.to_string().contains("/usr/inventory is reserved"),
            "{error:#}"
        );
        assert!(!store.exists());
    }
}

#[test]
fn inventory_cannot_be_claimed_or_supplied_by_an_imported_package() {
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("input");
    fs::create_dir(&input).unwrap();
    let store = work.path().join("store");
    let metadata = ppk::Metadata {
        claims: [("/usr/inventory".into(), "elsewhere".into())].into(),
        ..ppk::Metadata::default()
    };
    assert!(
        ppk::pack_with(&id(FIRST), &metadata, &input, &store)
            .unwrap_err()
            .to_string()
            .contains("reserved")
    );
    // Emulate an older or imported manifest bypassing pack's reservation.
    let manifest = ppk::render_manifest(
        &id(FIRST),
        &ppk::Metadata::default(),
        &[("inventory", ppk::Entry::Symlink("elsewhere".into()))],
    );
    let identity = Identity::of(manifest.as_bytes());
    let manifests = store.join("manifests/sha256");
    fs::create_dir_all(&manifests).unwrap();
    fs::write(manifests.join(identity.hex()), manifest).unwrap();
    let refs = store.join("refs/dev.peios.first/1.0-1");
    fs::create_dir_all(&refs).unwrap();
    fs::write(refs.join("noarch"), format!("{identity}\n")).unwrap();
    let error = ppk::closure(&[id(FIRST)], &store, &store.join("closures")).unwrap_err();
    assert!(error.to_string().contains("reserved"), "{error:#}");
    assert!(!store.join("closures").exists());
}
