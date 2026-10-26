//! Opens beneath the workspace root that follow no link swapped in after the check
//! (spec 3.1 §10 D4, spec 3.2 §10 W6).
//!
//! A file tool checks a path against the root once and opens the canonical target that
//! check found. Every open here refuses a link that appears on that target afterwards,
//! so the link fails the call instead of redirecting it.
//!
//! On Unix no component of the target is followed if it is a symlink. Linux opens with
//! `openat2` and `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`. Where `openat2` is unavailable,
//! and on every other Unix, each component is opened in turn with `O_NOFOLLOW` relative
//! to its parent's descriptor; macOS takes that walk because kernels before macOS 11
//! ignore `O_NOFOLLOW_ANY` instead of rejecting it.
//!
//! On Windows the open is verified on the handle it returns. The target is opened with
//! `FILE_FLAG_OPEN_REPARSE_POINT`, and the handle is refused when it is a name-surrogate
//! reparse point (a symlink or a junction) — an in-place one, such as a cloud placeholder
//! or a WOF-compressed file, is not — and when its final path is anything but the checked
//! target beneath the root's own final path. Each check reads the object already opened,
//! so a link swapped in afterwards cannot redirect the call, and one swapped in during
//! the open fails it.

#[cfg(unix)]
pub(crate) use unix::{
    create_dirs_beneath, is_symlink_refusal, open_beneath, open_entry, open_root, remove_entry,
    rename_entry,
};
#[cfg(windows)]
pub(crate) use windows::{
    is_symlink_refusal, read_beneath, remove_beneath, replace_beneath, write_beneath,
};

/// Whether a reparse tag names another entity in the system — a symlink or a junction —
/// rather than keeping the file's own data elsewhere (`IsReparseTagNameSurrogate`).
#[cfg(any(windows, test))]
pub(crate) fn is_name_surrogate(tag: u32) -> bool {
    tag & 0x2000_0000 != 0
}

#[cfg(unix)]
mod unix {
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
        let fd = unsafe {
            libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)
        };
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
}

