//! Deriving what a package requires and provides from its ELF objects, and
//! merging that with what its packager wrote.
//!
//! These are old pekit's rules, which it took from peipkg's `pack` package:
//!
//! - An object's `DT_SONAME` is a capability the package provides, and each
//!   of its `DT_NEEDED` is one it requires. The whole soname is the name, so
//!   `libfoo.so.3` is never satisfied by `libfoo.so.4`. A soname the package
//!   provides itself is not required.
//! - For the libraries in [`SYMBOL_VERSION_FLOORS`], the highest symbol
//!   version an object needs is a floor on the requirement, and the
//!   package that provides one stamps the provide with its own version.
//! - For the libraries in [`SYMBOL_CAPABILITIES`], every version node is an
//!   exact capability, `elfver(<soname>:<node>)`.
//!
//! Files under `src/`, which hold source rather than installed objects, are
//! skipped, as are symlinks, which alias an object packed at its own path,
//! and files that aren't ELF.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use crate::metadata::check_capability;
use crate::{Constraint, Metadata, Version};

/// Libraries whose symbol versions are on the same scale as their
/// package's version, with the prefix of the tokens that are. An object
/// needing `GLIBC_2.34` of `libc.so.6` requires `libc.so.6 >= 2.34`, and the
/// package providing `libc.so.6` provides it at its own version, such as
/// `2.44-1`, which that requirement can be compared with.
///
/// A library belongs here only when that holds. libstdc++'s `GLIBCXX_3.4.x`
/// doesn't track gcc's version, so libstdc++ is in [`SYMBOL_CAPABILITIES`]
/// instead. The loader's soname is x86-64's.
///
/// The list is built in rather than given to `ppk pack`, so that a payload
/// gives the same manifest whoever packs it, and so that the refusal of a
/// hand-written floor applies to every package alike.
pub const SYMBOL_VERSION_FLOORS: &[(&str, &str)] = &[
    ("ld-linux-x86-64.so.2", "GLIBC_"),
    ("libanl.so.1", "GLIBC_"),
    ("libc.so.6", "GLIBC_"),
    ("libdl.so.2", "GLIBC_"),
    ("libgcc_s.so.1", "GCC_"),
    ("libm.so.6", "GLIBC_"),
    ("libmvec.so.1", "GLIBC_"),
    ("libnsl.so.1", "GLIBC_"),
    ("libpthread.so.0", "GLIBC_"),
    ("libresolv.so.2", "GLIBC_"),
    ("librt.so.1", "GLIBC_"),
    ("libutil.so.1", "GLIBC_"),
];

/// Libraries whose every version node is an exact capability: a package
/// providing one provides `elfver(<soname>:<node>)` for each node it
/// defines, and an object needing a node requires that capability. Weak
/// needs, which the loader treats as optional, aren't required.
pub const SYMBOL_CAPABILITIES: &[&str] = &["libstdc++.so.6"];

/// Where a package's source is kept, rather than objects it installs.
const SOURCE_DIR: &str = "src/";

/// The prefix of a symbol version's token, if `soname` is in
/// [`SYMBOL_VERSION_FLOORS`].
fn floor_prefix(soname: &str) -> Option<&'static str> {
    SYMBOL_VERSION_FLOORS
        .iter()
        .find(|(name, _)| *name == soname)
        .map(|(_, prefix)| *prefix)
}

/// What a payload's ELF objects say a package requires and provides.
#[derive(Debug, Default)]
pub(crate) struct Derived {
    pub requires: BTreeMap<String, Constraint>,
    pub provides: BTreeMap<String, Option<Version>>,
    /// For each floor, the payload path of the first object needing it.
    pub floor_sources: BTreeMap<String, String>,
    /// What derivation skipped, for the packager to see.
    pub warnings: Vec<String>,
}

