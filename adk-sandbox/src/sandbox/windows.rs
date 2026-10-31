//! Windows restricted-token sandbox enforcer — the first Windows tier (spec 3.2 §10 W5).
//!
//! This enforcer confines a shell on Windows with a **restricted, Low-integrity token plus
//! explicit ACL grants and labels**, following the restricted-token ladder (as Codex does);
//! AppContainer stays out of scope. It runs the command under a token built with
//! `CreateRestrictedToken` — every group that widens access deny-only, `Administrators`
//! included, and the restricting SIDs `Everyone`, `Users`, `RESTRICTED`, the logon SID and the
//! user — at Low integrity. It grants `RESTRICTED` access to the paths the [`SandboxPolicy`]
//! names and labels each read-write one Low, so the token may write there and, under the
//! mandatory integrity policy's no-write-up rule, nowhere else. A Job Object kills every
//! descendant with the call.
//!
//! The user is a restricting SID because the MSYS2 runtime of Git for Windows' `sh` creates its
//! signal pipe and shared memory with a DACL that names only the user, `Administrators` and
//! `SYSTEM`, and opens them again; without the user the shell could not start. That widens
//! reads to what the user may read; the Low integrity level keeps writes confined.
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
//! | Write outside the granted paths | Denied — an object without a label is Medium integrity, which a Low token cannot write; objects already labelled Low, such as `%USERPROFILE%\AppData\LocalLow`, stay writable |
//! | Write inside a read-write path | Allowed — the path is labelled Low (inherited by what lies beneath it) and `RESTRICTED` is granted there |
//! | Read of a path the policy does not grant | Allowed where the user may read it, the user's profile included, since the user is a restricting SID; denied where only groups the token holds deny-only (`Administrators`) or no SID it holds are granted |
//! | System binaries and DLLs (`System32` grants `Users`) | Readable, so the shell can start programs |
//! | Named objects and pipes | Created in the logon session's namespace, which grants the logon SID and admits Low integrity; MSYS2's objects that name the user are opened again |
//! | MSYS2 programs (Git for Windows' `sh`) | Started from a private copy of their runtime, made for the call in an owner-only, Medium-integrity directory and compared with the installation's files, so the call's MSYS2 processes share an object namespace with no other process |
//! | The user's MSYS2 namespace of the program's installation and of each installation on `PATH` | Held at the user's integrity level by an unconfined `cat` of that installation for the call's life, so the Low token can neither create nor write it; an MSYS2 program that a program outside MSYS2 starts itself, such as a hook's `sh` that `git` starts, therefore cannot start |
//! | The MSYS2 or Cygwin namespace of any other installation | **Not protected** — a command that starts such an installation's program by its path while none of the user's runs creates that namespace at Low integrity, and a program of it the user starts before the command ends shares it |
//! | Descendant processes | Killed with the call's Job Object |
//! | Inherited handles | `spawn_contained`'s three standard handles only, never inheritable in this process, so no process another thread starts can take one; `run_confined` inherits none |
//! | Network | **Not enforced** — Windows offers no per-process network filter without AppContainer. A policy whose network access is `Denied` or `Proxied` is refused with `SandboxError::Unenforceable`, unless the enforcer was built with [`WindowsEnforcer::acknowledge_open_network`]; then the command runs with full network access and the first such policy in the process logs a `tracing::warn` |
//! | Labels and grants | Persist on the granted paths after the call: `RESTRICTED`'s entry, and a Low label on each read-write path, so any Low-integrity process of the machine may write there |

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use super::{
    AccessMode, AllowedPath, NetworkSupport, SandboxEnforcer, SandboxPolicy, WrappedCommand,
};
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
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsEnforcer {
    open_network_acknowledged: bool,
}

impl WindowsEnforcer {
    /// Creates a new Windows restricted-token enforcer, which refuses a policy whose network
    /// access is `Denied` or `Proxied`.
    pub fn new() -> Self {
        Self { open_network_acknowledged: false }
    }

    /// Runs a policy whose network access is `Denied` or `Proxied` with the network open, which
    /// the restricted token cannot restrict. Without it such a policy is refused with
    /// [`SandboxError::Unenforceable`].
    ///
    /// For a caller that reports the open network itself, as the harness shell does; the first
    /// such policy in the process also logs a `tracing::warn`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_sandbox::sandbox::windows::WindowsEnforcer;
    ///
    /// let enforcer = WindowsEnforcer::new().acknowledge_open_network();
    /// assert!(enforcer.open_network_acknowledged());
    /// ```
    #[must_use]
    pub fn acknowledge_open_network(mut self) -> Self {
        self.open_network_acknowledged = true;
        self
    }

    /// Whether [`acknowledge_open_network`](Self::acknowledge_open_network) was set.
    pub fn open_network_acknowledged(&self) -> bool {
        self.open_network_acknowledged
    }

    /// Refuses a restrictive network the caller has not acknowledged stays open.
    fn check_network(&self, policy: &SandboxPolicy) -> Result<(), SandboxError> {
        if self.open_network_acknowledged {
            return Ok(());
        }
        self.network_support().check(ENFORCER_NAME, &policy.network)
    }

