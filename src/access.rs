//! Security descriptors for what ppk writes, on Peios.
//!
//! A hardlink shares its inode's security descriptor, so every closure that
//! links a stored file shares that file's descriptor, and the store has to
//! grant what any closure's user needs. For now that is read and execute for
//! Everyone, with full control for SYSTEM and Administrators; hardening comes
//! once the core design is proven. A tree that copies its blobs instead gives
//! each copy this descriptor of its own, so that it grants what a link would
//! have, but hardening the store will not reach what was copied before.
//!
//! A file or directory a package gives security metadata is different: its
//! [`Security`] replaces the store's descriptor with its own and sets its
//! signature attribute. Each stored file with metadata is a file of its own,
//! so the links to it share exactly that metadata.
//!
//! This module builds the one descriptor it needs by hand and applies it with
//! the raw `kacs_set_sd` system call, using only constants from `peios-uapi`.
//! That keeps ppk a single static binary that runs anywhere: off Peios the
//! call does not exist (ENOSYS) and is skipped. If ppk comes to need more and
//! more of what the `peios` crate (peios-rs) already provides, such as SDDL,
//! other descriptors, `kacs_open` or token queries, rewrite this on that crate
//! rather than growing a second copy of its machinery here.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use peios_uapi as uapi;

use crate::SecurityDescriptor;

/// The extended attribute a file's stored descriptor is in, which the kernel
/// reads (`file_sd_cache.c`) and, on Peios, lets only `kacs_set_sd` write.
#[cfg(not(test))]
const SD_XATTR: &CStr = c"security.peios.sd";

/// The extended attribute a file's signature is in, which the kernel reads
/// (`signing.h`).
#[cfg(not(test))]
const SIGNATURE_XATTR: &CStr = c"security.peios.sig";

/// Unit tests use `user.` attributes, which any process may set on its own
/// files, so that they stamp for real on any host. The rest is the same.
#[cfg(test)]
const SD_XATTR: &CStr = c"user.peios.sd";

#[cfg(test)]
const SIGNATURE_XATTR: &CStr = c"user.peios.sig";

/// Every self-relative security descriptor has revision 1 (`sd.h`).
const SD_REVISION: u8 = 1;

/// The revision of an ACL holding only the basic ACE types (MS-DTYP 2.4.5).
const ACL_REVISION: u8 = 2;

/// Size of an ACL header (`sd.h`).
const ACL_HEADER_BYTES: usize = 8;

/// Size of an ACE header and its access mask, before the SID (`sd.h`).
const ACE_FIXED_BYTES: usize = 8;

/// Reading a file or listing a directory, with its attributes and descriptor.
const READ: u32 = uapi::KACS_FILE_READ_DATA
    | uapi::KACS_FILE_READ_EA
    | uapi::KACS_FILE_READ_ATTRIBUTES
    | uapi::KACS_ACCESS_READ_CONTROL
    | uapi::KACS_ACCESS_SYNCHRONIZE;

/// Executing a file or traversing a directory (the same bit).
const EXECUTE: u32 = uapi::KACS_FILE_EXECUTE;

/// Every right a file or directory has.
const FULL_CONTROL: u32 = READ
    | EXECUTE
    | uapi::KACS_FILE_WRITE_DATA
    | uapi::KACS_FILE_APPEND_DATA
    | uapi::KACS_FILE_WRITE_EA
    | uapi::KACS_FILE_DELETE_CHILD
    | uapi::KACS_FILE_WRITE_ATTRIBUTES
    | uapi::KACS_ACCESS_DELETE
    | uapi::KACS_ACCESS_WRITE_DAC
    | uapi::KACS_ACCESS_WRITE_OWNER;

/// Everyone, S-1-1-0.
const EVERYONE: &[u8] = &[1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];

/// Local System, S-1-5-18.
const SYSTEM: &[u8] = &[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];

/// Builtin Administrators, S-1-5-32-544.
const ADMINISTRATORS: &[u8] = &[1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 0x02, 0, 0];

