//! The Win32 pieces the named pipe and the Windows service share (spec 3.2 §10 W2, W3):
//! the user of a token, its SID as a string, and the account name.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authentication::Identity::{GetUserNameExW, NameSamCompatible};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The user of an access token: its `TOKEN_USER`, in storage aligned for it.
pub(crate) struct UserSid(Vec<u64>);

impl UserSid {
    /// The user this process runs as.
    pub(crate) fn current() -> io::Result<Self> {
        let mut token = std::ptr::null_mut();
        // SAFETY: the pseudo-handle needs no closing; `token` is writable.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `token` was just opened and nothing else owns it.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        Self::of_token(token.as_raw_handle())
    }

    /// The user of `token`, opened with `TOKEN_QUERY`.
    pub(crate) fn of_token(token: HANDLE) -> io::Result<Self> {
        let mut needed = 0u32;
        // SAFETY: a null buffer of length zero asks only for the size, written to `needed`.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &raw mut needed) };
        let mut user = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
        // SAFETY: `user` holds at least `needed` writable bytes.
        let read = unsafe {
            GetTokenInformation(token, TokenUser, user.as_mut_ptr().cast(), needed, &raw mut needed)
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(user))
    }

    /// The SID, valid while `self` lives.
    pub(crate) fn sid(&self) -> PSID {
        // SAFETY: `self.0` holds a `TOKEN_USER`, aligned, as `of_token` wrote it.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    /// Whether `other` is this user's SID.
    pub(crate) fn is(&self, other: PSID) -> bool {
        // SAFETY: both SIDs are valid for the call.
        !other.is_null() && unsafe { EqualSid(self.sid(), other) } != 0
    }

    /// The SID in its string form, `S-1-5-21-…`.
    pub(crate) fn to_sid_string(&self) -> io::Result<String> {
        let mut text = std::ptr::null_mut();
        // SAFETY: the SID is valid; the string the call allocates is freed below.
        if unsafe { ConvertSidToStringSidW(self.sid(), &raw mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `text` is a NUL-terminated wide string from the call.
        let length = (0..).take_while(|&at| unsafe { *text.add(at) } != 0).count();
        // SAFETY: `text` holds `length` initialized code units.
        let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
        // SAFETY: `text` was allocated by `ConvertSidToStringSidW` and is freed once.
        unsafe { LocalFree(text.cast()) };
        Ok(sid)
    }
}

/// The account this process runs as, `DOMAIN\user`.
pub(crate) fn account_name() -> io::Result<String> {
    let mut size = 0u32;
    // SAFETY: a null buffer asks for the size, written to `size`.
    unsafe { GetUserNameExW(NameSamCompatible, std::ptr::null_mut(), &raw mut size) };
    let mut name = vec![0u16; size as usize + 1];
    let mut length = name.len() as u32;
    // SAFETY: `name` holds `length` writable code units.
    if !unsafe { GetUserNameExW(NameSamCompatible, name.as_mut_ptr(), &raw mut length) } {
        return Err(io::Error::last_os_error());
    }
    Ok(String::from_utf16_lossy(&name[..length as usize]))
}

/// `text` as a NUL-terminated wide string.
pub(crate) fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}
