//! Deriving requirements and provides from a payload's ELF objects.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use ppk::{Constraint, Metadata, PackageId, Version, parse_manifest};

/// An ELF shared object or program, as much of one as derivation reads: a
/// dynamic section and GNU version tables, through section headers.
#[derive(Default, Clone)]
struct Elf {
    wide_big_endian: bool,
    soname: Option<&'static str>,
    needed: Vec<&'static str>,
    /// Version needs, by library, each with its flags.
    needs: Vec<(&'static str, Vec<(&'static str, u16)>)>,
    /// Version definitions, each with its flags.
    defines: Vec<(&'static str, u16)>,
    /// Write a DT_VERNEEDNUM one more than there are needs.
    miscount_needs: bool,
}

const BASE: u16 = 1;
const WEAK: u16 = 2;

/// A 32-bit little-endian object, unless asked for the other kind.
struct Writer {
    wide: bool,
    big: bool,
    out: Vec<u8>,
}

impl Writer {
    fn u16(&mut self, v: u16) {
        self.out.extend(if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        });
    }
    fn u32(&mut self, v: u32) {
        self.out.extend(if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        });
    }
    fn word(&mut self, v: u64) {
        if self.wide {
            self.out.extend(if self.big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            });
        } else {
            self.u32(v as u32);
        }
    }
}

impl Elf {
    fn so(soname: &'static str) -> Self {
        Self {
            soname: Some(soname),
            ..Self::default()
        }
    }

    fn needing(needed: &[&'static str]) -> Self {
        Self {
            needed: needed.to_vec(),
            ..Self::default()
        }
    }

    fn with_needs(mut self, file: &'static str, versions: &[(&'static str, u16)]) -> Self {
        self.needs.push((file, versions.to_vec()));
        self
    }

    fn bytes(&self) -> Vec<u8> {
        let wide = self.wide_big_endian;
        let mut w = Writer {
            wide,
            big: wide,
            out: Vec::new(),
        };
        let header_size: u64 = if wide { 64 } else { 52 };

        let mut strings = vec![0u8];
        let mut string = |s: &str| -> u32 {
            let at = strings.len() as u32;
            strings.extend(s.as_bytes());
            strings.push(0);
            at
        };
        let soname = self.soname.map(&mut string);
        let needed: Vec<u32> = self.needed.iter().map(|s| string(s)).collect();
        let needs: Vec<(u32, Vec<(u32, u16)>)> = self
            .needs
            .iter()
            .map(|(file, versions)| {
                let file = string(file);
                let versions = versions.iter().map(|(v, f)| (string(v), *f)).collect();
                (file, versions)
            })
            .collect();
        let defines: Vec<(u32, u16)> = self.defines.iter().map(|(v, f)| (string(v), *f)).collect();

        // Layout: header, strings, verneed, verdef, dynamic, section headers.
        // Addresses equal offsets.
        let strings_at = header_size;
        let verneed_at = strings_at + strings.len() as u64;
        let verneed_len: u64 = needs.iter().map(|(_, v)| 16 + 16 * v.len() as u64).sum();
        let verdef_at = verneed_at + verneed_len;
        let verdef_len = 28 * defines.len() as u64;
        let dynamic_at = verdef_at + verdef_len;
        let mut dynamic: Vec<(u64, u64)> = needed.iter().map(|&n| (1, u64::from(n))).collect();
        if let Some(soname) = soname {
            dynamic.push((14, u64::from(soname)));
        }
        if !needs.is_empty() {
            let count = needs.len() as u64 + u64::from(self.miscount_needs);
            dynamic.extend([(0x6fff_fffe, verneed_at), (0x6fff_ffff, count)]);
        }
        if !defines.is_empty() {
            dynamic.extend([
                (0x6fff_fffc, verdef_at),
                (0x6fff_fffd, defines.len() as u64),
            ]);
        }
        dynamic.push((0, 0));
        let dyn_entry: u64 = if wide { 16 } else { 8 };
        let dynamic_len = dyn_entry * dynamic.len() as u64;
        let sections_at = dynamic_at + dynamic_len;
        // null, strings, dynamic, then the version tables there are.
        let mut sections = vec![(0, 0, 0, 0, 0), (3, strings_at, strings.len() as u64, 0, 0)];
        sections.push((6, dynamic_at, dynamic_len, 1, 0));
        if !needs.is_empty() {
            sections.push((0x6fff_fffe, verneed_at, verneed_len, 1, needs.len() as u32));
        }
        if !defines.is_empty() {
            sections.push((0x6fff_fffd, verdef_at, verdef_len, 1, defines.len() as u32));
        }

        w.out.extend(b"\x7fELF");
        w.out
            .extend([if wide { 2 } else { 1 }, if wide { 2 } else { 1 }, 1]);
        w.out.resize(16, 0);
        w.u16(3);
        w.u16(if wide { 62 } else { 3 });
        w.u32(1);
        w.word(0);
        w.word(0);
        w.word(sections_at);
        w.u32(0);
        w.u16(header_size as u16);
        w.u16(0);
        w.u16(0);
        w.u16(if wide { 64 } else { 40 });
        w.u16(sections.len() as u16);
        w.u16(0);
        assert_eq!(w.out.len() as u64, header_size);

        w.out.extend(&strings);
        for (i, (file, versions)) in needs.iter().enumerate() {
            w.u16(1);
            w.u16(versions.len() as u16);
            w.u32(*file);
            w.u32(16);
            let last = i + 1 == needs.len();
            w.u32(if last {
                0
            } else {
                16 + 16 * versions.len() as u32
            });
            for (j, (name, flags)) in versions.iter().enumerate() {
                w.u32(0);
                w.u16(*flags);
                w.u16(0);
                w.u32(*name);
                w.u32(if j + 1 == versions.len() { 0 } else { 16 });
            }
        }
        for (i, (name, flags)) in defines.iter().enumerate() {
            w.u16(1);
            w.u16(*flags);
            w.u16(i as u16 + 1);
            w.u16(1);
            w.u32(0);
            w.u32(20);
            w.u32(if i + 1 == defines.len() { 0 } else { 28 });
            w.u32(*name);
            w.u32(0);
        }
        for (tag, value) in &dynamic {
            w.word(*tag);
            w.word(*value);
        }
        assert_eq!(w.out.len() as u64, sections_at);
        for (kind, offset, size, link, info) in sections {
            w.u32(0);
            w.u32(kind);
            w.word(0);
            w.word(offset);
            w.word(offset);
            w.word(size);
            w.u32(link);
            w.u32(info);
            w.word(0);
            w.word(0);
        }
        w.out
    }
}