/// Whether a descriptor's grants pass to what is created beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inherit {
    /// For a file: nothing is ever created beneath it.
    No,
    /// For a directory: files and directories created in it inherit the grants.
    ToContents,
}

/// The descriptor ppk gives everything it writes: a protected DACL granting
/// Everyone read and execute, and SYSTEM and Administrators full control. It
/// has no owner, group or SACL, because only the DACL is ever set.
pub(crate) fn descriptor(inherit: Inherit) -> Vec<u8> {
    let flags = match inherit {
        Inherit::No => 0,
        Inherit::ToContents => {
            (uapi::KACS_ACE_FLAG_OBJECT_INHERIT | uapi::KACS_ACE_FLAG_CONTAINER_INHERIT) as u8
        }
    };
    let aces = [
        (EVERYONE, READ | EXECUTE),
        (SYSTEM, FULL_CONTROL),
        (ADMINISTRATORS, FULL_CONTROL),
    ];

    let mut acl = Vec::new();
    for (sid, mask) in aces {
        let size = ACE_FIXED_BYTES + sid.len();
        acl.push(uapi::KACS_ACE_TYPE_ACCESS_ALLOWED as u8);
        acl.push(flags);
        acl.extend_from_slice(&(size as u16).to_le_bytes());
        acl.extend_from_slice(&mask.to_le_bytes());
        acl.extend_from_slice(sid);
    }
    let acl_size = (ACL_HEADER_BYTES + acl.len()) as u16;

    let header_bytes = uapi::KACS_SD_HEADER_BYTES as usize;
    let control = (uapi::KACS_SD_SELF_RELATIVE
        | uapi::KACS_SD_DACL_PRESENT
        | uapi::KACS_SD_DACL_PROTECTED) as u16;
    let mut sd = Vec::with_capacity(header_bytes + acl_size as usize);
    sd.push(SD_REVISION);
    sd.push(0);
    sd.extend_from_slice(&control.to_le_bytes());
    for offset in [0u32, 0, 0, header_bytes as u32] {
        // Owner, group and SACL are absent; the DACL follows the header.
        sd.extend_from_slice(&offset.to_le_bytes());
    }
    sd.push(ACL_REVISION);
    sd.push(0);
    sd.extend_from_slice(&acl_size.to_le_bytes());
    sd.extend_from_slice(&(aces.len() as u16).to_le_bytes());
    sd.extend_from_slice(&0u16.to_le_bytes());
    sd.extend_from_slice(&acl);
    sd
}

/// Give the open file `file` (at `path`, for messages) the store descriptor.
pub(crate) fn protect_file(file: &impl AsRawFd, path: &Path) -> Result<()> {
    let sd = descriptor(Inherit::No);
    set_sd(
        file.as_raw_fd(),
        c"",
        uapi::KACS_SECINFO_DACL,
        &sd,
        libc::AT_EMPTY_PATH,
    )
    .with_context(|| format!("{}: cannot set its security descriptor", path.display()))
}

/// Give the directory `path` the store descriptor, inherited by everything
/// created in it afterwards.
pub(crate) fn protect_dir(path: &Path) -> Result<()> {
    let sd = descriptor(Inherit::ToContents);
    set_sd(
        libc::AT_FDCWD,
        &c_path(path)?,
        uapi::KACS_SECINFO_DACL,
        &sd,
        libc::AT_SYMLINK_NOFOLLOW,
    )
    .with_context(|| format!("{}: cannot set its security descriptor", path.display()))
}

/// Whether a file or directory carries all of its security metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) enum Stamp {
    /// It does, or it has none.
    Done,
    /// This system is not Peios and would not let ppk give it some of it,
    /// such as `security.*` attributes for an unprivileged process. Its
    /// manifest still records it.
    Skipped,
}

/// The security metadata a stored file or a directory carries: a descriptor
/// to replace the store's, and a signature for its `security.peios.sig`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Security<'a> {
    pub(crate) descriptor: Option<&'a SecurityDescriptor>,
    pub(crate) signature: Option<&'a [u8]>,
}

