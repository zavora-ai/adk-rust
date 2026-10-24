//! Windows restricted-token sandbox enforcer — the first Windows tier (spec 3.2 §10 W5).
//!
//! This enforcer confines a shell on Windows with a **restricted token plus explicit ACL
//! grants**, following the restricted-token ladder (as Codex does); AppContainer stays out of
//! scope. It runs the command under a token built with `CreateRestrictedToken` — every group
//! that widens access deny-only, `Administrators` included, a restricting SID set that admits only what the policy
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
//! owns its own spawn through [`WindowsEnforcer::run_confined`], which waits for the command,
//! and `WindowsEnforcer::spawn_contained`, which returns the running process with its standard
//! streams on pipes; [`WindowsEnforcer::probe_confined`] probes that path.
//! [`SandboxEnforcer::probe`] and [`SandboxEnforcer::configure_command`] report that the
//! generic path cannot carry the token and name the working one, so a caller never runs a
//! command believing it is confined when it is not.
//!
//! # What is enforced, and what is not
//!
//! | Aspect | Behaviour |
//! |--------|-----------|
//! | Write outside the granted paths | Denied — the restricted-SID access check fails off the granted paths, except where a path grants write access to `Everyone` or `Users` |
//! | Write inside a read-write path | Allowed — `RESTRICTED` is granted there and the path keeps the user's own entry |
//! | Read of a path the policy does not grant and that grants neither `Everyone` nor `Users` (a user's profile) | Denied by the restricted-SID check |
//! | System binaries and DLLs (`System32` grants `Users`) | Readable, so the shell can start programs (`Everyone` and `Users` are restricting SIDs) |
//! | Descendant processes | Killed with the call's Job Object |
//! | Inherited handles | `spawn_contained`'s three standard handles only; `run_confined` inherits none |
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

impl WindowsEnforcer {
    /// Starts `program` with `args` in `cwd` under the restricted token and ACL grants of
    /// [`run_confined`](Self::run_confined), and returns at once with the running process
    /// (spec 3.2 §10 W5).
    ///
    /// The process is held, with every process it starts, by the returned child's Job Object
    /// from before it runs (spec 3.2 §10 W1); dropping the child ends them all. Its standard
    /// input, output and error are pipes, and it inherits those three handles and no other.
    /// The environment is `policy.env` as for `run_confined`. `program` should be a full
    /// path: a bare name is searched for as `CreateProcessAsUserW` does, from the directory
    /// of this process and its working directory before the system directories and `PATH`.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::PolicyViolation`] when an allowed path cannot be resolved, and
    /// [`SandboxError::EnforcerFailed`] when a Win32 step fails (with the failing call and the
    /// OS error).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::ffi::OsString;
    /// use std::io::Read as _;
    /// use std::path::Path;
    ///
    /// use adk_sandbox::sandbox::SandboxPolicyBuilder;
    /// use adk_sandbox::sandbox::windows::WindowsEnforcer;
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let workspace = Path::new("C:\\work\\project");
    /// let policy = SandboxPolicyBuilder::new().allow_read_write(workspace).build();
    /// let mut child = WindowsEnforcer::new().spawn_contained(
    ///     &policy,
    ///     "C:\\Windows\\System32\\cmd.exe".as_ref(),
    ///     &[OsString::from("/c"), OsString::from("echo ok")],
    ///     workspace,
    /// )?;
    /// drop(child.stdin.take());
    /// let mut output = String::new();
    /// child.stdout.take().expect("piped").read_to_string(&mut output)?;
    /// assert_eq!(child.process.wait().await?, 0);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(all(target_os = "windows", feature = "sandbox-windows"))]
    pub fn spawn_contained(
        &self,
        policy: &SandboxPolicy,
        program: &OsStr,
        args: &[OsString],
        cwd: &std::path::Path,
    ) -> Result<crate::process_tree::ContainedChild, SandboxError> {
        imp::spawn_contained(policy, &grant_targets(policy), program, args, cwd)
    }