fn glibc_program() -> Elf {
    Elf::needing(&["libc.so.6", "libfoo.so.1"]).with_needs(
        "libc.so.6",
        &[("GLIBC_2.2.5", 0), ("GLIBC_2.34", 0), ("GLIBC_PRIVATE", 0)],
    )
}

fn libstdcxx() -> Elf {
    Elf {
        defines: vec![
            ("libstdc++.so.6", BASE),
            ("GLIBCXX_3.4", 0),
            ("GLIBCXX_3.4.21", 0),
            ("CXXABI_1.3", 0),
        ],
        ..Elf::so("libstdc++.so.6")
    }
}

fn hello() -> PackageId {
    PackageId::new("dev.peios.hello", "1.0-1", "x86_64").unwrap()
}

/// A payload of objects, each at its path.
fn payload(objects: &[(&str, &Elf)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, elf) in objects {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, elf.bytes()).unwrap();
    }
    dir
}

fn pack(
    id: &PackageId,
    input: &Path,
    written: &Metadata,
) -> anyhow::Result<(ppk::Packed, Metadata)> {
    let out = tempfile::tempdir().unwrap();
    let packed = ppk::pack_with(id, written, input, out.path())?;
    let manifest = parse_manifest(&fs::read(&packed.manifest_path).unwrap()).unwrap();
    Ok((packed, manifest.metadata))
}

fn derived(input: &Path) -> Metadata {
    pack(&hello(), input, &Metadata::default()).unwrap().1
}

fn requires(metadata: &Metadata) -> BTreeMap<&str, String> {
    metadata
        .requires
        .iter()
        .map(|(name, c)| (name.as_str(), c.to_manifest_string()))
        .collect()
}

fn provides(metadata: &Metadata) -> BTreeMap<&str, String> {
    metadata
        .provides
        .iter()
        .map(|(name, v)| {
            (
                name.as_str(),
                v.as_ref()
                    .map_or_else(String::new, |v| v.as_str().to_owned()),
            )
        })
        .collect()
}