/// Derive requirements and provides from `files`, each a payload path and
/// the file it is read from, for a package whose version is `version`.
/// Iterating `files` in a fixed order gives the same result every time.
pub(crate) fn derive<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a Path)>,
    version: &Version,
) -> Result<Derived> {
    let mut provided = BTreeSet::new();
    let mut needed = BTreeSet::new();
    // Each floor library's highest needed version, with where it was found.
    let mut floors: BTreeMap<String, (Version, String, String)> = BTreeMap::new();
    let mut exact = Exact::default();
    let mut warnings = Vec::new();

    for (rel, path) in files {
        if rel.starts_with(SOURCE_DIR) {
            continue;
        }
        let Some(object) = Object::read(path).with_context(|| rel.to_owned())? else {
            continue;
        };
        let mut scan = || -> Result<()> {
            let dynamic = object.dynamic()?;
            let soname = dynamic.soname.clone();
            match &soname {
                Some(soname) => {
                    provided.insert(soname.clone());
                }
                None if is_library_name(rel) => warnings.push(format!(
                    "shared library {rel} has no DT_SONAME; nothing can depend on it"
                )),
                None => {}
            }
            needed.extend(dynamic.needed.iter().cloned());

            let wants_needs = dynamic.needed.iter().any(|name| {
                floor_prefix(name).is_some() || SYMBOL_CAPABILITIES.contains(&name.as_str())
            });
            let needs = if wants_needs {
                object.version_needs(&dynamic)?
            } else {
                Vec::new()
            };
            for need in &needs {
                let Some(prefix) = floor_prefix(&need.file) else {
                    continue;
                };
                // Weak needs count: the old rules took every token.
                let tokens = need.versions.iter().map(|(name, _)| name.as_str());
                if let Some((floor, spelling)) = highest_symbol_version(prefix, tokens) {
                    let higher = floors
                        .get(&need.file)
                        .is_none_or(|(current, _, _)| *current < floor);
                    if higher {
                        floors.insert(need.file.clone(), (floor, spelling, rel.to_owned()));
                    }
                }
            }
            exact.scan(&object, &dynamic, soname.as_deref(), &needs, rel)
        };
        scan().with_context(|| rel.to_owned())?;
    }

    let mut derived = Derived {
        warnings,
        ..Derived::default()
    };
    for name in &provided {
        if let Err(err) = check_capability(name) {
            derived
                .warnings
                .push(format!("not providing soname {name:?}: {err}"));
            continue;
        }
        let stamp = floor_prefix(name).map(|_| version.clone());
        derived.provides.insert(name.clone(), stamp);
    }
    for name in needed.difference(&provided) {
        if let Err(err) = check_capability(name) {
            derived
                .warnings
                .push(format!("not requiring soname {name:?}: {err}"));
            continue;
        }
        let constraint = match floors.get(name) {
            Some((_, spelling, source)) => {
                derived.floor_sources.insert(name.clone(), source.clone());
                Constraint::parse(&format!(">= {spelling}"))?
            }
            None => Constraint::default(),
        };
        derived.requires.insert(name.clone(), constraint);
    }
    exact.finish(&provided, &mut derived)?;
    Ok(derived)
}

