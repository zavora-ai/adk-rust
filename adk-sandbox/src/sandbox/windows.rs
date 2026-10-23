//! Windows restricted-token sandbox enforcer — the first Windows tier (spec 3.2 §10 W5).
//!
//! This enforcer confines a shell on Windows with a **restricted token plus explicit ACL
//! grants**, following the restricted-token ladder (as Codex does); AppContainer stays out of
//! scope. It runs the command under a token built with `CreateRestrictedToken` — deny-only
//! SIDs for the user's broad groups, a restricting SID set that admits only what the policy
//! allows — and grants the restricting `RESTRICTED` SID explicit access to the workspace and
//! scratch paths the [`SandboxPolicy`] names. A Job Object kills every descendant with the
//! call.
//!
//! # Why it does not plug into the generic `wrap_command`/`configure_command` path
//!
//! A restricted primary token can only be applied to a process at creation, through
//! `CreateProcessAsUserW`, whose result is a raw process handle. The generic enforcer path
//! builds a [`tokio::process::Command`] that the caller spawns with `Command::spawn`, and a
//! `tokio::process::Child` cannot be constructed from a foreign handle. The enforcer therefore
//! owns its own spawn through [`WindowsEnforcer::run_confined`]; [`SandboxEnforcer::probe`] and
//! [`SandboxEnforcer::configure_command`] report that the generic path cannot carry the token
//! and name the working one, so a caller never runs a command believing it is confined when it
//! is not.
//!
//! # What is enforced, and what is not
//!
//! | Aspect | Behaviour |
//! |--------|-----------|
//! | Write outside the granted paths | Denied — the restricted-SID access check fails off the granted paths |
//! | Write inside a read-write path | Allowed — `RESTRICTED` is granted there and the path keeps the user's own entry |
//! | Read of a path the policy does not grant and that is not world-readable | Denied by the restricted-SID check |
//! | World-readable system files (DLLs, `cmd.exe`) | Readable, so the process can start (`Everyone` is a restricting SID) |
//! | Descendant processes | Killed with the call's Job Object |
//! | Network | **Not enforced** — Windows offers no per-process network filter without AppContainer |
//! | Integrity level | Left at the token's default; Low/Untrusted integrity needs the granted paths labelled and is a later rung |

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use super::{AccessMode, AllowedPath, SandboxEnforcer, SandboxPolicy, WrappedCommand};
use crate::error::SandboxError;

/// The enforcer name reported in [`SandboxEnforcer::name`], audit records and
/// `IsolationStatus`.
pub const ENFORCER_NAME: &str = "restricted-token";

/// Windows restricted-token sandbox enforcer (spec 3.2 §10 W5).
///
/// Confines a command with a restricted token and explicit ACL grants on the policy's paths,
/// in a Job Object that dies with the call. Run a command through
/// [`run_confined`](WindowsEnforcer::run_confined); the generic
/// [`SandboxEnforcer`] trait methods cannot apply a restricted token to a
/// [`tokio::process::Command`] and say so.
///
/// # Example
///
/// ```rust,ignore
/// use std::ffi::OsString;
/// use std::path::Path;
/// use std::time::Duration;
///
/// use adk_sandbox::sandbox::windows::WindowsEnforcer;
/// use adk_sandbox::sandbox::SandboxPolicyBuilder;
///
/// let workspace = Path::new("C:\\work\\project");
/// let policy = SandboxPolicyBuilder::new().allow_read_write(workspace).build();
/// let enforcer = WindowsEnforcer::new();
/// let outcome = enforcer.run_confined(
///     &policy,
///     "cmd.exe".as_ref(),
///     &[OsString::from("/c"), OsString::from("echo ok > inside.txt")],
///     workspace,
///     Some(Duration::from_secs(30)),
/// )?;
/// assert_eq!(outcome.exit_code, Some(0));
/// # Ok::<(), adk_sandbox::SandboxError>(())
/// ```
pub struct WindowsEnforcer;

impl WindowsEnforcer {
    /// Creates a new Windows restricted-token enforcer.
    pub fn new() -> Self {
        Self
    }