fn constraint(s: &str) -> Constraint {
    Constraint::from_manifest_string(s).unwrap()
}

#[test]
fn needed_libraries_are_required_and_sonames_provided() {
    let input = payload(&[
        ("bin/hello", &glibc_program()),
        ("lib/libgreet.so.2", &Elf::so("libgreet.so.2")),
    ]);
    let metadata = derived(input.path());
    assert_eq!(
        requires(&metadata),
        BTreeMap::from([
            ("libc.so.6", ">= 2.34".to_owned()),
            ("libfoo.so.1", String::new())
        ])
    );
    assert_eq!(
        provides(&metadata),
        BTreeMap::from([("libgreet.so.2", String::new())])
    );
}

#[test]
fn a_library_the_package_provides_is_not_required() {
    let input = payload(&[
        ("bin/hello", &glibc_program()),
        ("lib/libfoo.so.1.2.3", &Elf::so("libfoo.so.1")),
    ]);
    let metadata = derived(input.path());
    assert_eq!(
        requires(&metadata),
        BTreeMap::from([("libc.so.6", ">= 2.34".to_owned())])
    );
}

#[test]
fn the_highest_floor_of_any_object_wins_and_weak_needs_count() {
    let older = Elf::needing(&["libc.so.6", "libgcc_s.so.1"])
        .with_needs("libc.so.6", &[("GLIBC_2.38", 0)])
        .with_needs("libgcc_s.so.1", &[("GCC_3.0", 0), ("GCC_4.3.0", WEAK)]);
    let newer = Elf::needing(&["libc.so.6", "libm.so.6"])
        .with_needs("libc.so.6", &[("GLIBC_2.4", 0), ("GLIBC_2.39", WEAK)])
        .with_needs("libm.so.6", &[("GLIBC_2.2.5", 0)]);
    let input = payload(&[("bin/a", &older), ("bin/b", &newer)]);
    assert_eq!(
        requires(&derived(input.path())),
        BTreeMap::from([
            ("libc.so.6", ">= 2.39".to_owned()),
            ("libgcc_s.so.1", ">= 4.3.0".to_owned()),
            ("libm.so.6", ">= 2.2.5".to_owned()),
        ])
    );
}

#[test]
fn only_the_listed_libraries_get_floors() {
    let program = Elf::needing(&["libz.so.1"]).with_needs("libz.so.1", &[("ZLIB_1.2.9", 0)]);
    let input = payload(&[("bin/a", &program)]);
    assert_eq!(
        requires(&derived(input.path())),
        BTreeMap::from([("libz.so.1", String::new())])
    );
}

#[test]
fn a_floor_library_is_provided_at_the_package_version() {
    let input = payload(&[
        ("lib/libc.so.6", &Elf::so("libc.so.6")),
        ("lib/libBrokenLocale.so.1", &Elf::so("libBrokenLocale.so.1")),
    ]);
    let id = PackageId::new("org.gnu.glibc", "2.44-1", "x86_64").unwrap();
    let (_, metadata) = pack(&id, input.path(), &Metadata::default()).unwrap();
    assert_eq!(
        provides(&metadata),
        BTreeMap::from([
            ("libBrokenLocale.so.1", String::new()),
            ("libc.so.6", "2.44-1".to_owned())
        ])
    );
}

#[test]
fn source_symlinks_and_other_files_are_not_read() {
    let input = payload(&[
        ("src/glibc/test/libc.so.6", &Elf::so("libc.so.6")),
        ("lib/libgreet.so.2", &Elf::so("libgreet.so.2")),
    ]);
    symlink("libgreet.so.2", input.path().join("lib/libgreet.so")).unwrap();
    symlink("/nowhere", input.path().join("lib/libdangling.so")).unwrap();
    // glibc's libc.so is a linker script.
    fs::write(input.path().join("lib/libc.so"), "GROUP ( libc.so.6 )\n").unwrap();
    fs::create_dir(input.path().join("bin")).unwrap();
    fs::write(input.path().join("bin/short"), "\x7fEL").unwrap();
    let (packed, metadata) = pack(&hello(), input.path(), &Metadata::default()).unwrap();
    assert!(metadata.requires.is_empty());
    assert_eq!(
        provides(&metadata),
        BTreeMap::from([("libgreet.so.2", String::new())])
    );
    assert_eq!(packed.warnings, Vec::<String>::new());
}

