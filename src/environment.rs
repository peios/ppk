//! Environments: the caller's own view of the filesystem, in a mount
//! namespace of its own, with a closure mounted at `/usr`.

use std::ffi::CString;
use std::fs;
use std::io;
use std::path::Path;
use std::ptr;

use anyhow::{Context, Result, bail};

/// Where an environment mounts its closure. Packages are laid out relative to
/// it, so a package's `bin/hello` is `/usr/bin/hello`.
pub const MOUNT_POINT: &str = "/usr";

/// Move this process into a new mount namespace in which `tree`, a closure's
/// tree, is mounted at [`MOUNT_POINT`]. Everything else is as it was. The
/// process's later children see the same view; nothing outside it changes.
///
/// The mount point must already exist: an environment replaces `/usr`, it
/// doesn't create one.
pub fn enter(tree: &Path) -> Result<()> {
    let mount_point = fs::metadata(MOUNT_POINT)
        .with_context(|| format!("{MOUNT_POINT}: cannot mount the closure here"))?;
    if !mount_point.is_dir() {
        bail!("{MOUNT_POINT}: not a directory, so the closure cannot be mounted here");
    }
    let tree_c = CString::new(tree.as_os_str().as_encoded_bytes())
        .with_context(|| format!("{}: path contains a NUL byte", tree.display()))?;
    let mount_point_c = CString::new(MOUNT_POINT).expect("the mount point has no NUL byte");

    // unshare(CLONE_NEWNS) fails with EINVAL in a multithreaded process; ppk
    // has one thread, and must keep it until this has run.
    // SAFETY: unshare takes no pointers.
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        return Err(io::Error::last_os_error()).context("cannot create a mount namespace");
    }

    // Stop mounts made here propagating back to the namespace this one was
    // copied from. A caller with the mount privilege gets a copy that shares
    // propagation with its parent, and this call is what isolates it. A caller
    // without it gets a copy whose mounts are already slaves of the parent's,
    // and may not change propagation, so EPERM means there is nothing to do.
    let root = c"/";
    // SAFETY: the target is a NUL-terminated string borrowed for the call;
    // the other pointers are null, which mount(2) accepts for this operation.
    let ret = unsafe {
        libc::mount(
            ptr::null(),
            root.as_ptr(),
            ptr::null(),
            libc::MS_REC | libc::MS_SLAVE,
            ptr::null(),
        )
    };
    if ret != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EPERM) {
            return Err(err).context("cannot stop mounts propagating to the parent namespace");
        }
    }

    // SAFETY: both paths are NUL-terminated strings borrowed for the call;
    // the filesystem type and data are null, which a bind mount ignores.
    let ret = unsafe {
        libc::mount(
            tree_c.as_ptr(),
            mount_point_c.as_ptr(),
            ptr::null(),
            libc::MS_BIND,
            ptr::null(),
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("{MOUNT_POINT}: cannot mount {}", tree.display()));
    }
    Ok(())
}