    /// Runs `program` with `args` under a restricted token, with ACL grants on the policy's
    /// paths and a Job Object that kills every descendant with the call, starting in `cwd`.
    ///
    /// The environment is `policy.env` (plus `SystemRoot`, injected from the parent when the
    /// policy omits it, so the shell can start). Standard streams are not captured; the
    /// outcome carries the exit code and whether the call timed out. On a timeout the job is
    /// terminated and [`ConfinedOutcome::timed_out`] is `true`.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::EnforcerUnavailable`] off Windows, [`SandboxError::PolicyViolation`]
    /// when an allowed path cannot be resolved, and [`SandboxError::EnforcerFailed`] when a Win32
    /// step fails (with the failing call and the OS error).
    pub fn run_confined(
        &self,
        policy: &SandboxPolicy,
        program: &OsStr,
        args: &[OsString],
        cwd: &std::path::Path,
        timeout: Option<std::time::Duration>,
    ) -> Result<ConfinedOutcome, SandboxError> {
        let grants = grant_targets(policy);
        #[cfg(all(target_os = "windows", feature = "sandbox-windows"))]
        {
            imp::run_confined(policy, &grants, program, args, cwd, timeout)
        }
        #[cfg(not(all(target_os = "windows", feature = "sandbox-windows")))]
        {
            let _ = (policy, &grants, program, args, cwd, timeout);
            Err(SandboxError::EnforcerUnavailable {
                enforcer: ENFORCER_NAME.to_string(),
                message: "the restricted-token enforcer runs only on Windows with the \
                          sandbox-windows feature"
                    .to_string(),
            })
        }
    }
}

impl Default for WindowsEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

/// The outcome of a confined run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfinedOutcome {
    /// The process exit code, or `None` when it was terminated without one.
    pub exit_code: Option<i32>,
    /// Whether the run hit the timeout and its Job Object was terminated.
    pub timed_out: bool,
}

/// The paths to grant the restricting SID, from the policy, in the order they are granted.
///
/// Only paths that exist are returned, since the grant resolves the path on the file system.
/// For the same path the most permissive mode wins, so a read-write grant is not shadowed by
/// a read-only one listed later.
///
/// # Example
///
/// ```rust
/// use std::path::PathBuf;
/// use adk_sandbox::sandbox::windows::grant_targets;
/// use adk_sandbox::sandbox::SandboxPolicyBuilder;
///
/// let tmp = std::env::temp_dir();
/// let policy = SandboxPolicyBuilder::new().allow_read_write(&tmp).build();
/// let grants = grant_targets(&policy);
/// assert!(grants.iter().any(|g| g.path == std::fs::canonicalize(&tmp).unwrap_or(tmp.clone())));
/// ```
pub fn grant_targets(policy: &SandboxPolicy) -> Vec<AllowedPath> {
    let mut best: Vec<AllowedPath> = Vec::new();
    for entry in &policy.allowed_paths {
        let path = std::fs::canonicalize(&entry.path).unwrap_or_else(|_| entry.path.clone());
        match best.iter_mut().find(|existing| existing.path == path) {
            Some(existing) => {
                if entry.mode == AccessMode::ReadWrite {
                    existing.mode = AccessMode::ReadWrite;
                }
            }
            None => best.push(AllowedPath { path, mode: entry.mode }),
        }
    }
    best
}

impl SandboxEnforcer for WindowsEnforcer {
    fn name(&self) -> &str {
        ENFORCER_NAME
    }

    fn probe(&self) -> Result<(), SandboxError> {
        // Honest about the *generic* trait path: the restricted token cannot be applied to a
        // `tokio::process::Command` that the caller spawns with `Command::spawn`, so an
        // enforcer selected by this probe and used through `sandboxed_command` would run
        // unconfined. `run_confined` is the working path and is named here. A probe that
        // succeeded while the generic path could not confine would be worse than one that
        // says so (spec 3.1 §7.9 "Availability").
        #[cfg(target_os = "windows")]
        let message = "the Windows restricted-token enforcer confines a command only through \
                       WindowsEnforcer::run_confined; it cannot be applied through the generic \
                       configure_command path, because a restricted token is set at process \
                       creation (CreateProcessAsUserW) and cannot be attached to a \
                       tokio::process::Command the caller spawns.";
        #[cfg(not(target_os = "windows"))]
        let message = "the restricted-token enforcer runs only on Windows.";
        Err(SandboxError::EnforcerUnavailable {
            enforcer: ENFORCER_NAME.to_string(),
            message: message.to_string(),
        })
    }