#[test]
fn a_library_without_a_soname_is_a_warning() {
    let input = payload(&[
        ("lib/libplugin.so", &Elf::needing(&["libc.so.6"])),
        ("bin/tool", &Elf::default()),
    ]);
    let (packed, metadata) = pack(&hello(), input.path(), &Metadata::default()).unwrap();
    assert_eq!(
        packed.warnings,
        ["shared library lib/libplugin.so has no DT_SONAME; nothing can depend on it"]
    );
    assert_eq!(
        requires(&metadata),
        BTreeMap::from([("libc.so.6", String::new())])
    );
}

#[test]
fn libstdcxx_versions_are_exact_capabilities() {
    let provider = payload(&[("lib/libstdc++.so.6.0.34", &libstdcxx())]);
    assert_eq!(
        provides(&derived(provider.path())),
        BTreeMap::from([
            ("elfver(libstdc++.so.6:CXXABI_1.3)", String::new()),
            ("elfver(libstdc++.so.6:GLIBCXX_3.4)", String::new()),
            ("elfver(libstdc++.so.6:GLIBCXX_3.4.21)", String::new()),
            ("libstdc++.so.6", String::new()),
        ])
    );

    let consumer = Elf::needing(&["libstdc++.so.6"]).with_needs(
        "libstdc++.so.6",
        &[
            ("GLIBCXX_3.4", 0),
            ("GLIBCXX_3.4.21", 0),
            ("GLIBCXX_3.4.32", WEAK),
        ],
    );
    let input = payload(&[("bin/a", &consumer)]);
    assert_eq!(
        requires(&derived(input.path())),
        BTreeMap::from([
            ("elfver(libstdc++.so.6:GLIBCXX_3.4)", String::new()),
            ("elfver(libstdc++.so.6:GLIBCXX_3.4.21)", String::new()),
            ("libstdc++.so.6", String::new()),
        ])
    );

    // A package with both needs nothing it defines itself.
    let input = payload(&[("bin/a", &consumer), ("lib/libstdc++.so.6", &libstdcxx())]);
    assert!(derived(input.path()).requires.is_empty());
}

#[test]
fn a_libstdcxx_provider_must_define_what_it_needs() {
    let consumer =
        Elf::needing(&["libstdc++.so.6"]).with_needs("libstdc++.so.6", &[("GLIBCXX_3.4.99", 0)]);
    let input = payload(&[("bin/a", &consumer), ("lib/libstdc++.so.6", &libstdcxx())]);
    let err = pack(&hello(), input.path(), &Metadata::default()).unwrap_err();
    assert!(
        format!("{err:#}").contains(
            "the package provides libstdc++.so.6 but needs \
             elfver(libstdc++.so.6:GLIBCXX_3.4.99), which it doesn't define"
        ),
        "{err:#}"
    );

    let input = payload(&[
        ("lib/libstdc++.so.6", &libstdcxx()),
        ("lib/other/libstdc++.so.6", &libstdcxx()),
    ]);
    let err = pack(&hello(), input.path(), &Metadata::default()).unwrap_err();
    assert!(
        format!("{err:#}").contains(
            "lib/libstdc++.so.6 and lib/other/libstdc++.so.6 both provide libstdc++.so.6"
        ),
        "{err:#}"
    );
}

#[test]
fn derived_edges_join_written_ones_and_written_ones_win() {
    let input = payload(&[("bin/hello", &glibc_program())]);
    let written = Metadata {
        requires: BTreeMap::from([
            ("libfoo.so.1".to_owned(), constraint(">= 1.2")),
            ("libc.so.6".to_owned(), Constraint::default()),
            ("sh".to_owned(), Constraint::default()),
        ]),
        provides: BTreeMap::from([("hello".to_owned(), None)]),
        ..Metadata::default()
    };
    let (_, metadata) = pack(&hello(), input.path(), &written).unwrap();
    assert_eq!(
        requires(&metadata),
        BTreeMap::from([
            ("libc.so.6", ">= 2.34".to_owned()),
            ("libfoo.so.1", ">= 1.2".to_owned()),
            ("sh", String::new()),
        ])
    );
    assert_eq!(
        provides(&metadata),
        BTreeMap::from([("hello", String::new())])
    );
}