/// Merge `derived` into what the packager wrote, `written`.
///
/// Derived requirements and provides join the written ones. Where both name
/// one, the written entry wins, as it did in old pekit, with two
/// exceptions for the libraries in [`SYMBOL_VERSION_FLOORS`], whose floors
/// and stamps come only from the payload:
///
/// - A written requirement on one is refused unless it is for any version.
///   A floor written by hand tends to record what a package was built
///   against, not what it needs, which ties it to a newer library than it
///   must have. A written requirement for any version adds nothing, and the
///   derived floor stands.
/// - A written provide of one at a version other than the stamp is refused.
pub(crate) fn merge(written: &Metadata, derived: Derived) -> Result<Metadata> {
    let mut merged = written.clone();
    for (name, constraint) in &written.requires {
        if floor_prefix(name).is_none() || constraint.is_any() {
            continue;
        }
        let found = match (derived.requires.get(name), derived.floor_sources.get(name)) {
            (Some(floor), Some(source)) => format!("the payload needs '{floor}' ({source})"),
            (Some(_), None) => format!("the payload needs no particular version of {name}"),
            (None, _) => format!("the payload doesn't need {name}"),
        };
        bail!(
            "requires {name}: the written floor '{constraint}' is refused, and {found}; \
             a floor on {name} comes only from the payload's symbol versions"
        );
    }
    for (name, version) in &written.provides {
        let (Some(_), Some(Some(stamp))) = (floor_prefix(name), derived.provides.get(name)) else {
            continue;
        };
        if let Some(version) = version
            && version.as_str() != stamp.as_str()
        {
            bail!(
                "provides {name}: the written version '{version}' is refused, and the \
                 payload provides it at the package's version '{stamp}'"
            );
        }
    }
    for (name, constraint) in derived.requires {
        let entry = merged.requires.entry(name).or_default();
        if entry.is_any() {
            *entry = constraint;
        }
    }
    for (name, version) in derived.provides {
        let entry = merged.provides.entry(name).or_default();
        if entry.is_none() {
            *entry = version;
        }
    }
    Ok(merged)
}

/// Whether a payload path is named as a shared library is, `lib*.so` or
/// `lib*.so.N[.N...]`, as old pekit's `(^|/)lib[^/]+\.so(\.\d+)*$` matched.
fn is_library_name(rel: &str) -> bool {
    let mut name = rel.rsplit('/').next().unwrap_or(rel);
    // Strip the trailing `.N` groups; no group can itself end in `.so`.
    while let Some((head, tail)) = name.rsplit_once('.')
        && !tail.is_empty()
        && tail.bytes().all(|c| c.is_ascii_digit())
    {
        name = head;
    }
    name.len() > "lib.so".len() && name.starts_with("lib") && name.ends_with(".so")
}

/// The highest version among `tokens` that start with `prefix`, with the
/// prefix removed, as a version and as it is spelled. A token whose rest
/// isn't a version starting with a digit, such as `GLIBC_PRIVATE`, is not a
/// floor and is ignored.
fn highest_symbol_version<'a>(
    prefix: &str,
    tokens: impl Iterator<Item = &'a str>,
) -> Option<(Version, String)> {
    let mut best: Option<(Version, String)> = None;
    for token in tokens {
        let Some(tail) = token.strip_prefix(prefix) else {
            continue;
        };
        if !tail.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let Ok(version) = Version::parse_relaxed(tail) else {
            continue;
        };
        if best.as_ref().is_none_or(|(current, _)| *current < version) {
            best = Some((version, tail.to_owned()));
        }
    }
    best
}

/// The capability `elfver(<soname>:<token>)`, if it can be written: the
/// soname is a capability without `(`, `)`, `:` or `/`, and the token is
/// letters, digits and `_ . + -`.
fn version_capability(soname: &str, token: &str) -> Result<String> {
    let name = format!("elfver({soname}:{token})");
    let token_ok = !token.is_empty()
        && token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c));
    if check_capability(soname).is_err() || soname.contains(['(', ')', ':', '/']) || !token_ok {
        bail!("ELF version capability {name:?} can't be written");
    }
    check_capability(&name)?;
    Ok(name)
}

/// The exact capabilities of the libraries in [`SYMBOL_CAPABILITIES`].
#[derive(Default)]
struct Exact {
    /// Each such soname the payload provides, with the object providing it.
    providers: BTreeMap<String, String>,
    provided: BTreeSet<String>,
    /// Each capability needed, with the soname it is a node of.
    needed: BTreeMap<String, String>,
    errors: Vec<String>,
}