impl Security<'_> {
    /// Give `path`, a file or directory that ppk has just created and
    /// nothing else yet uses, this metadata.
    ///
    /// On Peios the descriptor is set with `kacs_set_sd` and the signature
    /// as an attribute, and any failure is an error. Elsewhere both are
    /// attributes, given where the system allows and otherwise skipped, as
    /// the store's own descriptor is.
    pub(crate) fn stamp(&self, path: &Path) -> Result<Stamp> {
        let mut stamp = Stamp::Done;
        if let Some(sd) = self.descriptor {
            stamp = worst(stamp, stamp_descriptor(path, sd)?);
        }
        if let Some(signature) = self.signature {
            stamp = worst(stamp, ensure_xattr(path, SIGNATURE_XATTR, signature)?);
        }
        Ok(stamp)
    }

    /// Check that `path`, a stored file that ppk did not just create,
    /// carries this metadata, and give it what it lacks, as [`stamp`] does.
    /// A different signature, or off Peios a different descriptor, is an
    /// error: the file is not what its name says.
    ///
    /// On Peios a file's descriptor cannot be read back as it was set, so it
    /// is left as it is: ppk set it when it created the file.
    ///
    /// [`stamp`]: Self::stamp
    pub(crate) fn ensure(&self, path: &Path) -> Result<Stamp> {
        let mut stamp = Stamp::Done;
        if let Some(sd) = self.descriptor
            && !descriptors_by_syscall()
        {
            stamp = worst(stamp, ensure_xattr(path, SD_XATTR, sd.as_bytes())?);
        }
        if let Some(signature) = self.signature {
            stamp = worst(stamp, ensure_xattr(path, SIGNATURE_XATTR, signature)?);
        }
        Ok(stamp)
    }
}

fn worst(a: Stamp, b: Stamp) -> Stamp {
    if a == Stamp::Skipped { a } else { b }
}

/// Whether descriptors are set with `kacs_set_sd`: on Peios, except in unit
/// tests, which set an attribute in its place.
fn descriptors_by_syscall() -> bool {
    !cfg!(test) && kacs_present()
}

fn stamp_descriptor(path: &Path, sd: &SecurityDescriptor) -> Result<Stamp> {
    if !descriptors_by_syscall() {
        return ensure_xattr(path, SD_XATTR, sd.as_bytes());
    }
    set_sd(
        libc::AT_FDCWD,
        &c_path(path)?,
        sd.security_info(),
        sd.as_bytes(),
        libc::AT_SYMLINK_NOFOLLOW,
    )
    .with_context(|| format!("{}: cannot set its security descriptor", path.display()))?;
    Ok(Stamp::Done)
}

/// Whether the kernel is Peios's: whether it has `kacs_set_sd`.
fn kacs_present() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        // SAFETY: every pointer is null and every length zero; the kernel
        // refuses a missing descriptor before it looks at anything else.
        let ret = unsafe {
            libc::syscall(
                libc::c_long::from(uapi::SYS_KACS_SET_SD as i32),
                -1,
                std::ptr::null::<libc::c_char>(),
                0u32,
                std::ptr::null::<u8>(),
                0u32,
                0i32,
            )
        };
        !(ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS))
    })
}