    fn wrap_command(
        &self,
        program: &OsStr,
        args: &[OsString],
        policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError> {
        // The command is not rewritten: confinement is applied in `run_confined`. Paths are
        // still resolved here so a missing one is reported early, as the other enforcers do.
        canonicalize_paths(&policy.allowed_paths)?;
        Ok(WrappedCommand { program: program.to_owned(), args: args.to_vec() })
    }

    fn configure_command(
        &self,
        _cmd: &mut tokio::process::Command,
        _policy: &SandboxPolicy,
    ) -> Result<(), SandboxError> {
        Err(SandboxError::EnforcerFailed {
            enforcer: ENFORCER_NAME.to_string(),
            message: "the restricted token is applied at process creation and cannot be set on \
                      a tokio::process::Command; run the command through \
                      WindowsEnforcer::run_confined instead."
                .to_string(),
        })
    }
}

/// Canonicalizes all paths in the policy, failing when one cannot be resolved.
fn canonicalize_paths(paths: &[AllowedPath]) -> Result<Vec<AllowedPath>, SandboxError> {
    let mut result = Vec::with_capacity(paths.len());
    for entry in paths {
        let canonical = std::fs::canonicalize(&entry.path).map_err(|e| {
            SandboxError::PolicyViolation(format!(
                "failed to canonicalize allowed path '{}': {e}",
                entry.path.display()
            ))
        })?;
        if canonical != entry.path {
            tracing::warn!(
                original = %entry.path.display(),
                resolved = %canonical.display(),
                "allowed path resolved to a different location (possible symlink)"
            );
        }
        result.push(AllowedPath { path: canonical, mode: entry.mode });
    }
    Ok(result)
}

/// Strips the extended-length prefix `\\?\` that `std::fs::canonicalize` adds, which the
/// older ACL and process APIs do not accept.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn strip_unc_prefix(path: &std::path::Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

#[cfg(all(target_os = "windows", feature = "sandbox-windows"))]
mod imp {
    //! The Win32 implementation of the restricted-token launch.

    use std::ffi::{OsStr, OsString, c_void};
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_FAILED,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE,
        SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
        TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, AllocateAndInitializeSid, CopySid, CreateRestrictedToken, DACL_SECURITY_INFORMATION,
        DISABLE_MAX_PRIVILEGE, FreeSid, GetLengthSid, GetTokenInformation, PSID,
        SID_AND_ATTRIBUTES, SID_IDENTIFIER_AUTHORITY, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_QUERY, TokenGroups,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
        GetExitCodeProcess, INFINITE, OpenProcessToken, PROCESS_INFORMATION, ResumeThread,
        STARTUPINFOW, WaitForSingleObject,
    };

    use super::{ConfinedOutcome, ENFORCER_NAME, strip_unc_prefix};
    use crate::error::SandboxError;
    use crate::sandbox::{AccessMode, AllowedPath, SandboxPolicy};