impl Exact {
    fn scan(
        &mut self,
        object: &Object,
        dynamic: &Dynamic,
        soname: Option<&str>,
        needs: &[Need],
        rel: &str,
    ) -> Result<()> {
        if let Some(soname) = soname.filter(|soname| SYMBOL_CAPABILITIES.contains(soname)) {
            if let Some(previous) = self.providers.insert(soname.to_owned(), rel.to_owned()) {
                self.errors.push(format!(
                    "{previous} and {rel} both provide {soname}, whose versions are capabilities"
                ));
            }
            for (name, flags) in object.version_definitions(dynamic)? {
                if flags & VER_FLG_BASE != 0 {
                    continue;
                }
                match version_capability(soname, &name) {
                    Ok(capability) => {
                        self.provided.insert(capability);
                    }
                    Err(err) => self.errors.push(format!("{rel}: {err}")),
                }
            }
        }
        for need in needs {
            if !SYMBOL_CAPABILITIES.contains(&need.file.as_str()) {
                continue;
            }
            for (name, flags) in &need.versions {
                if flags & VER_FLG_WEAK != 0 {
                    continue;
                }
                match version_capability(&need.file, name) {
                    Ok(capability) => {
                        self.needed.insert(capability, need.file.clone());
                    }
                    Err(err) => self.errors.push(format!("{rel}: {err}")),
                }
            }
        }
        Ok(())
    }

    /// Add the capabilities to `derived`. A package that provides such a
    /// library and needs a node of it that it doesn't define can't be
    /// satisfied by anything, and is an error, as is anything [`Exact::scan`]
    /// couldn't represent.
    fn finish(mut self, provided: &BTreeSet<String>, derived: &mut Derived) -> Result<()> {
        for (capability, soname) in &self.needed {
            if self.provided.contains(capability) {
                continue;
            }
            if provided.contains(soname) {
                self.errors.push(format!(
                    "the package provides {soname} but needs {capability}, which it doesn't define"
                ));
                continue;
            }
            derived
                .requires
                .insert(capability.clone(), Constraint::default());
        }
        for capability in self.provided {
            derived.provides.insert(capability, None);
        }
        if !self.errors.is_empty() {
            bail!("{}", self.errors.join("; "));
        }
        Ok(())
    }
}

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const SHT_STRTAB: u32 = 3;
const SHT_DYNAMIC: u32 = 6;
const SHT_GNU_VERDEF: u32 = 0x6fff_fffd;
const SHT_GNU_VERNEED: u32 = 0x6fff_fffe;
const DT_NEEDED: u64 = 1;
const DT_SONAME: u64 = 14;
const DT_VERDEF: u64 = 0x6fff_fffc;
const DT_VERDEFNUM: u64 = 0x6fff_fffd;
const DT_VERNEED: u64 = 0x6fff_fffe;
const DT_VERNEEDNUM: u64 = 0x6fff_ffff;
const VER_FLG_BASE: u16 = 1;
const VER_FLG_WEAK: u16 = 2;

/// An ELF object, read through its section headers as old pekit's reader
/// read it. Anything it needs that is out of bounds or inconsistent is an
/// error, so that a requirement is never silently missed.
struct Object {
    data: Vec<u8>,
    wide: bool,
    little: bool,
    sections: Vec<Section>,
}

#[derive(Clone, Copy)]
struct Section {
    kind: u32,
    addr: u64,
    offset: u64,
    size: u64,
    link: u32,
    info: u32,
}

/// What an object's dynamic section says.
struct Dynamic {
    soname: Option<String>,
    needed: Vec<String>,
    /// Every entry, as tag and value.
    entries: Vec<(u64, u64)>,
}

/// The versions an object needs of one library, each with its flags.
struct Need {
    file: String,
    versions: Vec<(String, u16)>,
}

impl Object {
    /// Read `path`, or nothing if it isn't ELF.
    fn read(path: &Path) -> Result<Option<Self>> {
        let mut file = File::open(path)?;
        let mut magic = [0; 4];
        let mut got = 0;
        while got < magic.len() {
            match file.read(&mut magic[got..])? {
                0 => return Ok(None),
                n => got += n,
            }
        }
        if &magic != ELF_MAGIC {
            return Ok(None);
        }
        let mut data = magic.to_vec();
        file.read_to_end(&mut data)?;
        Self::parse(data).map(Some)
    }

