//! Opens beneath the workspace root that follow no symlink (spec 3.1 §10 D4).
//!
//! A file tool checks a path against the root once and opens the canonical target that
//! check found. Every open here refuses a symlink on any component of that target, so a
//! link swapped in after the check fails the call instead of redirecting it. Linux opens
//! with `openat2` and `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`. Where `openat2` is
//! unavailable, and on every other Unix, each component is opened in turn with
//! `O_NOFOLLOW` relative to its parent's descriptor; macOS takes that walk because
//! kernels before macOS 11 ignore `O_NOFOLLOW_ANY` instead of rejecting it.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

/// Opens the workspace root as a directory. The root's own path is trusted; only what
/// lies beneath it is opened without following links.
pub(crate) fn open_root(root: &Path) -> io::Result<OwnedFd> {
    let path = c_string(root.as_os_str().as_bytes())?;
    // SAFETY: `path` is NUL-terminated and outlives the call.
    let fd =
        unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    owned(fd)
}

/// Opens `relative` beneath `dir` with `flags`, following no symlink on any component.
///
/// `relative` holds only normal components; an empty path opens `dir` again. A symlink
/// on the path fails the open with `ELOOP`.
pub(crate) fn open_beneath(
    dir: &OwnedFd,
    relative: &Path,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<OwnedFd> {
    if relative.as_os_str().is_empty() {
        return dir.try_clone();
    }
    #[cfg(target_os = "linux")]
    match openat2(dir, relative, flags, mode) {
        // No `openat2` (before Linux 5.6, or refused by a seccomp filter): walk instead.
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) => {}
        result => return result,
    }
    walk(dir, relative, flags, mode, false)
}

/// Opens the directory `relative` beneath `dir`, creating each missing directory, and
/// following no symlink on any component.
pub(crate) fn create_dirs_beneath(dir: &OwnedFd, relative: &Path) -> io::Result<OwnedFd> {
    if relative.as_os_str().is_empty() {
        return dir.try_clone();
    }
    walk(dir, relative, libc::O_RDONLY | libc::O_DIRECTORY, 0, true)
}

/// Opens the single entry `name` in `dir` with `flags | O_NOFOLLOW`.
pub(crate) fn open_entry(
    dir: &OwnedFd,
    name: &std::ffi::OsStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<File> {
    let name = c_string(name.as_bytes())?;
    openat(dir, &name, flags | libc::O_NOFOLLOW, mode).map(File::from)
}

/// Renames the entry `from` in `dir` over the entry `to` in the same directory.
pub(crate) fn rename_entry(
    dir: &OwnedFd,
    from: &std::ffi::OsStr,
    to: &std::ffi::OsStr,
) -> io::Result<()> {
    let (from, to) = (c_string(from.as_bytes())?, c_string(to.as_bytes())?);
    // SAFETY: both names are NUL-terminated and outlive the call; `dir` is open.
    let result =
        unsafe { libc::renameat(dir.as_raw_fd(), from.as_ptr(), dir.as_raw_fd(), to.as_ptr()) };
    if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Removes the entry `name` from `dir`.
pub(crate) fn remove_entry(dir: &OwnedFd, name: &std::ffi::OsStr) -> io::Result<()> {
    let name = c_string(name.as_bytes())?;
    // SAFETY: `name` is NUL-terminated and outlives the call; `dir` is open.
    let result = unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Whether `error` reports a symlink the open refused to follow.
pub(crate) fn is_symlink_refusal(error: &io::Error) -> bool {
    // `EXDEV` is `openat2`'s answer to a path that would leave the directory.
    matches!(error.raw_os_error(), Some(libc::ELOOP | libc::EXDEV))
}

/// Opens `relative` with `openat2`, resolving beneath `dir` and through no symlink.
#[cfg(target_os = "linux")]
fn openat2(
    dir: &OwnedFd,
    relative: &Path,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<OwnedFd> {
    let path = c_string(relative.as_os_str().as_bytes())?;
    // SAFETY: `open_how` is plain integers, for which zero is valid.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = u64::from((flags | libc::O_CLOEXEC).cast_unsigned());
    // `openat2` rejects a mode without `O_CREAT`.
    if flags & libc::O_CREAT != 0 {
        how.mode = u64::from(mode);
    }
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS;
    // SAFETY: `path` and `how` are valid for the call, and the size is `how`'s own.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dir.as_raw_fd(),
            path.as_ptr(),
            &raw const how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    owned(libc::c_int::try_from(fd).unwrap_or(-1))
}

/// Opens `relative` one component at a time, each with `O_NOFOLLOW` relative to its
/// parent's descriptor; with `create_dirs`, missing directories are created on the way.
fn walk(
    dir: &OwnedFd,
    relative: &Path,
    flags: libc::c_int,
    mode: libc::mode_t,
    create_dirs: bool,
) -> io::Result<OwnedFd> {
    let names = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => c_string(name.as_bytes()),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a path beneath the workspace root holds only plain names",
            )),
        })
        .collect::<io::Result<Vec<_>>>()?;
    let Some((last, parents)) = names.split_last() else {
        return dir.try_clone();
    };
    let mut current: Option<OwnedFd> = None;
    for name in parents {
        let parent = current.as_ref().unwrap_or(dir);
        current = Some(open_dir_entry(parent, name, create_dirs)?);
    }
    let parent = current.as_ref().unwrap_or(dir);
    if create_dirs {
        return open_dir_entry(parent, last, true);
    }
    openat(parent, last, flags | libc::O_NOFOLLOW, mode)
        .map_err(|error| symlink_as_loop(parent, last, error))
}

/// Opens the directory `name` in `dir` without following it, creating it when asked.
fn open_dir_entry(dir: &OwnedFd, name: &CString, create: bool) -> io::Result<OwnedFd> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
    match openat(dir, name, flags, 0) {
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            // SAFETY: `name` is NUL-terminated and outlives the call; `dir` is open.
            let made = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o777) };
            if made != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            openat(dir, name, flags, 0).map_err(|error| symlink_as_loop(dir, name, error))
        }
        result => result.map_err(|error| symlink_as_loop(dir, name, error)),
    }
}

/// macOS reports a symlink opened with `O_DIRECTORY | O_NOFOLLOW` as `ENOTDIR`, the
/// answer for an ordinary file too; this reports a symlink as `ELOOP` everywhere.
fn symlink_as_loop(dir: &OwnedFd, name: &CString, error: io::Error) -> io::Error {
    if error.raw_os_error() != Some(libc::ENOTDIR) {
        return error;
    }
    // SAFETY: `stat` is plain data written by `fstatat`; `name` outlives the call.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let found = unsafe {
        libc::fstatat(dir.as_raw_fd(), name.as_ptr(), &raw mut stat, libc::AT_SYMLINK_NOFOLLOW)
    };
    if found == 0 && (stat.st_mode & libc::S_IFMT) == libc::S_IFLNK {
        io::Error::from_raw_os_error(libc::ELOOP)
    } else {
        error
    }
}

fn openat(
    dir: &OwnedFd,
    name: &CString,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<OwnedFd> {
    // SAFETY: `name` is NUL-terminated and outlives the call; `dir` is open.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            libc::c_uint::from(mode),
        )
    };
    owned(fd)
}

fn owned(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned open by the kernel and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn c_string(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}