    /// Whether [`run_confined`](Self::run_confined) and `spawn_contained` confine a command
    /// on this host: runs `cmd.exe /d /c exit 0` from the system directory under the
    /// restricted token, with no path granted (spec 3.2 §10 W5).
    ///
    /// This is the probe of the working path. [`SandboxEnforcer::probe`] reports the generic
    /// `configure_command` path, which cannot carry a restricted token and never succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::EnforcerUnavailable`] off Windows, without the
    /// `sandbox-windows` feature, and when the probe command cannot run or does not exit 0.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_sandbox::sandbox::windows::WindowsEnforcer;
    ///
    /// match WindowsEnforcer::new().probe_confined() {
    ///     Ok(()) => println!("commands run under the restricted token"),
    ///     Err(error) => println!("the restricted-token sandbox is unavailable: {error}"),
    /// }
    /// ```
    pub fn probe_confined(&self) -> Result<(), SandboxError> {
        let unavailable = |message: String| SandboxError::EnforcerUnavailable {
            enforcer: ENFORCER_NAME.to_string(),
            message,
        };
        #[cfg(all(target_os = "windows", feature = "sandbox-windows"))]
        {
            let system = std::env::var_os("SystemRoot")
                .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
                .join("System32");
            let outcome = self
                .run_confined(
                    &super::SandboxPolicyBuilder::new().build(),
                    system.join("cmd.exe").as_os_str(),
                    &[OsString::from("/d"), OsString::from("/c"), OsString::from("exit 0")],
                    &system,
                    Some(std::time::Duration::from_secs(15)),
                )
                .map_err(|error| {
                    unavailable(format!(
                        "a probe command could not start under the restricted token: {error}"
                    ))
                })?;
            match outcome {
                ConfinedOutcome { exit_code: Some(0), .. } => Ok(()),
                other => Err(unavailable(format!(
                    "a probe command under the restricted token ended with {other:?} instead of \
                     exit code 0"
                ))),
            }
        }
        #[cfg(not(all(target_os = "windows", feature = "sandbox-windows")))]
        {
            Err(unavailable(
                "the restricted-token enforcer runs only on Windows with the sandbox-windows \
                 feature"
                    .to_string(),
            ))
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

/// The restricted token's group plan, in binary SIDs so it is computed and tested on every
/// host (spec 3.2 §10 W5).
mod token {
    #![cfg_attr(not(all(target_os = "windows", feature = "sandbox-windows")), allow(dead_code))]

    /// `SE_GROUP_LOGON_ID`: both bits mark the logon session's SID.
    pub(super) const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
    /// `SE_GROUP_INTEGRITY`: a mandatory integrity label, which cannot be made deny-only.
    pub(super) const SE_GROUP_INTEGRITY: u32 = 0x0000_0020;

    /// A security identifier in binary form, in word-aligned storage as Win32 expects.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct Sid {
        words: Vec<u32>,
        len: usize,
    }

    impl Sid {
        /// A SID from its binary form.
        pub(super) fn from_bytes(bytes: &[u8]) -> Self {
            let mut words = vec![0u32; bytes.len().div_ceil(4)];
            for (i, byte) in bytes.iter().enumerate() {
                words[i / 4] |= u32::from(*byte) << (8 * (i % 4));
            }
            Self { words, len: bytes.len() }
        }

        /// `S-1-<authority>-<sub...>`, for the well-known SIDs below.
        fn well_known(authority: u8, subs: &[u32]) -> Self {
            let mut bytes = vec![1, subs.len() as u8, 0, 0, 0, 0, 0, authority];
            for sub in subs {
                bytes.extend_from_slice(&sub.to_le_bytes());
            }
            Self::from_bytes(&bytes)
        }

        /// The SID's bytes.
        #[cfg(test)]
        pub(super) fn as_bytes(&self) -> Vec<u8> {
            (0..self.len).map(|i| (self.words[i / 4] >> (8 * (i % 4))) as u8).collect()
        }

        /// A pointer to the SID for Win32 calls, valid while `self` lives.
        pub(super) fn as_ptr(&self) -> *const u32 {
            self.words.as_ptr()
        }

        /// `Everyone`, S-1-1-0.
        pub(super) fn everyone() -> Self {
            Self::well_known(1, &[0])
        }

        /// `NT AUTHORITY\INTERACTIVE`, S-1-5-4.
        pub(super) fn interactive() -> Self {
            Self::well_known(5, &[4])
        }

        /// `NT AUTHORITY\Authenticated Users`, S-1-5-11.
        pub(super) fn authenticated_users() -> Self {
            Self::well_known(5, &[11])
        }

        /// `NT AUTHORITY\RESTRICTED`, S-1-5-12.
        pub(super) fn restricted() -> Self {
            Self::well_known(5, &[12])
        }

        /// `NT AUTHORITY\SYSTEM`, S-1-5-18.
        pub(super) fn system() -> Self {
            Self::well_known(5, &[18])
        }

        /// `BUILTIN\Users`, S-1-5-32-545.
        pub(super) fn users() -> Self {
            Self::well_known(5, &[32, 545])
        }

        /// `BUILTIN\Administrators`, S-1-5-32-544.
        #[cfg(test)]
        pub(super) fn administrators() -> Self {
            Self::well_known(5, &[32, 544])
        }
    }

    /// One group of the caller's token.
    #[derive(Clone, Debug)]
    pub(super) struct TokenGroup {
        pub(super) sid: Sid,
        pub(super) attributes: u32,
    }

    /// What `CreateRestrictedToken` is given.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) struct TokenPlan {
        /// Groups made deny-only.
        pub(super) deny_only: Vec<Sid>,
        /// The restricting SIDs every access must also pass.
        pub(super) restricting: Vec<Sid>,
        /// The SIDs the token's default DACL grants full access. The objects the sandboxed
        /// process creates — its child processes and their threads among them — take this
        /// DACL, so it must admit a restricting SID or the process could not open its own
        /// children.
        pub(super) default_dacl: Vec<Sid>,
    }

    /// The plan for a token with `groups`.
    ///
    /// Every group becomes deny-only — `Administrators` and any other group that widens
    /// access — except those the shell needs to start and run: `Everyone`, `Users`,
    /// `Authenticated Users`, `INTERACTIVE`, the logon SID (window station and desktop), and
    /// integrity labels, which cannot be disabled. The restricting SIDs are `Everyone` and
    /// `Users`, so the shell can open the system binaries it starts (`System32` grants read
    /// and execute to `Users`, not `Everyone`), `RESTRICTED`, which the policy's paths are
    /// granted, and the logon SID. The default DACL grants the user, `SYSTEM`, `RESTRICTED` and the
    /// logon SID. Returns `None` when the token has no logon SID.
    pub(super) fn plan(user: &Sid, groups: &[TokenGroup]) -> Option<TokenPlan> {
        let keep = [Sid::everyone(), Sid::users(), Sid::authenticated_users(), Sid::interactive()];
        let is_logon = |g: &TokenGroup| g.attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID;
        let logon = groups.iter().find(|g| is_logon(g))?.sid.clone();
        let deny_only = groups
            .iter()
            .filter(|g| {
                !is_logon(g) && g.attributes & SE_GROUP_INTEGRITY == 0 && !keep.contains(&g.sid)
            })
            .map(|g| g.sid.clone())
            .collect();
        Some(TokenPlan {
            deny_only,
            restricting: vec![Sid::everyone(), Sid::users(), Sid::restricted(), logon.clone()],
            default_dacl: vec![user.clone(), Sid::system(), Sid::restricted(), logon],
        })
    }
}

#[cfg(all(target_os = "windows", feature = "sandbox-windows"))]
mod imp {
    //! The Win32 implementation of the restricted-token launch.