    fn parse(data: Vec<u8>) -> Result<Self> {
        let wide = match data.get(4) {
            Some(1) => false,
            Some(2) => true,
            _ => bail!("not a 32-bit or 64-bit ELF object"),
        };
        let little = match data.get(5) {
            Some(1) => true,
            Some(2) => false,
            _ => bail!("ELF object of unknown byte order"),
        };
        let mut object = Self {
            data,
            wide,
            little,
            sections: Vec::new(),
        };
        let (shoff, shentsize, shnum) = if wide {
            (object.u64(0x28)?, object.u16(0x3a)?, object.u16(0x3c)?)
        } else {
            (
                u64::from(object.u32(0x20)?),
                object.u16(0x2e)?,
                object.u16(0x30)?,
            )
        };
        if shoff == 0 {
            return Ok(object);
        }
        let entsize = if wide { 64 } else { 40 };
        if u64::from(shentsize) != entsize {
            bail!("section header size {shentsize} is not {entsize}");
        }
        let header = |object: &Self, index: u64| -> Result<Section> {
            let at = index
                .checked_mul(entsize)
                .and_then(|offset| offset.checked_add(shoff))
                .ok_or_else(|| anyhow!("section header {index} is out of bounds"))?;
            Ok(if object.wide {
                Section {
                    kind: object.u32(at + 4)?,
                    addr: object.u64(at + 16)?,
                    offset: object.u64(at + 24)?,
                    size: object.u64(at + 32)?,
                    link: object.u32(at + 40)?,
                    info: object.u32(at + 44)?,
                }
            } else {
                Section {
                    kind: object.u32(at + 4)?,
                    addr: u64::from(object.u32(at + 12)?),
                    offset: u64::from(object.u32(at + 16)?),
                    size: u64::from(object.u32(at + 20)?),
                    link: object.u32(at + 24)?,
                    info: object.u32(at + 28)?,
                }
            })
        };
        // With 0xff00 or more sections, the count is in the first header.
        let count = match shnum {
            0 => header(&object, 0)?.size,
            n => u64::from(n),
        };
        if count > object.data.len() as u64 / entsize {
            bail!("section count {count} is out of bounds");
        }
        let sections = (0..count)
            .map(|index| header(&object, index))
            .collect::<Result<_>>()?;
        object.sections = sections;
        Ok(object)
    }

    fn bytes(&self, at: u64, len: u64) -> Result<&[u8]> {
        let end = at.checked_add(len);
        match end {
            Some(end) if end <= self.data.len() as u64 => Ok(&self.data[at as usize..end as usize]),
            _ => bail!("{len} bytes at offset {at} are out of bounds"),
        }
    }