#[cfg(windows)]
mod windows {
    use std::ffi::{OsStr, OsString};
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read as _, Seek as _, Write as _};
    use std::os::windows::ffi::OsStringExt as _;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use std::path::{Component, Path, PathBuf};

    use windows_sys::Win32::Foundation::{
        ERROR_STOPPED_ON_SYMLINK, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
        FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileAttributeTagInfo, FileDispositionInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, GetFinalPathNameByHandleW, ReOpenFile,
        SetFileInformationByHandle, VOLUME_NAME_DOS,
    };

    use crate::workspace::{Precondition, StaleFile};

    /// Reads the file `relative` beneath `root`, or returns its link count when it has
    /// more than one hard link, since another link to it can lie outside the root
    /// (spec 3.2 §10 W6).
    pub(crate) fn read_beneath(root: &Path, relative: &Path) -> io::Result<Result<Vec<u8>, u64>> {
        let path = final_path(&open_dir(root, 0)?)?.join(plain(relative)?);
        let file =
            OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(&path)?;
        let opened = verify(&file, &path)?;
        if opened.info.nNumberOfLinks > 1 {
            return Ok(Err(u64::from(opened.info.nNumberOfLinks)));
        }
        let mut file = follow_in_place(file, &opened, GENERIC_READ)?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        Ok(Ok(contents))
    }

    /// Opens the root, creating it when missing, and each directory of `parent` beneath
    /// it without following a link on it, creating the missing ones when `create` is set.
    /// Returns the parent's final path and the handles, which are held without delete
    /// sharing so none can be renamed or replaced while they are open.
    fn hold_dirs(root: &Path, parent: &Path, create: bool) -> io::Result<(PathBuf, Vec<File>)> {
        let root_dir = match open_dir(root, 0) {
            // The root is trusted, so a workspace whose root does not exist yet creates it.
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir_all(root)?;
                open_dir(root, 0)?
            }
            result => result?,
        };
        let mut dir = final_path(&root_dir)?;
        let mut held = vec![root_dir];
        for component in plain(parent)?.components() {
            dir.push(component);
            held.push(if create {
                open_or_create_dir(&dir)?
            } else {
                let opened = open_dir(&dir, FILE_FLAG_OPEN_REPARSE_POINT)?;
                verify(&opened, &dir)?;
                opened
            });
        }
        Ok((dir, held))
    }

    /// Reads the file a verified handle holds, through a reopen for an in-place reparse
    /// point, and checks it against `precondition`.
    fn check_handle(file: &File, opened: &Opened, precondition: &Precondition) -> io::Result<()> {
        if matches!(precondition, Precondition::Any) {
            return Ok(());
        }
        let mut reader = follow_in_place(file.try_clone()?, opened, GENERIC_READ)?;
        reader.rewind()?;
        let mut current = Vec::new();
        reader.read_to_end(&mut current)?;
        precondition.check(Some(&current))
    }

    /// Opens the existing file `path` with `access` without following a link on it, and
    /// verifies the handle; `None` when it does not exist.
    fn open_existing(path: &Path, access: u32) -> io::Result<Option<(File, Opened)>> {
        let opened = OpenOptions::new()
            .access_mode(access | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path);
        let file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let checked = verify(&file, path)?;
        Ok(Some((file, checked)))
    }

    /// Writes `contents` to the entry `name` of the directory `relative` beneath `root`,
    /// creating the file and its missing directories, or truncating it, once
    /// `precondition` holds (spec 3.2 §10 W6, spec 3.1 §7.26).
    ///
    /// Each directory on the path is opened in turn without following a link on it,
    /// created when missing, and held open without delete sharing until the write ends,
    /// so none can be renamed or replaced by a link meanwhile. The file is opened without
    /// truncation, its content checked against the precondition through the same handle,
    /// and truncated only once every check has passed; a file with more than one hard
    /// link is replaced instead, so its other links keep their content.
    pub(crate) fn write_beneath(
        root: &Path,
        relative: &Path,
        name: &OsStr,
        contents: &[u8],
        precondition: &Precondition,
    ) -> io::Result<()> {
        let (dir, held) = hold_dirs(root, relative.parent().unwrap_or(Path::new("")), true)?;
        let path = dir.join(name);
        let checking = matches!(precondition, Precondition::Sha256(_));
        let (file, created) = open_for_write(&path, checking)?;
        let opened = match verify(&file, &path) {
            Ok(opened) => opened,
            Err(error) => {
                // A file this write created goes again, through its own handle.
                if created {
                    let _ = delete_on_close(&file);
                }
                return Err(error);
            }
        };
        match precondition {
            Precondition::Absent if !created => return Err(StaleFile::Exists.into()),
            Precondition::Sha256(_) if created => {
                let _ = delete_on_close(&file);
                return Err(StaleFile::Missing.into());
            }
            Precondition::Sha256(_) => check_handle(&file, &opened, precondition)?,
            Precondition::Any | Precondition::Absent => {}
        }
        if opened.info.nNumberOfLinks > 1 {
            let permissions = file.metadata()?.permissions();
            drop(file);
            return replace_entry(&dir, name, Some(permissions), contents);
        }
        let mut file = follow_in_place(file, &opened, GENERIC_WRITE)?;
        file.rewind()?;
        file.set_len(0)?;
        file.write_all(contents)?;
        file.flush()?;
        drop(held);
        Ok(())
    }

    /// Writes `contents` to a temporary sibling of the entry `name` of the directory
    /// `relative` beneath `root` and renames it over `name`, once `precondition` holds
    /// (spec 3.1 §7.26 `apply_patch`). The directories are held as
    /// [`write_beneath`] holds them.
    pub(crate) fn replace_beneath(
        root: &Path,
        relative: &Path,
        name: &OsStr,
        contents: &[u8],
        precondition: &Precondition,
    ) -> io::Result<()> {
        let (dir, held) = hold_dirs(root, relative.parent().unwrap_or(Path::new("")), true)?;
        let path = dir.join(name);
        let permissions = match open_existing(&path, GENERIC_READ)? {
            Some((file, opened)) => {
                check_handle(&file, &opened, precondition)?;
                if matches!(precondition, Precondition::Absent) {
                    return Err(StaleFile::Exists.into());
                }
                Some(file.metadata()?.permissions())
            }
            None => {
                precondition.check(None)?;
                None
            }
        };
        let result = replace_entry(&dir, name, permissions, contents);
        drop(held);
        result
    }

    /// Removes the entry `name` of the directory `relative` beneath `root` once
    /// `precondition` holds, through a verified handle that follows no link (spec 3.1
    /// §7.26 `apply_patch`).
    pub(crate) fn remove_beneath(
        root: &Path,
        relative: &Path,
        name: &OsStr,
        precondition: &Precondition,
    ) -> io::Result<()> {
        let (dir, held) = hold_dirs(root, relative.parent().unwrap_or(Path::new("")), false)?;
        let path = dir.join(name);
        let Some((file, opened)) = open_existing(&path, GENERIC_READ | DELETE)? else {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        };
        check_handle(&file, &opened, precondition)?;
        delete_on_close(&file)?;
        drop(file);
        drop(held);
        Ok(())
    }

    /// The reparse tag of the object `file` holds, or `None` when it is not a reparse
    /// point.
    fn reparse_tag(file: &File) -> io::Result<Option<u32>> {
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        // SAFETY: `tag` is writable and the size passed is its own; the handle is open.
        let queried = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileAttributeTagInfo,
                (&raw mut tag).cast(),
                size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(tag.ReparseTag))
    }

    /// Whether `error` reports an open refused because a link would have redirected it.
    pub(crate) fn is_symlink_refusal(error: &io::Error) -> bool {
        error.raw_os_error() == Some(ERROR_STOPPED_ON_SYMLINK as i32)
    }

    /// What the checks found on an opened handle.
    struct Opened {
        info: BY_HANDLE_FILE_INFORMATION,
        /// The handle holds an in-place reparse point, such as a cloud placeholder.
        in_place: bool,
    }

    /// Refuses a handle that holds a name-surrogate reparse point, or whose final path is
    /// not `expected`, the checked target beneath the root's final path.
    fn verify(file: &File, expected: &Path) -> io::Result<Opened> {
        let tag = reparse_tag(file)?;
        if tag.is_some_and(super::is_name_surrogate) || final_path(file)? != expected {
            return Err(io::Error::from_raw_os_error(ERROR_STOPPED_ON_SYMLINK as i32));
        }
        Ok(Opened { info: file_info(file)?, in_place: tag.is_some() })
    }

    /// The handle to read or write through: `file` itself, or for an in-place reparse
    /// point a reopen of the handle, which must reach the same file.
    ///
    /// A handle opened with `FILE_FLAG_OPEN_REPARSE_POINT` is returned "whether or not the
    /// filter that controls the reparse point is operational", so the content of a cloud
    /// placeholder or a WOF-compressed file is read and written through a reopen without
    /// the flag, which that filter serves. Reopening the handle, not the path, cannot reach
    /// another file.
    fn follow_in_place(file: File, opened: &Opened, access: u32) -> io::Result<File> {
        if !opened.in_place {
            return Ok(file);
        }
        // SAFETY: the handle is open; `ReOpenFile` returns a new handle or
        // `INVALID_HANDLE_VALUE`.
        let handle = unsafe {
            ReOpenFile(
                file.as_raw_handle(),
                access | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handle` was just opened and nothing else owns it.
        let reopened = unsafe { File::from_raw_handle(handle) };
        let (before, after) = (opened.info, file_info(&reopened)?);
        if (before.dwVolumeSerialNumber, before.nFileIndexHigh, before.nFileIndexLow)
            != (after.dwVolumeSerialNumber, after.nFileIndexHigh, after.nFileIndexLow)
        {
            return Err(io::Error::from_raw_os_error(ERROR_STOPPED_ON_SYMLINK as i32));
        }
        Ok(reopened)
    }

    /// The link count, volume and file index of the object `file` holds.
    fn file_info(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `info` is writable for the call; the handle is open.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info)
    }

    /// The final path of the object `file` holds, `\\?\`-prefixed (`VOLUME_NAME_DOS`).
    ///
    /// A volume mounted without a drive letter fails with `ERROR_PATH_NOT_FOUND`, which
    /// refuses the open instead of letting it through.
    fn final_path(file: &File) -> io::Result<PathBuf> {
        let mut buffer = vec![0u16; 512];
        loop {
            let capacity = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
            // SAFETY: `buffer` holds `capacity` writable units; the handle is open.
            let written = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle(),
                    buffer.as_mut_ptr(),
                    capacity,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
                )
            } as usize;
            if written == 0 {
                return Err(io::Error::last_os_error());
            }
            if written < buffer.len() {
                buffer.truncate(written);
                return Ok(PathBuf::from(OsString::from_wide(&buffer)));
            }
            // The buffer was too small: `written` is the size needed, its NUL included.
            buffer.resize(written, 0);
        }
    }

    /// `relative`, which beneath the root holds only plain names.
    fn plain(relative: &Path) -> io::Result<&Path> {
        if relative.components().all(|component| matches!(component, Component::Normal(_))) {
            Ok(relative)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a path beneath the workspace root holds only plain names",
            ))
        }
    }

    /// Opens the directory `path` with `flags`, holding it until the handle closes
    /// against being renamed, deleted or replaced.
    fn open_dir(path: &Path, flags: u32) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | flags)
            .open(path)
    }

    /// Opens and checks the directory `path`, creating it when missing, without following
    /// a link on it.
    fn open_or_create_dir(path: &Path) -> io::Result<File> {
        let dir = match open_dir(path, FILE_FLAG_OPEN_REPARSE_POINT) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Err(error) = std::fs::create_dir(path)
                    && error.kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(error);
                }
                open_dir(path, FILE_FLAG_OPEN_REPARSE_POINT)?
            }
            result => result?,
        };
        verify(&dir, path)?;
        Ok(dir)
    }

    /// Opens `path` for writing, and for reading when `read` is set, without truncating it
    /// or following a link on it, creating the file when missing; `true` when this open
    /// created it.
    fn open_for_write(path: &Path, read: bool) -> io::Result<(File, bool)> {
        let mut options = OpenOptions::new();
        options.write(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let read = if read { GENERIC_READ } else { 0 };
        // A file this open creates can be deleted through its handle should a check
        // refuse it.
        let created = options
            .clone()
            .create_new(true)
            .access_mode(GENERIC_WRITE | read | FILE_READ_ATTRIBUTES | DELETE)
            .open(path);
        match created {
            Ok(file) => Ok((file, true)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => options
                .access_mode(GENERIC_WRITE | read | FILE_READ_ATTRIBUTES)
                .open(path)
                .map(|file| (file, false)),
            Err(error) => Err(error),
        }
    }

    /// Marks the file `file` holds for deletion when its last handle closes.
    fn delete_on_close(file: &File) -> io::Result<()> {
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: `info` is readable and the size passed is its own; the handle is open.
        let set = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&raw const info).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if set == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// Writes `contents` to a new entry beside `name` in the held directory `dir` and
    /// renames it over `name`, with `permissions` when given.
    fn replace_entry(
        dir: &Path,
        name: &OsStr,
        permissions: Option<std::fs::Permissions>,
        contents: &[u8],
    ) -> io::Result<()> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let temp =
            dir.join(format!(".{}.{}.{nanos}.tmp", name.to_string_lossy(), std::process::id()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .access_mode(GENERIC_WRITE | FILE_READ_ATTRIBUTES)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&temp)?;
            verify(&file, &temp)?;
            file.write_all(contents)?;
            if let Some(permissions) = permissions {
                file.set_permissions(permissions)?;
            }
            drop(file);
            std::fs::rename(&temp, dir.join(name))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::is_name_surrogate;

    #[test]
    fn links_and_junctions_are_name_surrogates_and_in_place_tags_are_not() {
        // Tag values from winnt.h; only the name-surrogate bit decides.
        let surrogates = [
            ("IO_REPARSE_TAG_SYMLINK", 0xA000_000C),
            ("IO_REPARSE_TAG_MOUNT_POINT", 0xA000_0003),
            ("IO_REPARSE_TAG_LX_SYMLINK", 0xA000_001D),
            ("IO_REPARSE_TAG_GLOBAL_REPARSE", 0xA000_0019),
        ];
        let in_place = [
            ("IO_REPARSE_TAG_CLOUD", 0x9000_001A),
            ("IO_REPARSE_TAG_CLOUD_6", 0x9000_601A),
            ("IO_REPARSE_TAG_ONEDRIVE", 0x8000_0021),
            ("IO_REPARSE_TAG_WOF", 0x8000_0017),
            ("IO_REPARSE_TAG_DEDUP", 0x8000_0013),
            ("IO_REPARSE_TAG_PROJFS", 0x9000_001C),
            ("IO_REPARSE_TAG_APPEXECLINK", 0x8000_001B),
            ("IO_REPARSE_TAG_HSM", 0xC000_0004),
        ];
        for (name, tag) in surrogates {
            assert!(is_name_surrogate(tag), "{name} must be refused");
        }
        for (name, tag) in in_place {
            assert!(!is_name_surrogate(tag), "{name} must pass");
        }
    }
}