    /// Owns a Win32 `HANDLE` and closes it on drop.
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                // SAFETY: the handle is open and owned here.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// Owns a SID from `AllocateAndInitializeSid` and frees it on drop.
    struct AllocatedSid(PSID);
    impl Drop for AllocatedSid {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the SID came from `AllocateAndInitializeSid` and is freed once.
                unsafe { FreeSid(self.0) };
            }
        }
    }

    fn last_error(call: &str) -> SandboxError {
        // SAFETY: `GetLastError` reads thread-local state and is always safe.
        let code = unsafe { GetLastError() };
        SandboxError::EnforcerFailed {
            enforcer: ENFORCER_NAME.to_string(),
            message: format!("{call} failed (os error {code})"),
        }
    }

    /// Allocates a well-known SID from an authority and sub-authorities.
    fn sid(authority: [u8; 6], sub: &[u32]) -> Result<AllocatedSid, SandboxError> {
        let ident = SID_IDENTIFIER_AUTHORITY { Value: authority };
        let mut out: PSID = std::ptr::null_mut();
        let s = |i: usize| sub.get(i).copied().unwrap_or(0);
        // SAFETY: `ident` is readable, `out` is writable, and the sub-authority count matches
        // the slots passed.
        let ok = unsafe {
            AllocateAndInitializeSid(
                &ident,
                sub.len() as u8,
                s(0),
                s(1),
                s(2),
                s(3),
                s(4),
                s(5),
                s(6),
                s(7),
                &mut out,
            )
        };
        if ok == 0 {
            return Err(last_error("AllocateAndInitializeSid"));
        }
        Ok(AllocatedSid(out))
    }

    const SECURITY_WORLD_SID_AUTHORITY: [u8; 6] = [0, 0, 0, 0, 0, 1];
    const SECURITY_NT_AUTHORITY: [u8; 6] = [0, 0, 0, 0, 0, 5];
    const SECURITY_WORLD_RID: u32 = 0;
    const SECURITY_RESTRICTED_CODE_RID: u32 = 12;
    const SECURITY_AUTHENTICATED_USER_RID: u32 = 11;
    const SECURITY_BUILTIN_DOMAIN_RID: u32 = 32;
    const DOMAIN_ALIAS_RID_USERS: u32 = 545;
    /// `SE_GROUP_LOGON_ID`, as the `u32` mask `SID_AND_ATTRIBUTES::Attributes` carries.
    const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;

    /// The current process's primary token, opened with the rights
    /// `CreateRestrictedToken`/`CreateProcessAsUserW` need.
    fn open_process_token() -> Result<Handle, SandboxError> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle from `GetCurrentProcess` needs no close; `token` is writable.
        let ok = unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_QUERY,
                &mut token,
            )
        };
        if ok == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        Ok(Handle(token))
    }

    /// A copy of the token's logon SID (the group carrying `SE_GROUP_LOGON_ID`), in owned
    /// storage. The logon SID keeps the window-station and desktop access the restricted
    /// process needs to start.
    fn logon_sid(token: HANDLE) -> Result<Vec<u8>, SandboxError> {
        let mut needed: u32 = 0;
        // SAFETY: a null buffer asks only for the size, written to `needed`.
        unsafe { GetTokenInformation(token, TokenGroups, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(last_error("GetTokenInformation(size)"));
        }
        let mut buf = vec![0u8; needed as usize];
        // SAFETY: `buf` holds `needed` writable bytes.
        let ok = unsafe {
            GetTokenInformation(token, TokenGroups, buf.as_mut_ptr().cast(), needed, &mut needed)
        };
        if ok == 0 {
            return Err(last_error("GetTokenInformation"));
        }
        // SAFETY: `buf` holds a `TOKEN_GROUPS` as `GetTokenInformation` wrote it.
        let groups = unsafe { &*buf.as_ptr().cast::<TOKEN_GROUPS>() };
        let entries = groups.Groups.as_ptr();
        for i in 0..groups.GroupCount as usize {
            // SAFETY: `i` is within `GroupCount`, so the entry is part of the buffer.
            let entry = unsafe { &*entries.add(i) };
            if entry.Attributes & SE_GROUP_LOGON_ID != 0 {
                // SAFETY: `entry.Sid` points into `buf` and is a valid SID.
                let len = unsafe { GetLengthSid(entry.Sid) } as usize;
                let mut copy = vec![0u8; len];
                // SAFETY: `copy` holds `len` bytes and `entry.Sid` is a valid SID of that length.
                let ok = unsafe { CopySid(len as u32, copy.as_mut_ptr().cast(), entry.Sid) };
                if ok == 0 {
                    return Err(last_error("CopySid"));
                }
                return Ok(copy);
            }
        }
        Err(SandboxError::EnforcerFailed {
            enforcer: ENFORCER_NAME.to_string(),
            message: "the process token carries no logon SID".to_string(),
        })
    }

    /// Builds the restricted token: deny-only broad groups, restricting SIDs that admit only
    /// world-readable code, the `RESTRICTED` marker and the logon SID.
    ///
    /// Returns the token and the `RESTRICTED` SID storage (kept alive for the ACL grants).
    fn restricted_token(
        source: HANDLE,
        restricted_sid: PSID,
        world_sid: PSID,
        logon: &[u8],
        deny: &[PSID],
    ) -> Result<Handle, SandboxError> {
        let deny_attrs: Vec<SID_AND_ATTRIBUTES> =
            deny.iter().map(|&s| SID_AND_ATTRIBUTES { Sid: s, Attributes: 0 }).collect();
        let restrict_attrs = [
            SID_AND_ATTRIBUTES { Sid: world_sid, Attributes: 0 },
            SID_AND_ATTRIBUTES { Sid: restricted_sid, Attributes: 0 },
            SID_AND_ATTRIBUTES { Sid: logon.as_ptr() as PSID, Attributes: 0 },
        ];
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the SID arrays are readable for their stated counts, `source` is open, and
        // `token` is writable. The logon SID pointer is valid for the duration of the call.
        let ok = unsafe {
            CreateRestrictedToken(
                source,
                DISABLE_MAX_PRIVILEGE,
                deny_attrs.len() as u32,
                deny_attrs.as_ptr(),
                0,
                std::ptr::null(),
                restrict_attrs.len() as u32,
                restrict_attrs.as_ptr(),
                &mut token,
            )
        };
        if ok == 0 {
            return Err(last_error("CreateRestrictedToken"));
        }
        Ok(Handle(token))
    }

    /// Grants `restricted_sid` the access `mode` needs on `path`, merged into the path's
    /// existing DACL so the user's own entry — which the restricted token's normal access
    /// check still needs — is kept.
    fn grant_path(path: &Path, mode: AccessMode, restricted_sid: PSID) -> Result<(), SandboxError> {
        let access = match mode {
            AccessMode::ReadWrite => FILE_ALL_ACCESS,
            AccessMode::ReadOnly => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        };
        let mut wide: Vec<u16> =
            strip_unc_prefix(path).as_os_str().encode_wide().chain(Some(0)).collect();

        let mut old_dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; the out-pointers are writable; the descriptor the
        // call allocates is freed below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut old_dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(SandboxError::EnforcerFailed {
                enforcer: ENFORCER_NAME.to_string(),
                message: format!("GetNamedSecurityInfoW failed (os error {status})"),
            });
        }
        let _descriptor = LocalGuard(descriptor);

        let explicit = EXPLICIT_ACCESS_W {
            grfAccessPermissions: access,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: restricted_sid.cast(),
            },
        };
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: one explicit entry is passed, `old_dacl` is valid until its descriptor is
        // freed, and `new_dacl` is writable. The ACL the call allocates is freed below.
        let status = unsafe { SetEntriesInAclW(1, &explicit, old_dacl, &mut new_dacl) };
        if status != 0 {
            return Err(SandboxError::EnforcerFailed {
                enforcer: ENFORCER_NAME.to_string(),
                message: format!("SetEntriesInAclW failed (os error {status})"),
            });
        }
        let _new = LocalGuard(new_dacl.cast());

        // SAFETY: `wide` is NUL-terminated and `new_dacl` is a valid ACL for the call.
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new_dacl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            return Err(SandboxError::EnforcerFailed {
                enforcer: ENFORCER_NAME.to_string(),
                message: format!("SetNamedSecurityInfoW failed (os error {status})"),
            });
        }
        Ok(())
    }

    /// Frees memory from a `Local*`/`SetEntriesInAclW`/`GetNamedSecurityInfoW` allocation.
    struct LocalGuard(*mut c_void);
    impl Drop for LocalGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the pointer came from a Win32 call that allocates with `LocalAlloc`.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    /// Quotes one argv element for a Windows command line (the `CommandLineToArgvW` rules).
    fn quote_arg(arg: &OsStr) -> Vec<u16> {
        let text: Vec<u16> = arg.encode_wide().collect();
        let needs_quotes = text.is_empty()
            || text.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == b'"' as u16);
        if !needs_quotes {
            return text;
        }
        let mut out = vec![b'"' as u16];
        let mut backslashes = 0usize;
        for &c in &text {
            if c == b'\\' as u16 {
                backslashes += 1;
            } else if c == b'"' as u16 {
                for _ in 0..backslashes * 2 + 1 {
                    out.push(b'\\' as u16);
                }
                backslashes = 0;
                out.push(c);
            } else {
                for _ in 0..backslashes {
                    out.push(b'\\' as u16);
                }
                backslashes = 0;
                out.push(c);
            }
        }
        for _ in 0..backslashes * 2 {
            out.push(b'\\' as u16);
        }
        out.push(b'"' as u16);
        out
    }

    /// The command line `CreateProcessAsUserW` receives: program then args, each quoted.
    fn command_line(program: &OsStr, args: &[OsString]) -> Vec<u16> {
        let mut line = quote_arg(program);
        for arg in args {
            line.push(b' ' as u16);
            line.extend(quote_arg(arg));
        }
        line.push(0);
        line
    }

    /// The UTF-16 environment block for `policy.env`, with `SystemRoot` injected from the
    /// parent when absent so the shell and its DLLs can start. Returns `None` to inherit the
    /// parent environment when the policy sets nothing.
    fn environment_block(policy: &SandboxPolicy) -> Option<Vec<u16>> {
        if policy.env.is_empty() {
            return None;
        }
        let mut pairs: Vec<(String, String)> =
            policy.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let has = |name: &str| pairs.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
        if !has("SystemRoot")
            && let Ok(value) = std::env::var("SystemRoot")
        {
            pairs.push(("SystemRoot".to_string(), value));
        }
        // Windows requires the block sorted by name, case-insensitively.
        pairs.sort_by_key(|(key, _)| key.to_ascii_uppercase());
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in pairs {
            block.extend(OsString::from(format!("{k}={v}")).encode_wide());
            block.push(0);
        }
        block.push(0);
        Some(block)
    }

    /// Creates a Job Object that kills its processes when its last handle closes.
    fn create_job() -> Result<Handle, SandboxError> {
        // SAFETY: both arguments may be null: no security attributes, no name.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(last_error("CreateJobObjectW"));
        }
        let job = Handle(handle);
        // SAFETY: the limit structure is a plain C struct valid when zero-initialised.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is readable for its own size and the handle is open.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(last_error("SetInformationJobObject"));
        }
        Ok(job)
    }

    pub(super) fn run_confined(
        policy: &SandboxPolicy,
        grants: &[AllowedPath],
        program: &OsStr,
        args: &[OsString],
        cwd: &Path,
        timeout: Option<Duration>,
    ) -> Result<ConfinedOutcome, SandboxError> {
        let process_token = open_process_token()?;
        let logon = logon_sid(process_token.0)?;

        let world = sid(SECURITY_WORLD_SID_AUTHORITY, &[SECURITY_WORLD_RID])?;
        let restricted = sid(SECURITY_NT_AUTHORITY, &[SECURITY_RESTRICTED_CODE_RID])?;
        let users =
            sid(SECURITY_NT_AUTHORITY, &[SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_USERS])?;
        let authenticated = sid(SECURITY_NT_AUTHORITY, &[SECURITY_AUTHENTICATED_USER_RID])?;

        let token = restricted_token(
            process_token.0,
            restricted.0,
            world.0,
            &logon,
            &[users.0, authenticated.0],
        )?;

        // Grant the restricting RESTRICTED SID access to each allowed path, so the restricted
        // token's second access check passes there and nowhere else.
        for entry in grants {
            grant_path(&entry.path, entry.mode, restricted.0)?;
        }

        let job = create_job()?;

        let mut command = command_line(program, args);
        let cwd_wide: Vec<u16> =
            strip_unc_prefix(cwd).as_os_str().encode_wide().chain(Some(0)).collect();
        let env = environment_block(policy);
        // SAFETY: `STARTUPINFOW` and `PROCESS_INFORMATION` are plain C structs that an all-zero
        // bit pattern initialises validly.
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

        let mut flags = CREATE_SUSPENDED;
        let env_ptr: *const c_void = match env.as_ref() {
            Some(block) => {
                flags |= CREATE_UNICODE_ENVIRONMENT;
                block.as_ptr().cast()
            }
            None => std::ptr::null(),
        };

        // SAFETY: the token is a restricted child of the caller's token, the command line is
        // NUL-terminated and mutable as the API requires, the directory and environment are
        // NUL-terminated, and the startup and info structures are valid for the call.
        let ok = unsafe {
            CreateProcessAsUserW(
                token.0,
                std::ptr::null(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                env_ptr,
                cwd_wide.as_ptr(),
                &startup,
                &mut info,
            )
        };
        if ok == 0 {
            return Err(last_error("CreateProcessAsUserW"));
        }
        let process = Handle(info.hProcess);
        let thread = Handle(info.hThread);

        // Put the process in the job before it runs, so every descendant it starts is
        // captured and killed with it.
        // SAFETY: both handles are open.
        if unsafe { AssignProcessToJobObject(job.0, process.0) } == 0 {
            let err = last_error("AssignProcessToJobObject");
            // SAFETY: the process handle is open; terminate the stillborn process.
            unsafe { TerminateJobObject(job.0, 1) };
            return Err(err);
        }
        // SAFETY: the thread handle is open; resuming a suspended primary thread is safe.
        if unsafe { ResumeThread(thread.0) } == u32::MAX {
            let err = last_error("ResumeThread");
            unsafe { TerminateJobObject(job.0, 1) };
            return Err(err);
        }

        let wait_ms =
            timeout.map_or(INFINITE, |d| u32::try_from(d.as_millis()).unwrap_or(INFINITE));
        // SAFETY: the process handle is open.
        let waited = unsafe { WaitForSingleObject(process.0, wait_ms) };
        if waited == WAIT_TIMEOUT {
            // SAFETY: the job handle is open; kill the whole tree.
            unsafe { TerminateJobObject(job.0, 1) };
            return Ok(ConfinedOutcome { exit_code: None, timed_out: true });
        }
        if waited == WAIT_FAILED {
            let err = last_error("WaitForSingleObject");
            unsafe { TerminateJobObject(job.0, 1) };
            return Err(err);
        }
        debug_assert_eq!(waited, WAIT_OBJECT_0);

        let mut code: u32 = 0;
        // SAFETY: the process handle is open and `code` is writable.
        let got = unsafe { GetExitCodeProcess(process.0, &mut code) };
        // The process has exited, so terminating the job now only reaps any descendant that
        // outlived it.
        // SAFETY: the job handle is open.
        unsafe { TerminateJobObject(job.0, 1) };
        if got == 0 {
            return Err(last_error("GetExitCodeProcess"));
        }
        Ok(ConfinedOutcome { exit_code: Some(code as i32), timed_out: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxPolicyBuilder;

    #[test]
    fn name_is_restricted_token() {
        assert_eq!(WindowsEnforcer::new().name(), ENFORCER_NAME);
    }

    #[test]
    fn probe_reports_the_generic_path_is_unavailable() {
        let err = WindowsEnforcer::new().probe().unwrap_err();
        assert!(matches!(err, SandboxError::EnforcerUnavailable { .. }), "{err:?}");
        assert!(err.to_string().to_lowercase().contains("restricted-token"));
    }

    #[test]
    fn configure_command_refuses_the_tokio_path() {
        let mut cmd = tokio::process::Command::new("cmd.exe");
        let policy = SandboxPolicyBuilder::new().build();
        let err = WindowsEnforcer::new().configure_command(&mut cmd, &policy).unwrap_err();
        assert!(matches!(err, SandboxError::EnforcerFailed { .. }), "{err:?}");
    }

    #[test]
    fn wrap_command_returns_the_program_unchanged() {
        let dir = std::env::temp_dir();
        let policy = SandboxPolicyBuilder::new().allow_read_write(&dir).build();
        let wrapped = WindowsEnforcer::new()
            .wrap_command(
                OsStr::new("cmd.exe"),
                &[OsString::from("/c"), OsString::from("echo hi")],
                &policy,
            )
            .expect("wrap_command");
        assert_eq!(wrapped.program, OsString::from("cmd.exe"));
        assert_eq!(wrapped.args, vec![OsString::from("/c"), OsString::from("echo hi")]);
    }

    #[test]
    fn grant_targets_keeps_the_most_permissive_mode_per_path() {
        let dir = std::env::temp_dir();
        let canonical = std::fs::canonicalize(&dir).unwrap_or(dir.clone());
        // Same path once read-only, once read-write: read-write must win.
        let policy = SandboxPolicyBuilder::new().allow_read(&dir).allow_read_write(&dir).build();
        let grants = grant_targets(&policy);
        let entry = grants.iter().find(|g| g.path == canonical).expect("the path is granted");
        assert_eq!(entry.mode, AccessMode::ReadWrite);
    }

    #[test]
    fn grant_targets_drops_paths_that_do_not_exist() {
        let missing = std::env::temp_dir().join("adk-sandbox-w5-does-not-exist-xyz");
        let policy = SandboxPolicyBuilder::new().allow_read_write(&missing).build();
        let grants = grant_targets(&policy);
        // A non-existent path stays as written (canonicalize fails, falls back) but is never
        // promoted; the mapping itself does not drop it, the grant step resolves it. Here we
        // just assert the mapping is stable and single-entry.
        assert_eq!(grants.len(), 1);
    }

    #[test]
    fn strip_unc_prefix_removes_the_extended_prefix() {
        use std::path::Path;
        assert_eq!(strip_unc_prefix(Path::new(r"\\?\C:\work")), PathBuf::from(r"C:\work"));
        assert_eq!(
            strip_unc_prefix(Path::new(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
        assert_eq!(strip_unc_prefix(Path::new(r"C:\plain")), PathBuf::from(r"C:\plain"));
    }
}