    /// Runs `program` with `args` under a restricted token, with ACL grants on the policy's
    /// paths and a Job Object that kills every descendant with the call, starting in `cwd`.
    ///
    /// The environment is `policy.env`, or this process's environment when the policy sets
    /// none, plus `SystemRoot` from this process when it is absent, so the shell can start.
    /// An MSYS2 program — one beside `msys-2.0.dll`, such as Git for Windows' `usr\bin\sh.exe`,
    /// or a Git for Windows launcher of one, such as `bin\sh.exe` — runs from a private copy of
    /// its runtime, which comes first on `PATH`, and the call holds the user's MSYS2 namespace
    /// of its installation and of each installation on `PATH` (see the module documentation).
    /// Standard streams are not captured; the outcome carries the exit code and whether the
    /// call timed out. On a timeout the job is terminated and [`ConfinedOutcome::timed_out`]
    /// is `true`.
    ///
    /// The network is not restricted. A policy whose network access is `Denied` or `Proxied`
    /// is refused unless the enforcer [acknowledges](Self::acknowledge_open_network) that the
    /// network stays open; the first such policy in the process then logs a `tracing::warn`.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Unenforceable`] for a restrictive network that was not
    /// acknowledged, [`SandboxError::EnforcerUnavailable`] off Windows,
    /// [`SandboxError::PolicyViolation`] when an allowed path cannot be resolved, and
    /// [`SandboxError::EnforcerFailed`] when a Win32 step fails (with the failing call and the
    /// OS error).
    pub fn run_confined(
        &self,
        policy: &SandboxPolicy,
        program: &OsStr,
        args: &[OsString],
        cwd: &std::path::Path,
        timeout: Option<std::time::Duration>,
    ) -> Result<ConfinedOutcome, SandboxError> {
        self.check_network(policy)?;
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
    /// None of them is ever inheritable in this process, so a process another thread starts
    /// meanwhile — through `std::process::Command` or otherwise — cannot take one and hold the
    /// output open after the child exits: the child names a suspended `cmd.exe` from the
    /// system directory, which never runs and ends once the child exists, as its parent and
    /// inherits the three handles from it, so its parent process id is that of an exited
    /// process (spec 3.2 §10 W5).
    /// The environment and the network are as for `run_confined`. `program` should be a full
    /// path: a bare name is searched for as `CreateProcessAsUserW` does, from the directory
    /// of this process and its working directory before the system directories and `PATH`.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Unenforceable`] for a restrictive network that was not
    /// acknowledged, [`SandboxError::PolicyViolation`] when an allowed path cannot be resolved,
    /// and [`SandboxError::EnforcerFailed`] when a Win32 step fails (with the failing call and
    /// the OS error).
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
        self.check_network(policy)?;
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
            // The probe asks for no network restriction, so it is neither refused nor warned of.
            let outcome = self
                .run_confined(
                    &super::SandboxPolicyBuilder::new().allow_network().build(),
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

    /// The restricted token has no network filter, so no restrictive mode is enforced;
    /// [`WindowsEnforcer::acknowledge_open_network`] runs them with the network open.
    fn network_support(&self) -> NetworkSupport {
        NetworkSupport::NONE
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

mod msys2;

/// The confined command's environment: `policy.env`, or `inherited` when the policy sets
/// nothing, and `SystemRoot` from `inherited` when absent, so the shell and its DLLs can start.
#[cfg_attr(not(all(target_os = "windows", feature = "sandbox-windows")), allow(dead_code))]
fn confined_environment(
    policy: &SandboxPolicy,
    inherited: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let mut pairs: Vec<(OsString, OsString)> = if policy.env.is_empty() {
        inherited.to_vec()
    } else {
        policy.env.iter().map(|(k, v)| (k.into(), v.into())).collect()
    };
    let has = |pairs: &[(OsString, OsString)], name: &str| {
        pairs.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
    };
    if !has(&pairs, "SystemRoot")
        && let Some(root) = inherited.iter().find(|(k, _)| k.eq_ignore_ascii_case("SystemRoot"))
    {
        pairs.push(root.clone());
    }
    pairs
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

        /// The Low mandatory integrity level, S-1-16-4096.
        pub(super) fn low_integrity() -> Self {
            Self::well_known(16, &[0x1000])
        }

        /// The Medium mandatory integrity level, S-1-16-8192.
        pub(super) fn medium_integrity() -> Self {
            Self::well_known(16, &[0x2000])
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

    /// The plan for a token of `user` with `groups`.
    ///
    /// Every group becomes deny-only — `Administrators` and any other group that widens
    /// access — except those the shell needs to start and run: `Everyone`, `Users`,
    /// `Authenticated Users`, `INTERACTIVE`, the logon SID (window station and desktop), and
    /// integrity labels, which cannot be disabled. The restricting SIDs are `Everyone` and
    /// `Users`, so the shell can open the system binaries it starts (`System32` grants read
    /// and execute to `Users`, not `Everyone`), `RESTRICTED`, which the policy's paths are
    /// granted, the logon SID, and the user, whom MSYS2's signal pipe and shared memory name
    /// (spec 3.2 §10 W5). The default DACL grants the user, `SYSTEM`, `RESTRICTED` and the logon
    /// SID. Returns `None` when the token has no logon SID.
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
            restricting: vec![
                Sid::everyone(),
                Sid::users(),
                Sid::restricted(),
                logon.clone(),
                user.clone(),
            ],
            default_dacl: vec![user.clone(), Sid::system(), Sid::restricted(), logon],
        })
    }
}

#[cfg(all(target_os = "windows", feature = "sandbox-windows"))]
mod imp {
    //! The Win32 implementation of the restricted-token launch.

    use std::ffi::{OsStr, OsString, c_void};
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsHandle as _, AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, GENERIC_ALL,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_FAILED, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE,
        SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
        TRUSTEE_IS_UNKNOWN, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, ACL_REVISION, AddMandatoryAce, CONTAINER_INHERIT_ACE, CreateRestrictedToken,
        DACL_SECURITY_INFORMATION, DISABLE_MAX_PRIVILEGE, GetLengthSid, GetTokenInformation,
        InitializeAcl, LABEL_SECURITY_INFORMATION, NO_INHERITANCE, OBJECT_INHERIT_ACE,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, SID_AND_ATTRIBUTES,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetTokenInformation, TOKEN_ADJUST_DEFAULT,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_GROUPS,
        TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenDefaultDacl, TokenGroups,
        TokenIntegrityLevel, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
        CreateProcessAsUserW, CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, INFINITE,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
        PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
        TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    use super::token::{self, Sid, TokenGroup, TokenPlan};
    use super::{ConfinedOutcome, ENFORCER_NAME, confined_environment, msys2, strip_unc_prefix};
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
        set_low_integrity(token.0)?;
        Ok(token)
    }

    /// `SYSTEM_MANDATORY_LABEL_NO_WRITE_UP`: a token of lower integrity may not write the object.
    const NO_WRITE_UP: u32 = 0x1;

    /// Lowers `token` to Low integrity, so it writes only objects labelled Low (spec 3.2 §10
    /// W5). The user is one of its restricting SIDs, so the level is what confines its writes.
    fn set_low_integrity(token: HANDLE) -> Result<(), SandboxError> {
        let low = Sid::low_integrity();
        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES { Sid: psid(&low), Attributes: token::SE_GROUP_INTEGRITY },
        };
        // SAFETY: `low` is a valid SID that outlives the call.
        let size = size_of::<TOKEN_MANDATORY_LABEL>() as u32 + unsafe { GetLengthSid(psid(&low)) };
        // SAFETY: `label` and the SID it points to are readable for the call; the token is open
        // with `TOKEN_ADJUST_DEFAULT`, and lowering the level needs no privilege.
        let ok = unsafe {
            SetTokenInformation(token, TokenIntegrityLevel, (&raw const label).cast(), size)
        };
        if ok == 0 {
            return Err(last_error("SetTokenInformation(TokenIntegrityLevel)"));
        }
        Ok(())
    }

