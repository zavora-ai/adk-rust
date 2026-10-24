//! Files only their owner can open on Windows: a protected DACL with one entry, full access
//! for the current user, where Unix creates the file with mode `0600` (spec 3.2 §10 W4).
//!
//! The descriptor is given to `CreateFileW` itself, so the file never exists with the
//! permissions it would inherit from its directory. As with a Unix mode, it applies only to
//! a file the call creates; an existing file keeps its own. [`protect_dir`] covers the files
//! another component creates in a directory, such as SQLite's `-wal` and `-shm`.

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_FLAGS, ACL, ACL_REVISION, AddAccessAllowedAceEx, GetLengthSid,
    GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, PSID, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
/// Creates `path` for writing with an owner-only DACL; fails when it exists.
#[cfg(any(feature = "audit", feature = "persist"))]
pub(crate) fn create_new(path: &Path) -> io::Result<File> {
    create(path, CREATE_NEW)
}

/// Opens `path` for writing, truncating it, and creates it with an owner-only DACL when
/// it does not exist.
#[cfg(all(feature = "harness", feature = "persist"))]
pub(crate) fn create_or_truncate(path: &Path) -> io::Result<File> {
    create(path, windows_sys::Win32::Storage::FileSystem::CREATE_ALWAYS)
}

/// Opens `path` for appending, and creates it with an owner-only DACL when it does not
/// exist: the kernel service's log (spec 3.2 §7.12, §10 W4).
#[cfg(feature = "harness")]
pub(crate) fn open_append(path: &Path) -> io::Result<File> {
    use std::io::{Seek as _, SeekFrom};

    let mut file = create(path, windows_sys::Win32::Storage::FileSystem::OPEN_ALWAYS)?;
    file.seek(SeekFrom::End(0))?;
    Ok(file)
}
/// Gives directory `dir` a protected DACL with one entry, full access for the current user,
/// inherited by every file and directory later created in it (spec 3.2 §10 W4). An entry
/// inherited from above is dropped, and a child with a protected DACL of its own keeps it.
///
/// # Errors
///
/// Returns the OS error when the user may not change the directory's DACL.
#[cfg(feature = "persist")]
pub(crate) fn protect_dir(dir: &Path) -> io::Result<()> {
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows_sys::Win32::Security::{
        CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE,
        PROTECTED_DACL_SECURITY_INFORMATION,
    };

    let user = current_user()?;
    let mut acl = owner_acl(&user, OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE)?;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and `acl` is a valid list; both outlive the call.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl.as_mut_ptr().cast(),
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// An access-control list with one entry, full access for the `TOKEN_USER` in `user`,
/// carrying the inheritance `flags`. The `u32` storage keeps the list aligned as
/// `InitializeAcl` requires.
fn owner_acl(user: &[u64], flags: ACE_FLAGS) -> io::Result<Vec<u32>> {
    let sid = user_sid(user);
    // SAFETY: `sid` points at a valid SID inside `user`.
    let sid_len = unsafe { GetLengthSid(sid) } as usize;
    // The entry's `SidStart` field is the first four bytes of the SID it holds.
    let acl_len = size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_len;
    let mut storage = vec![0u32; acl_len.div_ceil(size_of::<u32>())];
    let acl = storage.as_mut_ptr().cast::<ACL>();
    // SAFETY: `acl` holds `acl_len` writable bytes and `sid` is valid.
    let built = unsafe {
        InitializeAcl(acl, acl_len as u32, ACL_REVISION) != 0
            && AddAccessAllowedAceEx(acl, ACL_REVISION, flags, FILE_ALL_ACCESS, sid) != 0
    };
    if !built {
        return Err(io::Error::last_os_error());
    }
    Ok(storage)
}

/// Opens `path` for writing with `disposition`, giving a file the call creates an
/// owner-only DACL.
fn create(path: &Path, disposition: u32) -> io::Result<File> {
    let user = current_user()?;
    let mut storage = owner_acl(&user, 0)?;
    let acl = storage.as_mut_ptr().cast::<ACL>();
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let descriptor_ptr = (&raw mut descriptor).cast();
    // SAFETY: `acl` is a valid list and `descriptor` is writable; both outlive
    // `CreateFileW`, which reads them.
    let built = unsafe {
        InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION) != 0
            && SetSecurityDescriptorDacl(descriptor_ptr, 1, acl, 0) != 0
            // Protected: no entry is inherited from the directory.
            && SetSecurityDescriptorControl(descriptor_ptr, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
                != 0
    };
    if !built {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor_ptr,
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated; `attributes` and what it points to outlive the call.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &raw const attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` was just opened and nothing else owns it.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// The `TOKEN_USER` of the current process, in storage aligned for it.
fn current_user() -> io::Result<Vec<u64>> {
    let mut token = std::ptr::null_mut();
    // SAFETY: the pseudo-handle needs no closing; `token` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` was just opened and nothing else owns it.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0u32;
    // SAFETY: a null buffer of length zero asks only for the size, written to `needed`.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &raw mut needed,
        );
    }
    let mut user = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `user` holds at least `needed` writable bytes.
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            user.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(user)
}

/// The SID of the `TOKEN_USER` that `current_user` returned.
fn user_sid(user: &[u64]) -> PSID {
    // SAFETY: `user` holds a `TOKEN_USER`, aligned, as `current_user` wrote it.
    unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
}

/// Whether `path` carries the DACL `create` gives a file: protected, with one entry that
/// grants the current user full access.
#[cfg(all(test, any(feature = "audit", feature = "persist")))]
pub(crate) fn is_owner_only(path: &Path) -> io::Result<bool> {
    owner_only_dacl(path, true)
}

/// Whether `path`'s DACL has one entry, full access for the current user, set on the file or
/// inherited from a directory [`protect_dir`] protected.
#[cfg(all(test, feature = "persist"))]
pub(crate) fn only_the_owner_has_access(path: &Path) -> io::Result<bool> {
    owner_only_dacl(path, false)
}

/// Whether `path`'s DACL has one entry, full access for the current user, and, when
/// `protected`, is protected from inheritance.
#[cfg(all(test, any(feature = "audit", feature = "persist")))]
fn owner_only_dacl(path: &Path, protected: bool) -> io::Result<bool> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
        GetAclInformation, GetSecurityDescriptorControl,
    };
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    let user = current_user()?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut dacl, mut descriptor) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: `wide` is NUL-terminated and the out-pointers are writable; the descriptor
    // the call allocates is freed below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut dacl,
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let (mut control, mut revision, mut size) = (0u16, 0u32, ACL_SIZE_INFORMATION::default());
    let mut ace = std::ptr::null_mut();
    // SAFETY: `descriptor` and `dacl` come from `GetNamedSecurityInfoW` and stay valid until
    // `LocalFree`; every out-pointer is writable.
    let owner_only = unsafe {
        GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) != 0
            && GetAclInformation(
                dacl,
                (&raw mut size).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            ) != 0
            && (!protected || control & SE_DACL_PROTECTED != 0)
            && size.AceCount == 1
            && GetAce(dacl, 0, &raw mut ace) != 0
            && {
                let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
                u32::from((*ace).Header.AceType) == ACCESS_ALLOWED_ACE_TYPE
                    && (*ace).Mask == FILE_ALL_ACCESS
                    && EqualSid((&raw mut (*ace).SidStart).cast(), user_sid(&user)) != 0
            }
    };
    // SAFETY: `descriptor` was allocated by `GetNamedSecurityInfoW` and is freed once.
    unsafe { LocalFree(descriptor) };
    Ok(owner_only)
}