    fn u16(&self, at: u64) -> Result<u16> {
        let b: [u8; 2] = self.bytes(at, 2)?.try_into()?;
        Ok(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&self, at: u64) -> Result<u32> {
        let b: [u8; 4] = self.bytes(at, 4)?.try_into()?;
        Ok(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn u64(&self, at: u64) -> Result<u64> {
        let b: [u8; 8] = self.bytes(at, 8)?.try_into()?;
        Ok(if self.little {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }

    /// The first section of type `kind`, as old pekit's reader took it.
    fn section(&self, kind: u32) -> Option<Section> {
        self.sections.iter().copied().find(|s| s.kind == kind)
    }

    /// The string table a section links to, which must be one.
    fn strings(&self, section: Section) -> Result<&[u8]> {
        let table = self
            .sections
            .get(section.link as usize)
            .filter(|table| table.kind == SHT_STRTAB)
            .ok_or_else(|| anyhow!("section links to no string table"))?;
        self.bytes(table.offset, table.size)
    }

    /// What the dynamic section says, which is nothing if there isn't one.
    fn dynamic(&self) -> Result<Dynamic> {
        let mut dynamic = Dynamic {
            soname: None,
            needed: Vec::new(),
            entries: Vec::new(),
        };
        let Some(section) = self.section(SHT_DYNAMIC) else {
            return Ok(dynamic);
        };
        let strings = self.strings(section).context("dynamic section")?;
        let entsize = if self.wide { 16 } else { 8 };
        let data = self.bytes(section.offset, section.size)?;
        for index in 0..data.len() as u64 / entsize {
            let at = section.offset + index * entsize;
            let (tag, value) = if self.wide {
                (self.u64(at)?, self.u64(at + 8)?)
            } else {
                (u64::from(self.u32(at)?), u64::from(self.u32(at + 4)?))
            };
            dynamic.entries.push((tag, value));
            match tag {
                DT_NEEDED => dynamic
                    .needed
                    .push(string_at(strings, value).context("DT_NEEDED")?),
                DT_SONAME if dynamic.soname.is_none() => {
                    dynamic.soname = Some(string_at(strings, value).context("DT_SONAME")?);
                }
                _ => {}
            }
        }
        Ok(dynamic)
    }

    /// The versions the object needs, by library.
    fn version_needs(&self, dynamic: &Dynamic) -> Result<Vec<Need>> {
        let mut needs = Vec::new();
        self.version_table(dynamic, SHT_GNU_VERNEED, |file, versions| {
            needs.push(Need {
                file: file.unwrap_or_default(),
                versions,
            });
        })
        .context("version needs")?;
        Ok(needs)
    }

    /// The versions the object defines, each with its flags.
    fn version_definitions(&self, dynamic: &Dynamic) -> Result<Vec<(String, u16)>> {
        let mut definitions = Vec::new();
        self.version_table(dynamic, SHT_GNU_VERDEF, |_, mut names| {
            // A definition's name is its first auxiliary entry; the rest
            // name its parents.
            let (name, flags) = names.swap_remove(0);
            definitions.push((name, flags));
        })
        .context("version definitions")?;
        Ok(definitions)
    }

    /// Walk the GNU version table `kind`, checking it as old pekit did,
    /// because its reader tolerated truncated chains. For each entry, `each`
    /// gets the library a need names, and the entry's names with flags: a
    /// need's auxiliary entries have their own flags, and a definition's
    /// carry the definition's.
    fn version_table(
        &self,
        dynamic: &Dynamic,
        kind: u32,
        mut each: impl FnMut(Option<String>, Vec<(String, u16)>),
    ) -> Result<()> {
        let need = kind == SHT_GNU_VERNEED;
        let (header_size, aux_size) = if need { (16, 16) } else { (20, 8) };
        let (count_at, aux_at, next_at, name_at) =
            if need { (2, 8, 12, 8) } else { (6, 12, 16, 0) };
        let (pointer_tag, count_tag) = if need {
            (DT_VERNEED, DT_VERNEEDNUM)
        } else {
            (DT_VERDEF, DT_VERDEFNUM)
        };
        let tagged = |tag| -> Vec<u64> {
            dynamic
                .entries
                .iter()
                .filter(|(t, _)| *t == tag)
                .map(|(_, v)| *v)
                .collect()
        };
        let (pointers, counts) = (tagged(pointer_tag), tagged(count_tag));
        let Some(section) = self.section(kind) else {
            if !pointers.is_empty() || !counts.is_empty() {
                bail!("dynamic section names a version table there is no section for");
            }
            return Ok(());
        };
        if pointers != [section.addr] || counts != [u64::from(section.info)] {
            bail!("version section and dynamic section disagree");
        }
        let data = self.bytes(section.offset, section.size)?;
        let strings = self.strings(section)?;
        let size = data.len() as u64;
        if section.info == 0 || u64::from(section.info) > size / header_size {
            bail!("invalid entry count");
        }
        let base = section.offset;
        let mut offset = 0;
        for entry in 0..section.info {
            if offset + header_size > size {
                bail!("truncated header");
            }
            if self.u16(base + offset)? != 1 {
                bail!("unsupported version");
            }
            let count = u64::from(self.u16(base + offset + count_at)?);
            let aux = u64::from(self.u32(base + offset + aux_at)?);
            let next = u64::from(self.u32(base + offset + next_at)?);
            let limit = if entry + 1 < section.info {
                if next < header_size || offset + next + header_size > size {
                    bail!("truncated header chain");
                }
                offset + next
            } else if next != 0 {
                bail!("entry count does not end the chain");
            } else {
                size
            };
            if count == 0 || aux < header_size || count > size / aux_size {
                bail!("invalid auxiliary count or offset");
            }
            let file = if need {
                let name = self.u32(base + offset + 4)?;
                Some(nonempty_string_at(strings, name.into()).context("library name")?)
            } else {
                None
            };
            let flags = if need {
                0
            } else {
                self.u16(base + offset + 2)?
            };
            let mut names = Vec::new();
            let mut cursor = offset + aux;
            for i in 0..count {
                if cursor + aux_size > limit {
                    bail!("truncated auxiliary entry");
                }
                let name = self.u32(base + cursor + name_at)?;
                let name = nonempty_string_at(strings, name.into()).context("version name")?;
                let flags = if need {
                    self.u16(base + cursor + 4)?
                } else {
                    flags
                };
                names.push((name, flags));
                let step = u64::from(self.u32(base + cursor + aux_size - 4)?);
                if i + 1 < count {
                    if step < aux_size {
                        bail!("truncated auxiliary chain");
                    }
                    cursor += step;
                } else if step != 0 {
                    bail!("auxiliary count does not end the chain");
                }
            }
            each(file, names);
            offset += next;
        }
        Ok(())
    }
}

/// The NUL-terminated UTF-8 string at `offset` in a string table.
fn string_at(strings: &[u8], offset: u64) -> Result<String> {
    let start = usize::try_from(offset)
        .ok()
        .filter(|start| *start < strings.len())
        .ok_or_else(|| anyhow!("string offset {offset} is out of bounds"))?;
    let len = strings[start..]
        .iter()
        .position(|&c| c == 0)
        .ok_or_else(|| anyhow!("string at offset {offset} is not terminated"))?;
    String::from_utf8(strings[start..start + len].to_vec())
        .map_err(|_| anyhow!("string at offset {offset} is not UTF-8"))
}

fn nonempty_string_at(strings: &[u8], offset: u64) -> Result<String> {
    let s = string_at(strings, offset)?;
    if s.is_empty() {
        bail!("empty string at offset {offset}");
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_names() {
        for name in [
            "lib/libc.so.6",
            "lib/libz.so",
            "lib/libstdc++.so.6.0.34",
            "libfoo.so.1.2.3",
        ] {
            assert!(is_library_name(name), "{name}");
        }
        for name in [
            "lib/libz.so.1.debug",
            "lib/libz.a",
            "bin/ls",
            "lib/lib.so",
            "lib/libz.so.x",
            "lib/python3/foo.so",
        ] {
            assert!(!is_library_name(name), "{name}");
        }
    }

    #[test]
    fn highest_symbol_version_ignores_other_tokens() {
        let tokens = [
            "GLIBC_2.2.5",
            "GLIBC_2.34",
            "GLIBC_PRIVATE",
            "GLIBC_2.4",
            "GCC_3.0",
        ];
        let (_, spelling) = highest_symbol_version("GLIBC_", tokens.into_iter()).unwrap();
        assert_eq!(spelling, "2.34");
        assert!(highest_symbol_version("GLIBC_", ["GLIBC_PRIVATE"].into_iter()).is_none());
    }

    #[test]
    fn version_capability_rules() {
        assert_eq!(
            version_capability("libstdc++.so.6", "GLIBCXX_3.4.21").unwrap(),
            "elfver(libstdc++.so.6:GLIBCXX_3.4.21)"
        );
        assert!(version_capability("libstdc++.so.6", "A:B").is_err());
        assert!(version_capability("lib(x).so", "V1").is_err());
    }
}