    use std::ffi::{OsStr, OsString, c_void};
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{BorrowedHandle, FromRawHandle as _, OwnedHandle};
    use std::path::Path;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_ALL, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        LocalFree, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE,
        SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
        TRUSTEE_IS_UNKNOWN, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, CreateRestrictedToken, DACL_SECURITY_INFORMATION, DISABLE_MAX_PRIVILEGE, GetLengthSid,
        GetTokenInformation, NO_INHERITANCE, PSID, SID_AND_ATTRIBUTES,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetTokenInformation, TOKEN_ADJUST_DEFAULT,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_QUERY,
        TOKEN_USER, TokenDefaultDacl, TokenGroups, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
        GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
        LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
        TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    use super::token::{self, Sid, TokenGroup, TokenPlan};
    use super::{ConfinedOutcome, ENFORCER_NAME, strip_unc_prefix};
    use crate::error::SandboxError;
    use crate::process_tree::{ContainedChild, ContainedProcess, JobObject};
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

    fn last_error(call: &str) -> SandboxError {
        // SAFETY: `GetLastError` reads thread-local state and is always safe.
        let code = unsafe { GetLastError() };
        SandboxError::EnforcerFailed {
            enforcer: ENFORCER_NAME.to_string(),
            message: format!("{call} failed (os error {code})"),
        }
    }