/// Give `path` the attribute `name` holding `value`, unless it holds it
/// already. Off Peios, a refusal to read or write it, for want of privilege
/// or of support for such attributes, skips it.
fn ensure_xattr(path: &Path, name: &CStr, value: &[u8]) -> Result<Stamp> {
    let c_path = c_path(path)?;
    let what = || format!("{}: {}", path.display(), name.to_string_lossy());
    let skip = |err: &io::Error| {
        !kacs_present()
            && matches!(
                err.raw_os_error(),
                Some(libc::EPERM | libc::EACCES | libc::EOPNOTSUPP)
            )
    };
    // One byte more than `value`, so that a longer value doesn't fit.
    let mut existing = vec![0u8; value.len() + 1];
    // SAFETY: both strings are NUL-terminated and `existing` is a buffer of
    // its length, all borrowed for the whole call.
    let len = unsafe {
        libc::lgetxattr(
            c_path.as_ptr(),
            name.as_ptr(),
            existing.as_mut_ptr().cast(),
            existing.len(),
        )
    };
    if len >= 0 {
        if existing[..len as usize] == *value {
            return Ok(Stamp::Done);
        }
        bail!("{}: holds something else", what());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ENODATA) => {}
        Some(libc::ERANGE) => bail!("{}: holds something else", what()),
        _ if skip(&err) => return Ok(Stamp::Skipped),
        _ => return Err(err).with_context(what),
    }
    // SAFETY: both strings are NUL-terminated and `value` is a buffer of its
    // length, all borrowed for the whole call; the kernel only reads them.
    let ret = unsafe {
        libc::lsetxattr(
            c_path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if ret == 0 {
        return Ok(Stamp::Done);
    }
    let err = io::Error::last_os_error();
    if skip(&err) {
        return Ok(Stamp::Skipped);
    }
    Err(err).with_context(|| format!("{}: cannot set it", what()))
}

fn c_path(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_encoded_bytes())
        .with_context(|| format!("{}: path contains a NUL byte", path.display()))
}

/// `kacs_set_sd(dirfd, path, info, sd)`. Succeeds without doing anything
/// where the system call does not exist, which is everywhere but Peios.
fn set_sd(dirfd: i32, path: &CStr, info: u32, sd: &[u8], flags: i32) -> io::Result<()> {
    // SAFETY: `path` is a NUL-terminated string and `sd` a buffer of `sd.len()`
    // bytes, both borrowed for the whole call; the kernel only reads them.
    let ret = unsafe {
        libc::syscall(
            libc::c_long::from(uapi::SYS_KACS_SET_SD as i32),
            dirfd,
            path.as_ptr(),
            info,
            sd.as_ptr(),
            sd.len() as u32,
            flags,
        )
    };
    if ret == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ENOSYS) {
        return Ok(());
    }
    Err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The bytes spelled out field by field, against the well-known values:
    /// FILE_GENERIC_READ | FILE_GENERIC_EXECUTE is 0x1200A9 and
    /// FILE_ALL_ACCESS is 0x1F01FF.
    #[test]
    fn the_descriptor_is_exactly_the_intended_bytes() {
        let expected = [
            // Revision 1; control SELF_RELATIVE | DACL_PROTECTED | DACL_PRESENT.
            "0100",
            "0490",
            // No owner, group or SACL; the DACL at offset 20.
            "00000000",
            "00000000",
            "00000000",
            "14000000",
            // ACL revision 2, 72 bytes, 3 ACEs.
            "0200",
            "4800",
            "0300",
            "0000",
            // Allow, 20 bytes, read and execute, Everyone (S-1-1-0).
            "00",
            "00",
            "1400",
            "a9001200",
            "010100000000000100000000",
            // Allow, 20 bytes, full control, SYSTEM (S-1-5-18).
            "00",
            "00",
            "1400",
            "ff011f00",
            "010100000000000512000000",
            // Allow, 24 bytes, full control, Administrators (S-1-5-32-544).
            "00",
            "00",
            "1800",
            "ff011f00",
            "01020000000000052000000020020000",
        ]
        .concat();
        assert_eq!(hex(&descriptor(Inherit::No)), expected);
    }

    #[test]
    fn a_directory_descriptor_differs_only_in_its_inheritance_flags() {
        let file = descriptor(Inherit::No);
        let dir = descriptor(Inherit::ToContents);
        assert_eq!(file.len(), dir.len());
        let ace_flag_offsets = [29, 49, 69];
        for (i, (f, d)) in file.iter().zip(&dir).enumerate() {
            if ace_flag_offsets.contains(&i) {
                assert_eq!(
                    (*f, *d),
                    (0x00, 0x03),
                    "OBJECT_INHERIT | CONTAINER_INHERIT at {i}"
                );
            } else {
                assert_eq!(f, d, "byte {i}");
            }
        }
    }
}
