//! Where the kernel's Unix socket lives when `<home>/kernel.sock` is too long for a socket
//! address (spec 3.3 G18): in a directory of its own under the user's temp directory, named
//! after the resolved home, so the kernel and every client derive the same path from one home.

use std::ffi::OsStr;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::ControlPlaneConfig;
use crate::error::{KernelError, Result};

/// The longest path a Unix socket address holds, in bytes: `sun_path` less its NUL, 103 on
/// macOS and 107 on Linux.
pub(crate) const MAX_LEN: usize =
    size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) - 1;

/// Whether `path` fits a Unix socket address.
pub(crate) fn fits(path: &Path) -> bool {
    path.as_os_str().len() <= MAX_LEN
}

/// Where the socket configured at `path` is served: `path` itself when it fits both as given
/// and with its directory resolved, else `adk-rust-<tag>/<file name>` under the user's temp
/// directory, the tag derived from the resolved path.
///
/// The decision and the tag both read the resolved path, so a home spelled through a
/// symlink, or relatively, reaches the same socket as its canonical spelling.
pub(crate) fn place(path: PathBuf) -> PathBuf {
    let absolute = std::path::absolute(&path).unwrap_or_else(|_| path.clone());
    // The nearest directory that exists, resolved, with the rest as given: a home that does
    // not exist yet resolves the same once it does.
    let mut rest = Vec::new();
    let mut at = absolute.as_path();
    let resolved = loop {
        let (Some(parent), Some(name)) = (at.parent(), at.file_name()) else {
            break absolute.clone();
        };
        rest.push(name);
        if let Ok(parent) = std::fs::canonicalize(parent) {
            break rest.iter().rev().fold(parent, |resolved, part| resolved.join(part));
        }
        at = parent;
    };
    if fits(&path) && fits(&resolved) {
        return path;
    }
    let name = resolved.file_name().unwrap_or(OsStr::new(ControlPlaneConfig::DEFAULT_SOCKET));
    user_temp_dir().join(format!("adk-rust-{}", crate::path_tag(&resolved))).join(name)
}

/// The user's own temp directory: `confstr(_CS_DARWIN_USER_TEMP_DIR)` on macOS, which a
/// launchd agent reads the same as a login shell; elsewhere `XDG_RUNTIME_DIR`, which a
/// systemd user unit and a login session share, else the system temp directory.
fn user_temp_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStringExt;

        let mut buf = vec![0_u8; 1024];
        // SAFETY: `buf` is writable for `buf.len()` bytes, the most `confstr` writes.
        let len = unsafe {
            libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len())
        };
        if (2..=buf.len()).contains(&len) {
            buf.truncate(len - 1);
            return PathBuf::from(std::ffi::OsString::from_vec(buf));
        }
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(dir) =
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|dir| dir.is_absolute())
    {
        return dir;
    }
    std::env::temp_dir()
}

/// Checks a short socket's directory `dir` before use: `Ok(false)` when there is none,
/// `Ok(true)` when it is a directory, not a symlink, owned by this user and closed to
/// everyone else.
///
/// # Errors
///
/// Returns [`KernelError::ControlPlane`] naming `dir` and what is wrong with it.
pub(crate) fn check_dir(dir: &Path) -> Result<bool> {
    let refuse = |what: String| KernelError::ControlPlane {
        reason: format!(
            "the kernel socket directory {} {what}, so no kernel socket is served or reached \
             there; remove it, then start the kernel again",
            dir.display()
        ),
    };
    let metadata = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(refuse(format!("cannot be read ({error})"))),
    };
    // SAFETY: `geteuid` takes nothing and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink() {
        Err(refuse("is a symlink".into()))
    } else if !metadata.is_dir() {
        Err(refuse("is not a directory".into()))
    } else if metadata.uid() != uid {
        Err(refuse(format!("belongs to another user (uid {})", metadata.uid())))
    } else if metadata.permissions().mode() & 0o077 != 0 {
        Err(refuse(format!(
            "is open to other users (mode {:o})",
            metadata.permissions().mode() & 0o777
        )))
    } else {
        Ok(true)
    }
}

/// Creates a short socket's directory `dir`, `0700`, or checks the one there with
/// [`check_dir`].
///
/// # Errors
///
/// Returns [`KernelError::ControlPlane`] when `dir` cannot be created or fails the check.
pub(crate) fn prepare_dir(dir: &Path) -> Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            check_dir(dir)?;
            Ok(())
        }
        Err(error) => Err(KernelError::ControlPlane {
            reason: format!(
                "could not create the kernel socket directory {}: {error}",
                dir.display()
            ),
        }),
    }
}