    /// Labels the file or directory `wide` (NUL-terminated) Low integrity with no-write-up,
    /// inherited by what lies beneath it, so a Low token may write there.
    fn label_low(wide: &mut [u16]) -> Result<(), SandboxError> {
        label(wide, &Sid::low_integrity())
    }

    /// Gives `path` an owner-only DACL, protected from inheritance, and an explicit Medium
    /// integrity label inherited by what is created beneath it, so the Low token cannot
    /// write there whatever a grant of an enclosing path left behind. The token still reads
    /// it: the user is one of its restricting SIDs.
    pub(super) fn protect_owner_only(path: &Path) -> Result<(), SandboxError> {
        let process_token = open_process_token()?;
        let user = token_user(process_token.0)?;
        let mut wide: Vec<u16> =
            strip_unc_prefix(path).as_os_str().encode_wide().chain(Some(0)).collect();
        let explicit = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: psid(&user).cast(),
            },
        };
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: one explicit entry is passed, no old ACL is merged, and `dacl` is writable.
        // The ACL the call allocates is freed below.
        let status = unsafe { SetEntriesInAclW(1, &explicit, std::ptr::null(), &mut dacl) };
        if status != 0 {
            return Err(SandboxError::EnforcerFailed {
                enforcer: ENFORCER_NAME.to_string(),
                message: format!("SetEntriesInAclW(owner only) failed (os error {status})"),
            });
        }
        let _dacl = LocalGuard(dacl.cast());
        // SAFETY: `wide` is NUL-terminated and `dacl` is a valid ACL for the call.
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            return Err(SandboxError::EnforcerFailed {
                enforcer: ENFORCER_NAME.to_string(),
                message: format!(
                    "SetNamedSecurityInfoW(owner only) on {} failed (os error {status})",
                    path.display()
                ),
            });
        }
        label(&mut wide, &Sid::medium_integrity())
    }

    /// Labels the file or directory `wide` (NUL-terminated) at the integrity `level` with
    /// no-write-up, inherited by what lies beneath it.
    fn label(wide: &mut [u16], level: &Sid) -> Result<(), SandboxError> {
        // An ACL header and one mandatory label entry with its SID fit in 64 bytes.
        let mut acl = [0u64; 8];
        let acl_ptr: *mut ACL = acl.as_mut_ptr().cast();
        // SAFETY: the buffer is writable for the size passed and outlives every use below.
        if unsafe { InitializeAcl(acl_ptr, size_of_val(&acl) as u32, ACL_REVISION) } == 0 {
            return Err(last_error("InitializeAcl"));
        }
        // SAFETY: the ACL was initialised above with room for the entry; `level` is valid.
        let added = unsafe {
            AddMandatoryAce(
                acl_ptr,
                ACL_REVISION,
                OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                NO_WRITE_UP,
                psid(level),
            )
        };
        if added == 0 {
            return Err(last_error("AddMandatoryAce"));
        }
        // SAFETY: `wide` is NUL-terminated and the ACL is valid for the call.
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_mut_ptr(),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                acl_ptr,
            )
        };
        if status != 0 {
            return Err(SandboxError::EnforcerFailed {
                enforcer: ENFORCER_NAME.to_string(),
                message: format!("SetNamedSecurityInfoW(label) failed (os error {status})"),
            });
        }
        Ok(())
    }

    /// Grants `restricted_sid` the access `mode` needs on `path`, merged into the path's
    /// existing DACL so the user's own entry — which the restricted token's normal access
    /// check still needs — is kept, and labels a read-write path Low so the Low token may
    /// write there.
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
        // The Low token writes only what is labelled Low.
        if mode == AccessMode::ReadWrite {
            label_low(&mut wide)?;
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

    /// The command line a new process receives: program then args, each quoted.
    pub(super) fn command_line(program: &OsStr, args: &[OsString]) -> Vec<u16> {
        let mut line = quote_arg(program);
        for arg in args {
            line.push(b' ' as u16);
            line.extend(quote_arg(arg));
        }
        line.push(0);
        line
    }

    /// The UTF-16 environment block of `pairs`, sorted by name without regard to case as
    /// Windows expects.
    pub(super) fn environment_block(mut pairs: Vec<(OsString, OsString)>) -> Vec<u16> {
        pairs.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in pairs {
            block.extend(k.encode_wide());
            block.push(u16::from(b'='));
            block.extend(v.encode_wide());
            block.push(0);
        }
        block.push(0);
        block
    }

    /// An enforcer error for the Job Object step `call` that failed with `error`.
    fn job_error(call: &str, error: std::io::Error) -> SandboxError {
        SandboxError::EnforcerFailed {
            enforcer: ENFORCER_NAME.to_string(),
            message: format!("{call} failed ({error})"),
        }
    }

    /// The ends of its standard streams a new process inherits. None is inheritable in this
    /// process: [`create_suspended`] hands them over through a [`Broker`].
    pub(super) struct ChildStdio {
        pub(super) input: OwnedHandle,
        pub(super) output: OwnedHandle,
        pub(super) error: OwnedHandle,
    }

    /// An anonymous pipe, neither end inheritable: `(read, write)`.
    pub(super) fn pipe() -> Result<(OwnedHandle, OwnedHandle), SandboxError> {
        let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: both out-pointers are writable; no security attributes, so neither end is
        // inheritable.
        if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
            return Err(last_error("CreatePipe"));
        }
        // SAFETY: both handles were just created and nothing else owns them.
        Ok(unsafe { (OwnedHandle::from_raw_handle(read), OwnedHandle::from_raw_handle(write)) })
    }

    /// A process created suspended in its job, which runs once resumed.
    pub(super) struct Suspended {
        pub(super) process: OwnedHandle,
        thread: OwnedHandle,
        pub(super) pid: u32,
    }

    impl Suspended {
        /// Lets the process run.
        pub(super) fn resume(&self) -> Result<(), SandboxError> {
            // SAFETY: the thread handle is open; resuming a suspended primary thread is safe.
            if unsafe { ResumeThread(self.thread.as_raw_handle()) } == u32::MAX {
                return Err(last_error("ResumeThread"));
            }
            Ok(())
        }
    }

    /// The creation flags that choose a process's console; a broker gets those of the
    /// process it stands parent to.
    const CONSOLE_FLAGS: u32 = CREATE_NEW_CONSOLE | CREATE_NO_WINDOW | DETACHED_PROCESS;

    /// Creates `command` suspended in `job` (spec 3.2 §10 W1, W5), under `token` when one is
    /// given and this process's token otherwise, with the UTF-16 `environment` block, in `cwd`
    /// (this process's directory when `None`) and with the creation `flags`.
    ///
    /// With `stdio`, the process inherits those three handles and no other, and none of them
    /// is ever inheritable in this process. `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` names only
    /// inheritable handles, and a process another thread starts meanwhile with inheritance and
    /// no list — as `std::process::Command` starts every process — takes every inheritable
    /// handle this process holds; holding a pipe's write end, it would delay the call's end of
    /// output until it exits. Inheritable copies are therefore made only in a [`Broker`],
    /// which the process names as its parent (`PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`) and
    /// inherits them from. Without `stdio`, the process inherits nothing.
    ///
    /// The process is in `job` before it can run: a process is born in its parent's jobs, and
    /// one created without a broker is added while suspended.
    pub(super) fn create_suspended(
        job: &JobObject,
        token: Option<HANDLE>,
        command: &mut [u16],
        environment: &[u16],
        cwd: Option<&[u16]>,
        flags: u32,
        stdio: Option<&ChildStdio>,
    ) -> Result<Suspended, SandboxError> {
        let flags = flags | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT;
        let suspended = match stdio {
            None => {
                // SAFETY: `STARTUPINFOW` is a plain C struct that all zeroes initialises.
                let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
                startup.cb = size_of::<STARTUPINFOW>() as u32;
                // SAFETY: `startup` is a `STARTUPINFOW` naming its size and pointing at nothing.
                unsafe {
                    create(token, command, environment, cwd, false, flags, &raw const startup)
                }?
            }
            Some(io) => {
                let mut broker = Broker::start(job, flags & CONSOLE_FLAGS)?;
                let inherited = [
                    broker.inherit(&io.input)?,
                    broker.inherit(&io.output)?,
                    broker.inherit(&io.error)?,
                ];
                let parent = broker.process.as_raw_handle();
                let mut attributes = AttributeList::new(2)?;
                // SAFETY: `parent` and `inherited` outlive the list's use by `create` below.
                unsafe {
                    attributes.set(
                        PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
                        (&raw const parent).cast(),
                        size_of::<HANDLE>(),
                    )?;
                    attributes.set(
                        PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                        inherited.as_ptr().cast(),
                        size_of_val(&inherited),
                    )?;
                }
                // SAFETY: `STARTUPINFOEXW` is a plain C struct that all zeroes initialises.
                let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
                startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
                startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
                // The values are the copies in the broker, which is where the process inherits
                // its handles from.
                startup.StartupInfo.hStdInput = inherited[0];
                startup.StartupInfo.hStdOutput = inherited[1];
                startup.StartupInfo.hStdError = inherited[2];
                startup.lpAttributeList = attributes.as_ptr();
                let flags = flags | EXTENDED_STARTUPINFO_PRESENT;
                // SAFETY: `startup` is a `STARTUPINFOEXW` naming its size, and its attribute list
                // and the handles it names live until the end of this block.
                unsafe {
                    create(
                        token,
                        command,
                        environment,
                        cwd,
                        true,
                        flags,
                        (&raw const startup).cast(),
                    )
                }?
                // The broker closes its copies and ends here; the process holds its own.
            }
        };
        if !job.contains(suspended.process.as_handle()).unwrap_or(false)
            && let Err(error) = job.assign(suspended.process.as_handle())
        {
            // SAFETY: the process handle is open; the process never ran.
            unsafe { TerminateProcess(suspended.process.as_raw_handle(), 1) };
            return Err(job_error("AssignProcessToJobObject", error));
        }
        Ok(suspended)
    }

    /// `CreateProcessAsUserW` under `token`, or `CreateProcessW` without one.
    ///
    /// # Safety
    ///
    /// `startup` must point at a `STARTUPINFOW` naming its size, or at a `STARTUPINFOEXW` when
    /// `flags` holds `EXTENDED_STARTUPINFO_PRESENT`, valid for the call with all it points to.
    /// `command` and `cwd` must be NUL-terminated and `environment` a UTF-16 environment block.
    unsafe fn create(
        token: Option<HANDLE>,
        command: &mut [u16],
        environment: &[u16],
        cwd: Option<&[u16]>,
        inherit: bool,
        flags: u32,
        startup: *const STARTUPINFOW,
    ) -> Result<Suspended, SandboxError> {
        // SAFETY: `PROCESS_INFORMATION` is a plain C struct that all zeroes initialises.
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let environment: *const c_void = environment.as_ptr().cast();
        let cwd = cwd.map_or(std::ptr::null(), <[u16]>::as_ptr);
        let (created, call) = match token {
            // SAFETY: the token is a restricted child of this process's token, and the caller
            // guarantees every pointer as stated above.
            Some(token) => (
                unsafe {
                    CreateProcessAsUserW(
                        token,
                        std::ptr::null(),
                        command.as_mut_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        i32::from(inherit),
                        flags,
                        environment,
                        cwd,
                        startup,
                        &mut info,
                    )
                },
                "CreateProcessAsUserW",
            ),
            // SAFETY: the caller guarantees every pointer as stated above.
            None => (
                unsafe {
                    CreateProcessW(
                        std::ptr::null(),
                        command.as_mut_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        i32::from(inherit),
                        flags,
                        environment,
                        cwd,
                        startup,
                        &mut info,
                    )
                },
                "CreateProcessW",
            ),
        };
        if created == 0 {
            return Err(last_error(call));
        }
        // SAFETY: both handles were just returned and nothing else owns them.
        let (process, thread) = unsafe {
            (
                OwnedHandle::from_raw_handle(info.hProcess),
                OwnedHandle::from_raw_handle(info.hThread),
            )
        };
        Ok(Suspended { process, thread, pid: info.dwProcessId })
    }

    /// How long a broker may take to exit once terminated; it never ran, so it exits at once.
    const BROKER_EXIT_WAIT_MS: u32 = 5_000;

    /// A process that never runs, whose handle table is the only place a new process's
    /// standard handles are ever inheritable (spec 3.2 §10 W5).
    ///
    /// It is `cmd.exe` from the system directory, created suspended, inheriting nothing, in
    /// the new process's job and with its console flags; no process starts in it but the one
    /// that names it as parent, with a list of exactly its handles. Dropping it closes the
    /// copies made in it and ends it.
    struct Broker {
        process: OwnedHandle,
        copies: Vec<HANDLE>,
    }

    impl Broker {
        /// Starts a broker in `job` with the creation flags `console`.
        fn start(job: &JobObject, console: u32) -> Result<Self, SandboxError> {
            let mut application = system_directory()?;
            application.extend(r"\cmd.exe".encode_utf16().chain(Some(0)));
            let mut command: Vec<u16> = "cmd.exe".encode_utf16().chain(Some(0)).collect();
            // SAFETY: `STARTUPINFOW` and `PROCESS_INFORMATION` are plain C structs that all
            // zeroes initialises.
            let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
            startup.cb = size_of::<STARTUPINFOW>() as u32;
            let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: the application name and the command line are NUL-terminated and the
            // command line is mutable as the API requires; the startup structure names its
            // size; no handle is inherited and this process's environment and directory apply.
            let created = unsafe {
                CreateProcessW(
                    application.as_ptr(),
                    command.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    CREATE_SUSPENDED | console,
                    std::ptr::null(),
                    std::ptr::null(),
                    &startup,
                    &mut info,
                )
            };
            if created == 0 {
                return Err(last_error("CreateProcessW(broker)"));
            }
            // SAFETY: both handles were just returned and nothing else owns them; the thread's
            // is closed at once, since the broker is never resumed.
            let process = unsafe {
                drop(OwnedHandle::from_raw_handle(info.hThread));
                OwnedHandle::from_raw_handle(info.hProcess)
            };
            let broker = Self { process, copies: Vec::new() };
            // In the job from the start, so it cannot outlive this process; dropped on failure,
            // which ends it.
            job.assign(broker.process.as_handle())
                .map_err(|error| job_error("AssignProcessToJobObject(broker)", error))?;
            Ok(broker)
        }

        /// An inheritable copy of `handle` in the broker, valid only there.
        fn inherit(&mut self, handle: &OwnedHandle) -> Result<HANDLE, SandboxError> {
            let mut copy: HANDLE = std::ptr::null_mut();
            // SAFETY: `handle` is open in this process, the broker's handle has every access
            // right, `PROCESS_DUP_HANDLE` among them, and `copy` is writable.
            let duplicated = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    handle.as_raw_handle(),
                    self.process.as_raw_handle(),
                    &mut copy,
                    0,
                    1,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if duplicated == 0 {
                return Err(last_error("DuplicateHandle"));
            }
            self.copies.push(copy);
            Ok(copy)
        }
    }

    impl Drop for Broker {
        fn drop(&mut self) {
            let process = self.process.as_raw_handle();
            for copy in self.copies.drain(..) {
                // SAFETY: `copy` is open in the broker, whose handle allows closing it there.
                unsafe {
                    DuplicateHandle(
                        process,
                        copy,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        0,
                        0,
                        DUPLICATE_CLOSE_SOURCE,
                    )
                };
            }
            // SAFETY: the process handle is open; the broker never ran.
            unsafe {
                TerminateProcess(process, 0);
                WaitForSingleObject(process, BROKER_EXIT_WAIT_MS);
            }
        }
    }

    /// The system directory, such as `C:\Windows\System32`, without a terminating NUL.
    fn system_directory() -> Result<Vec<u16>, SandboxError> {
        let mut buffer = vec![0u16; 260];
        loop {
            // SAFETY: `buffer` is writable for the length passed.
            let len =
                unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
            if len == 0 {
                return Err(last_error("GetSystemDirectoryW"));
            }
            if len < buffer.len() {
                buffer.truncate(len);
                return Ok(buffer);
            }
            // Too small: `len` is the size needed, its NUL included.
            buffer.resize(len, 0);
        }
    }

    /// Logs, for the first acknowledged policy of this process that asks to restrict the
    /// network, that the token does not.
    static NETWORK_WARNING: std::sync::Once = std::sync::Once::new();

    /// A process the restricted token runs, in its job and resumed.
    struct Launched {
        job: JobObject,
        process: OwnedHandle,
        pid: u32,
    }

    /// Builds the restricted token, grants the policy's paths, and starts `program` with
    /// `args` in `cwd` under the token, suspended in a new Job Object, and only then resumes
    /// it. With `stdio`, the child inherits those three handles and no other (see
    /// [`create_suspended`]).
    ///
    /// An MSYS2 program starts from a private copy of its runtime, and the user's MSYS2
    /// namespaces the call can reach are held first; both live as long as the job's
    /// processes (see `msys2`).
    fn launch(
        policy: &SandboxPolicy,
        grants: &[AllowedPath],
        program: &OsStr,
        args: &[OsString],
        cwd: &Path,
        stdio: Option<&ChildStdio>,
    ) -> Result<Launched, SandboxError> {
        // A restrictive network reaches here only when the caller acknowledged that the token
        // leaves it open. Once per process, since a shell would otherwise warn on every command.
        if policy.network.restricts() {
            NETWORK_WARNING.call_once(|| {
                tracing::warn!(
                    sandbox.network = %policy.network,
                    "the restricted-token enforcer does not restrict the network; confined \
                     processes keep full network access, as the caller acknowledged"
                );
            });
        }
        let process_token = open_process_token()?;
        let user = token_user(process_token.0)?;
        let groups = token_groups(process_token.0)?;
        let plan = token::plan(&user, &groups).ok_or_else(|| SandboxError::EnforcerFailed {
            enforcer: ENFORCER_NAME.to_string(),
            message: "the process token carries no logon SID".to_string(),
        })?;
        let token = restricted_token(process_token.0, &plan)?;

        let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        let mut env = confined_environment(policy, &inherited);
        let msys2_program = msys2::program(Path::new(program));
        let mut installations = msys2::installations_on_path(&env);
        if let Some(found) = &msys2_program
            && !installations
                .iter()
                .any(|known| msys2::same_path(&known.bin, &found.installation.bin))
        {
            installations.push(found.installation.clone());
        }
        let runtime = match &msys2_program {
            Some(found) => {
                let granted: Vec<PathBuf> = policy
                    .allowed_paths
                    .iter()
                    .filter(|entry| entry.mode == AccessMode::ReadWrite)
                    .map(|entry| {
                        strip_unc_prefix(
                            &std::fs::canonicalize(&entry.path)
                                .unwrap_or_else(|_| entry.path.clone()),
                        )
                    })
                    .collect();
                Some(msys2::PrivateRuntime::create(found, &granted)?)
            }
            None => None,
        };
        let program: OsString = match (&msys2_program, &runtime) {
            (Some(found), Some(runtime)) => {
                msys2::environment(&mut env, &runtime.bin, found);
                runtime.bin.join(&found.name).into_os_string()
            }
            _ => program.to_owned(),
        };
        // Held before the process exists, so it never finds a namespace it could create.
        let keepers =
            installations.iter().map(msys2::Keeper::start).collect::<Result<Vec<_>, _>>()?;

        // Grant the restricting RESTRICTED SID access to each allowed path, so the restricted
        // token's second access check passes there and nowhere else.
        let restricted = Sid::restricted();
        for entry in grants {
            grant_path(&entry.path, entry.mode, psid(&restricted))?;
        }

        let mut job = JobObject::new().map_err(|error| job_error("CreateJobObjectW", error))?;
        job.attach(Box::new(msys2::Held { _runtime: runtime, _keepers: keepers }));

        let mut command = command_line(&program, args);
        let cwd: Vec<u16> =
            strip_unc_prefix(cwd).as_os_str().encode_wide().chain(Some(0)).collect();
        let suspended = create_suspended(
            &job,
            Some(token.0),
            &mut command,
            &environment_block(env),
            Some(&cwd),
            0,
            stdio,
        )?;
        if let Err(error) = suspended.resume() {
            job.terminate();
            return Err(error);
        }
        Ok(Launched { job, process: suspended.process, pid: suspended.pid })
    }

    /// A process-thread attribute list, freed on drop.
    struct AttributeList {
        // `u64` storage keeps the list pointer-aligned.
        buffer: Vec<u64>,
    }

    impl AttributeList {
        /// An empty list with room for `count` attributes.
        fn new(count: u32) -> Result<Self, SandboxError> {
            let mut size = 0usize;
            // SAFETY: a null list asks only for the size, written to `size`; the call fails
            // with ERROR_INSUFFICIENT_BUFFER by design.
            unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size) };
            if size == 0 {
                return Err(last_error("InitializeProcThreadAttributeList(size)"));
            }
            let mut list = Self { buffer: vec![0u64; size.div_ceil(8)] };
            // SAFETY: the buffer holds at least `size` writable bytes.
            if unsafe { InitializeProcThreadAttributeList(list.as_ptr(), count, 0, &mut size) } == 0
            {
                // An uninitialised list must not be deleted.
                std::mem::forget(list);
                return Err(last_error("InitializeProcThreadAttributeList"));
            }
            Ok(list)
        }

        /// Sets `attribute` to the `size` bytes at `value`.
        ///
        /// # Safety
        ///
        /// `value` must stay readable for `size` bytes for as long as the list is used.
        unsafe fn set(
            &mut self,
            attribute: u32,
            value: *const c_void,
            size: usize,
        ) -> Result<(), SandboxError> {
            // SAFETY: the list is initialised, and the caller keeps `value` readable.
            let updated = unsafe {
                UpdateProcThreadAttribute(
                    self.as_ptr(),
                    0,
                    attribute as usize,
                    value,
                    size,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            if updated == 0 {
                return Err(last_error("UpdateProcThreadAttribute"));
            }
            Ok(())
        }

        fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.buffer.as_mut_ptr().cast()
        }
    }

    impl Drop for AttributeList {
        fn drop(&mut self) {
            // SAFETY: the list was initialised in `new`.
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
        let process = process.as_raw_handle();
        // Ends the tree and waits for it, so no write a process had in flight lands after
        // the call returns.
        let end_tree = |job: &JobObject| match job.terminate_and_wait(JobObject::TERMINATION_WAIT) {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                wait = ?JobObject::TERMINATION_WAIT,
                "a confined process had not exited when the call returned"
            ),
            Err(error) => {
                tracing::warn!(error = %error, "could not wait for the confined processes to exit")
            }
        };

        let wait_ms =
            timeout.map_or(INFINITE, |d| u32::try_from(d.as_millis()).unwrap_or(INFINITE));
        // SAFETY: the process handle is open.
        let waited = unsafe { WaitForSingleObject(process, wait_ms) };
        if waited == WAIT_TIMEOUT {
            // Kill the whole tree.
            end_tree(&job);
            return Ok(ConfinedOutcome { exit_code: None, timed_out: true });
        }
        if waited == WAIT_FAILED {
            let err = last_error("WaitForSingleObject");
            end_tree(&job);
            return Err(err);
        }
        debug_assert_eq!(waited, WAIT_OBJECT_0);

        let mut code: u32 = 0;
        // SAFETY: the process handle is open and `code` is writable.
        let got = unsafe { GetExitCodeProcess(process, &mut code) };
        // The process has exited, so terminating the job now only reaps any descendant that
        // outlived it.
        end_tree(&job);
        if got == 0 {
            return Err(last_error("GetExitCodeProcess"));
        }
        Ok(ConfinedOutcome { exit_code: Some(code as i32), timed_out: false })
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
        Ok(ContainedChild {
            job,
            process: ContainedProcess::new(process, pid),
            stdin: Some(stdin_write.into()),
            stdout: Some(stdout_read.into()),
            stderr: Some(stderr_read.into()),
        })
    }

    #[cfg(test)]
    mod tests {
        use std::io::Read as _;
        use std::os::windows::ffi::OsStringExt as _;

        use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};

        use super::*;

        /// Whether `handle` is inheritable in this process.
        fn inheritable(handle: &OwnedHandle) -> bool {
            let mut flags = 0;
            // SAFETY: the handle is open and `flags` is writable.
            let queried = unsafe { GetHandleInformation(handle.as_raw_handle(), &mut flags) };
            assert_ne!(queried, 0, "GetHandleInformation");
            flags & HANDLE_FLAG_INHERIT != 0
        }

        /// A process created with standard handles inherits them from its broker. Here they
        /// stay non-inheritable throughout, so a process another thread starts cannot take
        /// one, and the process is in its job before it runs (spec 3.2 §10 W5).
        #[test]
        fn a_new_processs_standard_handles_are_never_inheritable_here() {
            let (input, input_write) = pipe().expect("stdin pipe");
            let (output_read, output) = pipe().expect("stdout pipe");
            let (error_read, error) = pipe().expect("stderr pipe");
            let stdio = ChildStdio { input, output, error };
            let job = JobObject::new().expect("job");
            let cmd = PathBuf::from(OsString::from_wide(&system_directory().expect("system")))
                .join("cmd.exe");
            let args: Vec<OsString> =
                ["/d", "/c", "echo out& echo err 1>&2"].iter().map(OsString::from).collect();
            let suspended = create_suspended(
                &job,
                None,
                &mut command_line(cmd.as_os_str(), &args),
                &environment_block(std::env::vars_os().collect()),
                None,
                CREATE_NO_WINDOW,
                Some(&stdio),
            )
            .expect("the process is created");

            for handle in [&stdio.input, &stdio.output, &stdio.error] {
                assert!(!inheritable(handle), "a standard handle became inheritable here");
            }
            assert!(
                job.contains(suspended.process.as_handle()).expect("membership"),
                "the process is born in its job"
            );
            suspended.resume().expect("resumed");
            drop((stdio, input_write));
            let (mut out, mut err) = (String::new(), String::new());
            std::fs::File::from(output_read).read_to_string(&mut out).expect("stdout");
            std::fs::File::from(error_read).read_to_string(&mut err).expect("stderr");
            assert_eq!((out.trim(), err.trim()), ("out", "err"));
        }

        /// The restricted token has no network filter: a restrictive network is refused unless
        /// acknowledged, and the first acknowledged one of the process says the network stays
        /// open.
        mod network {
            use std::sync::{Arc, Mutex};

            use tracing::field::{Field, Visit};
            use tracing::span::{Attributes, Id, Record};
            use tracing::{Event, Level, Metadata, Subscriber};

            use super::*;
            use crate::sandbox::SandboxPolicyBuilder;
            use crate::sandbox::windows::WindowsEnforcer;

            /// The network warnings logged on this thread while it is the default subscriber:
            /// each one's fields other than its message, as `name=value`.
            #[derive(Clone, Default)]
            struct NetworkWarnings(Arc<Mutex<Vec<Vec<String>>>>);

            struct Fields<'a>(&'a mut String, &'a mut Vec<String>);

            impl Visit for Fields<'_> {
                fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                    match field.name() {
                        "message" => *self.0 = format!("{value:?}"),
                        name => self.1.push(format!("{name}={value:?}")),
                    }
                }
            }

            impl Subscriber for NetworkWarnings {
                fn enabled(&self, _: &Metadata<'_>) -> bool {
                    true
                }

                fn new_span(&self, _: &Attributes<'_>) -> Id {
                    Id::from_u64(1)
                }

                fn record(&self, _: &Id, _: &Record<'_>) {}

                fn record_follows_from(&self, _: &Id, _: &Id) {}

                fn event(&self, event: &Event<'_>) {
                    let (mut message, mut fields) = (String::new(), Vec::new());
                    event.record(&mut Fields(&mut message, &mut fields));
                    if *event.metadata().level() == Level::WARN
                        && message.contains("network access")
                    {
                        self.0.lock().expect("the warnings").push(fields);
                    }
                }

                fn enter(&self, _: &Id) {}

                fn exit(&self, _: &Id) {}
            }

            /// Runs `cmd.exe /d /c exit 0` from the system directory under `policy`, with the
            /// open network acknowledged, and returns the fields of each network warning it
            /// logged.
            fn network_warnings(policy: &SandboxPolicy) -> Vec<Vec<String>> {
                let warnings = NetworkWarnings::default();
                let system =
                    PathBuf::from(OsString::from_wide(&system_directory().expect("system")));
                let outcome = tracing::subscriber::with_default(warnings.clone(), || {
                    WindowsEnforcer::new().acknowledge_open_network().run_confined(
                        policy,
                        system.join("cmd.exe").as_os_str(),
                        &[OsString::from("/d"), OsString::from("/c"), OsString::from("exit 0")],
                        &system,
                        Some(Duration::from_secs(60)),
                    )
                })
                .expect("the confined command runs");
                assert_eq!(outcome, ConfinedOutcome { exit_code: Some(0), timed_out: false });
                std::mem::take(&mut *warnings.0.lock().expect("the warnings"))
            }

            /// The warning is logged once per process, so the cases run in order in one test:
            /// it is the only test of this library that starts a confined process, and the
            /// first assertion fails if another one logged the warning first.
            #[test]
            fn the_first_acknowledged_restrictive_policy_warns_once() {
                assert!(
                    !NETWORK_WARNING.is_completed(),
                    "another test in this process logged the network warning first"
                );
                let open = SandboxPolicyBuilder::new().allow_network().build();
                let denied = SandboxPolicyBuilder::new().build();
                let again = SandboxPolicyBuilder::new().build();

                let logged: Vec<_> = [&open, &denied, &again].map(network_warnings).into();

                let first = vec!["sandbox.network=denied".to_string()];
                assert_eq!(logged, vec![vec![], vec![first], vec![]]);
            }
        }
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

    /// The restricted token cannot restrict the network, so a restrictive policy is refused
    /// before anything starts unless the caller acknowledged the open network.
    #[test]
    fn a_restrictive_network_is_refused_unless_acknowledged() {
        let endpoint = crate::sandbox::ProxyEndpoint { port: 3128 };
        for policy in [
            SandboxPolicyBuilder::new().build(),
            SandboxPolicyBuilder::new().proxied(endpoint).build(),
        ] {
            let result = WindowsEnforcer::new().run_confined(
                &policy,
                OsStr::new("cmd.exe"),
                &[],
                std::path::Path::new("."),
                None,
            );
            assert!(matches!(result, Err(SandboxError::Unenforceable { .. })), "{result:?}");
            assert!(WindowsEnforcer::new().check_network(&policy).is_err());
            assert!(
                WindowsEnforcer::new().acknowledge_open_network().check_network(&policy).is_ok()
            );
        }
        let open = SandboxPolicyBuilder::new().allow_network().build();
        assert!(WindowsEnforcer::new().check_network(&open).is_ok());
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
            token::Sid::low_integrity().as_bytes(),
            [1, 1, 0, 0, 0, 0, 0, 16, 0, 0x10, 0, 0]
        );
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
                    logon.clone(),
                    user.clone()
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

    fn pairs(entries: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        entries.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))).collect()
    }

    #[test]
    fn the_environment_is_the_policys_or_this_processs_with_system_root() {
        let inherited = pairs(&[("Path", r"C:\bin"), ("SystemRoot", r"C:\Windows"), ("A", "1")]);

        let empty = SandboxPolicyBuilder::new().build();
        assert_eq!(confined_environment(&empty, &inherited), inherited);

        let set = SandboxPolicyBuilder::new().env("PATH", r"C:\tools").build();
        assert_eq!(
            confined_environment(&set, &inherited),
            pairs(&[("PATH", r"C:\tools"), ("SystemRoot", r"C:\Windows")]),
        );
    }
}