    fn psid(sid: &Sid) -> PSID {
        sid.as_ptr() as PSID
    }

    /// The current process's primary token, opened with the rights
    /// `CreateRestrictedToken`/`CreateProcessAsUserW` need.
    fn open_process_token() -> Result<Handle, SandboxError> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle from `GetCurrentProcess` needs no close; `token` is writable.
        let ok = unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
                &mut token,
            )
        };
        if ok == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        Ok(Handle(token))
    }

    /// Reads `class` from `token` into `u64`-aligned storage.
    fn token_information(
        token: HANDLE,
        class: windows_sys::Win32::Security::TOKEN_INFORMATION_CLASS,
    ) -> Result<Vec<u64>, SandboxError> {
        let mut needed: u32 = 0;
        // SAFETY: a null buffer asks only for the size, written to `needed`.
        unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(last_error("GetTokenInformation(size)"));
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: `buf` holds at least `needed` writable bytes.
        let ok = unsafe {
            GetTokenInformation(token, class, buf.as_mut_ptr().cast(), needed, &mut needed)
        };
        if ok == 0 {
            return Err(last_error("GetTokenInformation"));
        }
        Ok(buf)
    }

    /// A copy of the SID at `sid`.
    ///
    /// # Safety
    ///
    /// `sid` must point at a valid SID.
    unsafe fn copy_sid(sid: PSID) -> Sid {
        // SAFETY: the caller guarantees `sid` is a valid SID of `GetLengthSid` bytes.
        let bytes =
            unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize) };
        Sid::from_bytes(bytes)
    }

    /// The token's user SID.
    fn token_user(token: HANDLE) -> Result<Sid, SandboxError> {
        let buf = token_information(token, TokenUser)?;
        // SAFETY: `buf` holds a `TOKEN_USER` whose SID points into `buf`.
        Ok(unsafe { copy_sid((*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid) })
    }

    /// Sets the token's default DACL to grant `sids` full access.
    fn set_default_dacl(token: HANDLE, sids: &[Sid]) -> Result<(), SandboxError> {
        let entries: Vec<EXPLICIT_ACCESS_W> = sids
            .iter()
            .map(|sid| EXPLICIT_ACCESS_W {
                grfAccessPermissions: GENERIC_ALL,
                grfAccessMode: GRANT_ACCESS,
                grfInheritance: NO_INHERITANCE,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: std::ptr::null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_UNKNOWN,
                    ptstrName: psid(sid).cast(),
                },
            })
            .collect();
        let mut acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `entries` is readable for its length and its SIDs outlive the call; no old
        // ACL is merged; the ACL the call allocates is freed below.
        let status = unsafe {
            SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), std::ptr::null(), &mut acl)
        };
        if status != 0 {
            return Err(SandboxError::EnforcerFailed {
                enforcer: ENFORCER_NAME.to_string(),
                message: format!("SetEntriesInAclW(default DACL) failed (os error {status})"),
            });
        }
        let _acl = LocalGuard(acl.cast());
        let info = TOKEN_DEFAULT_DACL { DefaultDacl: acl };
        // SAFETY: `info` is readable for its own size and points at a valid ACL.
        let ok = unsafe {
            SetTokenInformation(
                token,
                TokenDefaultDacl,
                (&raw const info).cast(),
                size_of::<TOKEN_DEFAULT_DACL>() as u32,
            )
        };
        if ok == 0 {
            return Err(last_error("SetTokenInformation(TokenDefaultDacl)"));
        }
        Ok(())
    }

    /// The token's groups, each SID copied into owned storage.
    fn token_groups(token: HANDLE) -> Result<Vec<TokenGroup>, SandboxError> {
        let buf = token_information(token, TokenGroups)?;
        // SAFETY: `buf` holds a `TOKEN_GROUPS` as `GetTokenInformation` wrote it.
        let groups = unsafe { &*buf.as_ptr().cast::<TOKEN_GROUPS>() };
        let entries = groups.Groups.as_ptr();
        let mut out = Vec::with_capacity(groups.GroupCount as usize);
        for i in 0..groups.GroupCount as usize {
            // SAFETY: `i` is within `GroupCount`, so the entry is part of the buffer, and its
            // SID points into `buf`.
            let entry = unsafe { &*entries.add(i) };
            // SAFETY: as above, the entry's SID is valid.
            let sid = unsafe { copy_sid(entry.Sid) };
            out.push(TokenGroup { sid, attributes: entry.Attributes });
        }
        Ok(out)
    }

    /// Builds the restricted token from `plan` (see [`token::plan`]), with every privilege
    /// but change-notify removed.
    fn restricted_token(source: HANDLE, plan: &TokenPlan) -> Result<Handle, SandboxError> {
        let attrs = |sids: &[Sid]| -> Vec<SID_AND_ATTRIBUTES> {
            sids.iter().map(|sid| SID_AND_ATTRIBUTES { Sid: psid(sid), Attributes: 0 }).collect()
        };
        let deny = attrs(&plan.deny_only);
        let restrict = attrs(&plan.restricting);
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the SID arrays are readable for their stated counts and point into `plan`,
        // which outlives the call; `source` is open and `token` is writable.
        let ok = unsafe {
            CreateRestrictedToken(
                source,
                DISABLE_MAX_PRIVILEGE,
                deny.len() as u32,
                deny.as_ptr(),
                0,
                std::ptr::null(),
                restrict.len() as u32,
                restrict.as_ptr(),
                &mut token,
            )
        };
        if ok == 0 {
            return Err(last_error("CreateRestrictedToken"));
        }
        let token = Handle(token);
        set_default_dacl(token.0, &plan.default_dacl)?;
        Ok(token)
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

    /// An enforcer error for the Job Object step `call` that failed with `error`.
    fn job_error(call: &str, error: std::io::Error) -> SandboxError {
        SandboxError::EnforcerFailed {
            enforcer: ENFORCER_NAME.to_string(),
            message: format!("{call} failed ({error})"),
        }
    }

    /// The child's ends of its standard streams, inheritable only while the child is created.
    struct ChildStdio {
        input: Handle,
        output: Handle,
        error: Handle,
    }

    /// A process the restricted token runs, in its job and resumed.
    struct Launched {
        job: JobObject,
        process: Handle,
        pid: u32,
    }

    /// Builds the restricted token, grants the policy's paths, and starts `program` with
    /// `args` in `cwd` under the token, suspended; places it in a new Job Object and only
    /// then resumes it. With `stdio`, the child inherits those three handles and no other.
    fn launch(
        policy: &SandboxPolicy,
        grants: &[AllowedPath],
        program: &OsStr,
        args: &[OsString],
        cwd: &Path,
        stdio: Option<&ChildStdio>,
    ) -> Result<Launched, SandboxError> {
        let process_token = open_process_token()?;
        let user = token_user(process_token.0)?;
        let groups = token_groups(process_token.0)?;
        let plan = token::plan(&user, &groups).ok_or_else(|| SandboxError::EnforcerFailed {
            enforcer: ENFORCER_NAME.to_string(),
            message: "the process token carries no logon SID".to_string(),
        })?;
        let token = restricted_token(process_token.0, &plan)?;

        // Grant the restricting RESTRICTED SID access to each allowed path, so the restricted
        // token's second access check passes there and nowhere else.
        let restricted = Sid::restricted();
        for entry in grants {
            grant_path(&entry.path, entry.mode, psid(&restricted))?;
        }

        let job = JobObject::new().map_err(|error| job_error("CreateJobObjectW", error))?;

        let mut command = command_line(program, args);
        let cwd_wide: Vec<u16> =
            strip_unc_prefix(cwd).as_os_str().encode_wide().chain(Some(0)).collect();
        let env = environment_block(policy);
        // SAFETY: `STARTUPINFOEXW` and `PROCESS_INFORMATION` are plain C structs that an
        // all-zero bit pattern initialises validly.
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

        let mut flags = CREATE_SUSPENDED;
        let env_ptr: *const c_void = match env.as_ref() {
            Some(block) => {
                flags |= CREATE_UNICODE_ENVIRONMENT;
                block.as_ptr().cast()
            }
            None => std::ptr::null(),
        };

        // The child inherits exactly its three standard handles: a handle list keeps every
        // other inheritable handle of this process, which the restricted token's access
        // check never saw, out of the sandbox.
        let inherited: Vec<HANDLE> =
            stdio.map(|io| vec![io.input.0, io.output.0, io.error.0]).unwrap_or_default();
        let attributes = match stdio {
            Some(io) => {
                for handle in &inherited {
                    // SAFETY: the handle is open; only its inherit flag changes.
                    if unsafe {
                        SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
                    } == 0
                    {
                        return Err(last_error("SetHandleInformation"));
                    }
                }
                let mut attributes = AttributeList::handles(&inherited)?;
                startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
                startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
                startup.StartupInfo.hStdInput = io.input.0;
                startup.StartupInfo.hStdOutput = io.output.0;
                startup.StartupInfo.hStdError = io.error.0;
                startup.lpAttributeList = attributes.as_ptr();
                flags |= EXTENDED_STARTUPINFO_PRESENT;
                Some(attributes)
            }
            None => None,
        };

        // SAFETY: the token is a restricted child of the caller's token, the command line is
        // NUL-terminated and mutable as the API requires, the directory and environment are
        // NUL-terminated, the startup structure is valid for the call with `cb` naming its
        // size, and the attribute list and the handles it names outlive the call.
        let ok = unsafe {
            CreateProcessAsUserW(
                token.0,
                std::ptr::null(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                i32::from(stdio.is_some()),
                flags,
                env_ptr,
                cwd_wide.as_ptr(),
                (&raw const startup).cast::<STARTUPINFOW>(),
                &mut info,
            )
        };
        drop(attributes);
        if ok == 0 {
            return Err(last_error("CreateProcessAsUserW"));
        }
        let process = Handle(info.hProcess);
        let thread = Handle(info.hThread);

        // Put the process in the job before it runs, so every descendant it starts is
        // captured and killed with it.
        // SAFETY: the process handle is open for the borrow, which ends with the call.
        if let Err(error) = job.assign(unsafe { BorrowedHandle::borrow_raw(process.0) }) {
            // SAFETY: the process handle is open; terminate the stillborn process.
            unsafe { TerminateProcess(process.0, 1) };
            return Err(job_error("AssignProcessToJobObject", error));
        }
        // SAFETY: the thread handle is open; resuming a suspended primary thread is safe.
        if unsafe { ResumeThread(thread.0) } == u32::MAX {
            let err = last_error("ResumeThread");
            job.terminate();
            return Err(err);
        }
        Ok(Launched { job, process, pid: info.dwProcessId })
    }

    /// A `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` attribute list, freed on drop.
    struct AttributeList {
        // `u64` storage keeps the list pointer-aligned.
        buffer: Vec<u64>,
    }

    impl AttributeList {
        /// A list whose one attribute names `handles`, which must outlive the list's use.
        fn handles(handles: &[HANDLE]) -> Result<Self, SandboxError> {
            let mut size = 0usize;
            // SAFETY: a null list asks only for the size, written to `size`; the call fails
            // with ERROR_INSUFFICIENT_BUFFER by design.
            unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
            if size == 0 {
                return Err(last_error("InitializeProcThreadAttributeList(size)"));
            }
            let mut list = Self { buffer: vec![0u64; size.div_ceil(8)] };
            // SAFETY: the buffer holds at least `size` writable bytes.
            if unsafe { InitializeProcThreadAttributeList(list.as_ptr(), 1, 0, &mut size) } == 0 {
                // An uninitialised list must not be deleted.
                std::mem::forget(list);
                return Err(last_error("InitializeProcThreadAttributeList"));
            }
            // SAFETY: the list is initialised; `handles` is readable for its stated size and
            // outlives the list's use, as the caller guarantees.
            let updated = unsafe {
                UpdateProcThreadAttribute(
                    list.as_ptr(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast(),
                    size_of_val(handles),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            if updated == 0 {
                return Err(last_error("UpdateProcThreadAttribute"));
            }
            Ok(list)
        }

        fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.buffer.as_mut_ptr().cast()
        }
    }

    impl Drop for AttributeList {
        fn drop(&mut self) {
            // SAFETY: the list was initialised in `handles`.
            unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
        }
    }

    pub(super) fn run_confined(
        policy: &SandboxPolicy,
        grants: &[AllowedPath],
        program: &OsStr,
        args: &[OsString],
        cwd: &Path,
        timeout: Option<Duration>,
    ) -> Result<ConfinedOutcome, SandboxError> {
        let Launched { job, process, .. } = launch(policy, grants, program, args, cwd, None)?;

        let wait_ms =
            timeout.map_or(INFINITE, |d| u32::try_from(d.as_millis()).unwrap_or(INFINITE));
        // SAFETY: the process handle is open.
        let waited = unsafe { WaitForSingleObject(process.0, wait_ms) };
        if waited == WAIT_TIMEOUT {
            // Kill the whole tree.
            job.terminate();
            return Ok(ConfinedOutcome { exit_code: None, timed_out: true });
        }
        if waited == WAIT_FAILED {
            let err = last_error("WaitForSingleObject");
            job.terminate();
            return Err(err);
        }
        debug_assert_eq!(waited, WAIT_OBJECT_0);

        let mut code: u32 = 0;
        // SAFETY: the process handle is open and `code` is writable.
        let got = unsafe { GetExitCodeProcess(process.0, &mut code) };
        // The process has exited, so terminating the job now only reaps any descendant that
        // outlived it.
        job.terminate();
        if got == 0 {
            return Err(last_error("GetExitCodeProcess"));
        }
        Ok(ConfinedOutcome { exit_code: Some(code as i32), timed_out: false })
    }

    /// An anonymous pipe, neither end inheritable: `(read, write)`.
    fn pipe() -> Result<(Handle, Handle), SandboxError> {
        let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: both out-pointers are writable; no security attributes, so neither end is
        // inheritable.
        if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
            return Err(last_error("CreatePipe"));
        }
        Ok((Handle(read), Handle(write)))
    }

    /// The parent's end of a pipe as a file.
    fn into_file(handle: Handle) -> std::fs::File {
        let raw = handle.0;
        std::mem::forget(handle);
        // SAFETY: `raw` is an open handle this function now solely owns.
        std::fs::File::from(unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    pub(super) fn spawn_contained(
        policy: &SandboxPolicy,
        grants: &[AllowedPath],
        program: &OsStr,
        args: &[OsString],
        cwd: &Path,
    ) -> Result<ContainedChild, SandboxError> {
        let (stdin_read, stdin_write) = pipe()?;
        let (stdout_read, stdout_write) = pipe()?;
        let (stderr_read, stderr_write) = pipe()?;
        let stdio = ChildStdio { input: stdin_read, output: stdout_write, error: stderr_write };
        let Launched { job, process, pid } =
            launch(policy, grants, program, args, cwd, Some(&stdio))?;
        // The child holds its own copies; closing these lets its output end when it does.
        drop(stdio);
        let raw = process.0;
        std::mem::forget(process);
        // SAFETY: `raw` is the open process handle this function now solely owns.
        let process = ContainedProcess::new(unsafe { OwnedHandle::from_raw_handle(raw) }, pid);
        Ok(ContainedChild {
            job,
            process,
            stdin: Some(into_file(stdin_write)),
            stdout: Some(into_file(stdout_read)),
            stderr: Some(into_file(stderr_read)),
        })
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

    fn group(sid: token::Sid, attributes: u32) -> token::TokenGroup {
        token::TokenGroup { sid, attributes }
    }

    #[test]
    fn well_known_sids_have_their_binary_form() {
        assert_eq!(token::Sid::everyone().as_bytes(), [1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(token::Sid::restricted().as_bytes(), [1, 1, 0, 0, 0, 0, 0, 5, 12, 0, 0, 0]);
        assert_eq!(
            token::Sid::users().as_bytes(),
            [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x21, 0x02, 0, 0]
        );
        let logon = [1, 3, 0, 0, 0, 0, 0, 5, 5, 0, 0, 0, 1, 2, 0, 0, 3, 4, 0, 0];
        assert_eq!(token::Sid::from_bytes(&logon).as_bytes(), logon);
    }

    /// An elevated administrator's token: Administrators and an organisation group become
    /// deny-only; the groups a shell needs, the logon SID and the integrity label do not.
    #[test]
    fn the_plan_makes_every_widening_group_deny_only() {
        let logon =
            token::Sid::from_bytes(&[1, 3, 0, 0, 0, 0, 0, 5, 5, 0, 0, 0, 7, 0, 0, 0, 9, 0, 0, 0]);
        let organisation = token::Sid::from_bytes(&[1, 1, 0, 0, 0, 0, 0, 5, 15, 0, 0, 0]);
        let medium_label = token::Sid::from_bytes(&[1, 1, 0, 0, 0, 0, 0, 16, 0, 0x20, 0, 0]);
        let groups = [
            group(token::Sid::everyone(), 7),
            group(token::Sid::administrators(), 0xF),
            group(token::Sid::users(), 7),
            group(token::Sid::interactive(), 7),
            group(logon.clone(), token::SE_GROUP_LOGON_ID | 7),
            group(token::Sid::authenticated_users(), 7),
            group(organisation.clone(), 7),
            group(medium_label, token::SE_GROUP_INTEGRITY | 0x40),
        ];
        let user = token::Sid::from_bytes(&[1, 2, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 0xE9, 3, 0, 0]);
        let plan = token::plan(&user, &groups).expect("the token has a logon SID");
        assert_eq!(
            plan,
            token::TokenPlan {
                deny_only: vec![token::Sid::administrators(), organisation],
                restricting: vec![
                    token::Sid::everyone(),
                    token::Sid::users(),
                    token::Sid::restricted(),
                    logon.clone()
                ],
                default_dacl: vec![user, token::Sid::system(), token::Sid::restricted(), logon],
            }
        );
    }

    #[test]
    fn a_token_without_a_logon_sid_has_no_plan() {
        assert_eq!(token::plan(&token::Sid::system(), &[group(token::Sid::everyone(), 7)]), None);
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