#[test]
fn a_written_floor_on_a_derived_library_is_refused() {
    let input = payload(&[("bin/hello", &glibc_program())]);
    let out = tempfile::tempdir().unwrap();
    let written = Metadata {
        requires: BTreeMap::from([("libc.so.6".to_owned(), constraint(">= 2.40"))]),
        ..Metadata::default()
    };
    let err = ppk::pack_with(&hello(), &written, input.path(), out.path()).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "requires libc.so.6: the written floor '>= 2.40' is refused, and the payload needs \
         '>= 2.34' (bin/hello); a floor on libc.so.6 comes only from the payload's symbol versions"
    );
    assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);

    // Even where the payload doesn't need it.
    let input = payload(&[("bin/tool", &Elf::default())]);
    let written = Metadata {
        requires: BTreeMap::from([("libgcc_s.so.1".to_owned(), constraint(">= 4.3.0"))]),
        ..Metadata::default()
    };
    let err = pack(&hello(), input.path(), &written).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "requires libgcc_s.so.1: the written floor '>= 4.3.0' is refused, and the payload \
         doesn't need libgcc_s.so.1; a floor on libgcc_s.so.1 comes only from the payload's \
         symbol versions"
    );
}

#[test]
fn a_written_provide_of_a_floor_library_must_match_its_stamp() {
    let input = payload(&[("lib/libc.so.6", &Elf::so("libc.so.6"))]);
    let id = PackageId::new("org.gnu.glibc", "2.44-1", "x86_64").unwrap();
    let at = |version: &str| Metadata {
        provides: BTreeMap::from([(
            "libc.so.6".to_owned(),
            Some(Version::parse_relaxed(version).unwrap()),
        )]),
        ..Metadata::default()
    };
    let err = pack(&id, input.path(), &at("2.45-1")).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "provides libc.so.6: the written version '2.45-1' is refused, and the payload provides \
         it at the package's version '2.44-1'"
    );
    let (_, metadata) = pack(&id, input.path(), &at("2.44-1")).unwrap();
    assert_eq!(
        provides(&metadata),
        BTreeMap::from([("libc.so.6", "2.44-1".to_owned())])
    );
}

#[test]
fn the_same_payload_gives_the_same_manifest() {
    let objects = [
        ("bin/a", glibc_program()),
        ("lib/libstdc++.so.6", libstdcxx()),
        ("lib/libfoo.so.1", Elf::so("libfoo.so.1")),
        ("bin/b", Elf::needing(&["libz.so.1", "libc.so.6"])),
    ];
    let forward: Vec<(&str, &Elf)> = objects.iter().map(|(p, e)| (*p, e)).collect();
    let backward: Vec<(&str, &Elf)> = forward.iter().rev().copied().collect();
    let one = pack(&hello(), payload(&forward).path(), &Metadata::default()).unwrap();
    let two = pack(&hello(), payload(&backward).path(), &Metadata::default()).unwrap();
    assert_eq!(one.0.identity, two.0.identity);
    assert_eq!(one.1, two.1);
}

#[test]
fn big_endian_64_bit_objects_are_read() {
    let program = Elf {
        wide_big_endian: true,
        ..glibc_program()
    };
    let input = payload(&[("bin/hello", &program)]);
    assert_eq!(
        requires(&derived(input.path())),
        BTreeMap::from([
            ("libc.so.6", ">= 2.34".to_owned()),
            ("libfoo.so.1", String::new())
        ])
    );
}

#[test]
fn a_malformed_object_is_an_error_naming_it() {
    let program = Elf {
        miscount_needs: true,
        ..glibc_program()
    };
    let input = payload(&[("bin/hello", &program)]);
    let out = tempfile::tempdir().unwrap();
    let err = ppk::pack_with(&hello(), &Metadata::default(), input.path(), out.path()).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "deriving requirements and provides: bin/hello: version needs: version section and \
         dynamic section disagree"
    );
    assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);

    let mut truncated = glibc_program().bytes();
    truncated.truncate(80);
    fs::write(input.path().join("bin/hello"), truncated).unwrap();
    let err = ppk::pack_with(&hello(), &Metadata::default(), input.path(), out.path()).unwrap_err();
    assert!(format!("{err:#}").contains("bin/hello"), "{err:#}");
}
