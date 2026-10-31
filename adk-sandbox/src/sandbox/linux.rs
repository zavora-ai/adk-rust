//! Linux bubblewrap sandbox enforcer.
//!
//! Uses `bwrap` with user namespaces to create isolated filesystem and
//! process environments without requiring root privileges.
//!
//! ## How It Works
//!
//! 1. Bubblewrap arguments are generated from the [`SandboxPolicy`]
//! 2. The original command is wrapped: `bwrap <args> -- <program> <args...>`
//! 3. The kernel enforces namespace-based isolation on the child process
//!
//! ## Key bwrap Arguments
//!
//! - `--die-with-parent` — kill child when parent exits
//! - `--unshare-pid` — isolate process ID namespace
//! - `--unshare-net` — unless the network is `Open`: an empty network namespace, so no IP
//!   traffic and no abstract Unix sockets leave the sandbox
//! - `--ro-bind <src> <dest>` — read-only filesystem bind mount
//! - `--bind <src> <dest>` — read-write filesystem bind mount
//! - `--new-session` — always: a new session with no controlling terminal, so the child
//!   cannot inject input into the caller's terminal with `TIOCSTI`
//! - `--seccomp <fd>` — seccomp program that denies process creation
//!
//! ## Nested Paths
//!
//! bubblewrap shows the last mount made over a path, so the policy's paths are mounted
//! shallowest first and the most specific entry decides (spec 3.1 §10 S3): a read-only entry
//! inside a read-write entry is mounted after it and stays read-only. Each writable directory
//! above such an entry is also bound onto itself, which makes it a mount point the kernel
//! refuses to rename or remove, so the entry cannot be carried away from its path and the
//! path recreated writable.
//!
//! ## Network
//!
//! `Denied` network access is `--unshare-net`: an empty network namespace, so no IP traffic and
//! no abstract Unix sockets. A Unix-domain socket on the filesystem is reached through its
//! path, not the network namespace, and a read-only bind does not refuse `connect`, so under
//! `Denied` the enforcer also keeps the host's shared socket directories out:
//!
//! | Policy | Effect |
//! |--------|--------|
//! | A grant of `/run`, or of a path under it, that holds a socket or cannot be fully listed | Refused with [`SandboxError::Unenforceable`] |
//! | A grant of `/tmp`, `/var/tmp` or `/dev/shm` itself | Refused the same way; a directory below them can be granted |
//! | A grant above them, such as `/` | Each is covered with an empty, read-only `tmpfs` before deeper grants are mounted, hiding the sockets of Docker, D-Bus, systemd, tmux, X11 and SSH agents |
//!
//! A socket elsewhere under a granted path, such as in a home directory, stays reachable;
//! grant directories that hold none. `Proxied` is not enforced yet and is refused with
//! [`SandboxError::Unenforceable`].
//!
//! ## Process Spawning
//!
//! When `allow_process_spawn` is `false`, the wrapped program runs under a seccomp filter
//! that fails `fork`, `vfork`, and non-thread `clone` with `EPERM` and `clone3` with
//! `ENOSYS`. Threads and `execve` keep working. The filter is generated for x86-64 and
//! AArch64; on any other architecture [`SandboxEnforcer::configure_command`] returns
//! `EnforcerFailed` rather than running without it.
//!
//! The filter reaches bubblewrap as file descriptor [`SECCOMP_FD`], which
//! [`SandboxEnforcer::configure_command`] installs on the command. Running the output of
//! [`LinuxEnforcer::generate_args`] without that call makes bubblewrap exit with an error.

use std::ffi::{OsStr, OsString};

use super::layering::Layering;
use super::seccomp;
use super::{
    AccessMode, AllowedPath, NetworkAccess, NetworkSupport, SandboxEnforcer, SandboxPolicy,
    WrappedCommand,
};
use crate::error::SandboxError;

/// Directories where host daemons and sessions keep Unix sockets. Under a restrictive network
/// a grant of one of them is refused, and a grant above one covers it with an empty `tmpfs`.
const SOCKET_DIRECTORIES: &[&str] = &["/run", "/tmp", "/var/tmp", "/dev/shm"];

/// File descriptor bubblewrap reads the process-creation seccomp program from.
///
/// High enough to stay clear of stdio and the descriptors the standard library uses while
/// spawning, and below every default `RLIMIT_NOFILE`.
pub const SECCOMP_FD: i32 = 64;

/// Linux bubblewrap sandbox enforcer.
///
/// Wraps child processes with `bwrap` arguments to enforce namespace-based
/// filesystem, network, and process isolation.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::sandbox::linux::LinuxEnforcer;
/// use adk_sandbox::sandbox::{SandboxEnforcer, SandboxPolicyBuilder};
/// use std::ffi::OsString;
///
/// let enforcer = LinuxEnforcer::new();
/// enforcer.probe()?;
///
/// let policy = SandboxPolicyBuilder::new()
///     .allow_read("/usr/lib")
///     .allow_read_write("/tmp/work")
///     .build();
///
/// let wrapped = enforcer.wrap_command(
///     "python3".as_ref(),
///     &[OsString::from("-c"), OsString::from("print('hello')")],
///     &policy,
/// )?;
/// // wrapped.program == "bwrap"
/// // wrapped.args == ["--die-with-parent", "--unshare-pid", "--unshare-net",
/// //                   "--new-session", "--seccomp", "64",
/// //                   "--ro-bind", "/usr/lib", "/usr/lib",
/// //                   "--bind", "/tmp/work", "/tmp/work",
/// //                   "--", "python3", "-c", "print('hello')"]
/// // `configure_command` must then be applied to the command built from `wrapped`.
/// ```
pub struct LinuxEnforcer;

impl LinuxEnforcer {
    /// Creates a new Linux bubblewrap enforcer.
    pub fn new() -> Self {
        Self
    }

    /// Generates bubblewrap arguments from the policy.
    ///
    /// Always starts with `--die-with-parent` and `--unshare-pid`, and always passes
    /// `--new-session`. Bind mounts follow, shallowest path first, with a read-write bind
    /// of each writable directory above a nested read-only entry (see
    /// [Nested Paths](self#nested-paths)).
    /// The returned arguments do NOT include the `--` separator or the
    /// original program/args — those are appended by `wrap_command`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_sandbox::sandbox::linux::LinuxEnforcer;
    /// use adk_sandbox::sandbox::SandboxPolicyBuilder;
    ///
    /// let policy = SandboxPolicyBuilder::new()
    ///     .allow_read("/usr/lib")
    ///     .build();
    ///
    /// let args = LinuxEnforcer::generate_args(&policy);
    /// assert_eq!(args[0], "--die-with-parent");
    /// assert_eq!(args[1], "--unshare-pid");
    /// assert!(args.contains(&"--unshare-net".to_string()));
    /// ```
    pub fn generate_args(policy: &SandboxPolicy) -> Vec<String> {
        Self::generate_args_from_paths(
            &policy.allowed_paths,
            &policy.network,
            policy.allow_process_spawn,
        )
    }

    /// Internal: generates args from pre-canonicalized paths.
    fn generate_args_from_paths(
        paths: &[AllowedPath],
        network: &NetworkAccess,
        allow_process_spawn: bool,
    ) -> Vec<String> {
        let mut args = Vec::with_capacity(16);

        // Always: terminate child when parent exits
        args.push("--die-with-parent".to_string());

        // Always: isolate PID namespace
        args.push("--unshare-pid".to_string());

        // Network isolation
        if network.restricts() {
            args.push("--unshare-net".to_string());
        }

        // Always: a new session with no controlling terminal, so the child cannot inject
        // input into the caller's terminal with `TIOCSTI` (spec 3.1 §10 S2).
        args.push("--new-session".to_string());

        // Process spawn restriction: the seccomp program is what denies fork and clone.
        if !allow_process_spawn {
            args.push("--seccomp".to_string());
            args.push(SECCOMP_FD.to_string());
        }

        // Filesystem bind mounts, shallowest first: a later mount covers an earlier one, so the
        // most specific entry decides. Pinned directories become mount points, which the
        // kernel refuses to rename or remove. Under a restrictive network a socket directory
        // below a mount is covered right after it, before any deeper grant is mounted.
        let mut covered: Vec<&str> = Vec::new();
        for (path, mode) in Layering::new(paths).mounts() {
            let path_str = path.to_string_lossy().to_string();
            match mode {
                AccessMode::ReadOnly => {
                    args.push("--ro-bind".to_string());
                    args.push(path_str.clone());
                    args.push(path_str);
                }
                AccessMode::ReadWrite => {
                    args.push("--bind".to_string());
                    args.push(path_str.clone());
                    args.push(path_str);
                }
            }
            if !network.restricts() {
                continue;
            }
            for directory in SOCKET_DIRECTORIES {
                let target = std::path::Path::new(directory);
                let below = target != path && target.starts_with(path);
                if below
                    && !covered.contains(directory)
                    && std::fs::symlink_metadata(target).is_ok_and(|m| m.is_dir())
                {
                    args.push("--tmpfs".to_string());
                    args.push((*directory).to_string());
                    covered.push(directory);
                }
            }
        }
        for directory in covered {
            args.push("--remount-ro".to_string());
            args.push(directory.to_string());
        }

        args
    }
}

impl Default for LinuxEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxEnforcer for LinuxEnforcer {
    fn name(&self) -> &str {
        "bubblewrap"
    }

    fn network_support(&self) -> NetworkSupport {
        NetworkSupport { denied: true, proxied: false }
    }

    fn probe(&self) -> Result<(), SandboxError> {
        // Check that bwrap binary exists
        let result = std::process::Command::new("bwrap")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match result {
            Ok(status) if status.success() => {
                // Also check user namespaces are available
                // The root bind is not optional. bwrap gives the new namespace an empty
                // root, so without it `/bin/true` does not exist inside and execvp fails —
                // which this probe then reported as "user namespaces are not available",
                // pointing operators at a sysctl that was never the problem. The check
                // failed on every host, so the enforcer was never selectable on Linux.
                let ns_check = std::process::Command::new("bwrap")
                    .args(["--ro-bind", "/", "/", "--unshare-user", "--", "/bin/true"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();

                match ns_check {
                    Ok(s) if s.success() => Ok(()),
                    _ => Err(SandboxError::EnforcerUnavailable {
                        enforcer: "bubblewrap".to_string(),
                        message: "user namespaces are not available. Check that \
                                  `kernel.unprivileged_userns_clone` sysctl is set to 1."
                            .to_string(),
                    }),
                }
            }
            Ok(_) => Err(SandboxError::EnforcerUnavailable {
                enforcer: "bubblewrap".to_string(),
                message: "bwrap binary found but returned an error. \
                          Verify installation is complete."
                    .to_string(),
            }),
            Err(e) => Err(SandboxError::EnforcerUnavailable {
                enforcer: "bubblewrap".to_string(),
                message: format!(
                    "bwrap binary not found: {e}. Install bubblewrap: \
                     `apt install bubblewrap` (Debian/Ubuntu) or \
                     `dnf install bubblewrap` (Fedora/RHEL)."
                ),
            }),
        }
    }

    fn wrap_command(
        &self,
        program: &OsStr,
        args: &[OsString],
        policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError> {
        self.network_support().check(self.name(), &policy.network)?;

        // 1. Canonicalize all paths in the policy
        let canonicalized_paths = canonicalize_paths(&policy.allowed_paths)?;
        if policy.network.restricts() {
            refuse_socket_directories(&canonicalized_paths, &policy.network)?;
        }

        // 2. Generate bwrap args from canonicalized paths
        let bwrap_args = Self::generate_args_from_paths(
            &canonicalized_paths,
            &policy.network,
            policy.allow_process_spawn,
        );

        // 3. Build the wrapped command: bwrap <args> -- <program> <original_args...>
        let mut wrapped_args: Vec<OsString> = bwrap_args.into_iter().map(OsString::from).collect();
        wrapped_args.push(OsString::from("--"));
        wrapped_args.push(program.to_owned());
        wrapped_args.extend_from_slice(args);

        Ok(WrappedCommand { program: OsString::from("bwrap"), args: wrapped_args })
    }

    /// Installs the process-creation seccomp program as file descriptor [`SECCOMP_FD`].
    ///
    /// # Errors
    ///
    /// Returns `EnforcerFailed` when the policy denies process spawning on an architecture
    /// without a generated filter, or when the descriptor cannot be prepared.
    fn configure_command(
        &self,
        cmd: &mut tokio::process::Command,
        policy: &SandboxPolicy,
    ) -> Result<(), SandboxError> {
        use std::io::Write;
        use std::os::fd::{AsRawFd, OwnedFd};

        if policy.allow_process_spawn {
            return Ok(());
        }

        let abi = seccomp::Abi::native().ok_or_else(|| SandboxError::EnforcerFailed {
            enforcer: "bubblewrap".to_string(),
            message: format!(
                "no process-creation seccomp filter exists for {}; allow process spawning in \
                 the policy or run on x86_64 or aarch64",
                std::env::consts::ARCH
            ),
        })?;
        let program = seccomp::encode(&seccomp::deny_process_creation(abi));

        let failed = |error: std::io::Error| SandboxError::EnforcerFailed {
            enforcer: "bubblewrap".to_string(),
            message: format!("failed to prepare the seccomp program descriptor: {error}"),
        };
        // The program is a few hundred bytes, well inside a pipe buffer, so the write cannot
        // block, and closing the writer gives bubblewrap its end-of-file.
        let (reader, mut writer) = std::io::pipe().map_err(failed)?;
        writer.write_all(&program).map_err(failed)?;
        drop(writer);
        let program_fd = OwnedFd::from(reader);

        // SAFETY: the closure runs in the forked child before exec and calls only dup2(2) and
        // fcntl(2), which are async-signal-safe. `program_fd` is owned by the closure, so the
        // descriptor stays open until the command is dropped.
        unsafe {
            cmd.pre_exec(move || {
                let source = program_fd.as_raw_fd();
                let result = if source == SECCOMP_FD {
                    // dup2 onto itself leaves FD_CLOEXEC set, so clear the flag directly.
                    libc::fcntl(source, libc::F_SETFD, 0)
                } else {
                    libc::dup2(source, SECCOMP_FD)
                };
                if result == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
            });
        }
        Ok(())
    }
}

/// Canonicalizes all paths in the policy, logging warnings for changed paths.
/// Refuses a grant of a shared socket directory, or of a path under `/run` that holds a Unix
/// socket: a bind mount keeps its sockets reachable whatever the network namespace.
fn refuse_socket_directories(
    paths: &[AllowedPath],
    network: &NetworkAccess,
) -> Result<(), SandboxError> {
    let run = std::path::Path::new("/run");
    for entry in paths {
        let shared = SOCKET_DIRECTORIES
            .iter()
            .any(|directory| entry.path == std::path::Path::new(directory));
        // A walk of at most this many entries; a larger tree is refused unread.
        let mut budget = 4096;
        if shared || (entry.path.starts_with(run) && holds_socket(&entry.path, &mut budget)) {
            return Err(SandboxError::Unenforceable {
                enforcer: "bubblewrap".to_string(),
                requirement: format!(
                    "network access {network} while granting '{}': Unix sockets there (Docker, \
                     D-Bus, systemd, SSH agents) stay reachable through a bind mount; grant a \
                     directory below it that holds none",
                    entry.path.display()
                ),
            });
        }
    }
    Ok(())
}

/// Whether `path` is, or holds within `budget` entries, a Unix socket; a directory that cannot
/// be listed or a tree past the budget counts as holding one.
fn holds_socket(path: &std::path::Path, budget: &mut usize) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let Ok(metadata) = std::fs::symlink_metadata(path) else { return true };
    if metadata.file_type().is_socket() {
        return true;
    }
    if !metadata.is_dir() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return true };
    for entry in entries {
        if *budget == 0 {
            return true;
        }
        *budget -= 1;
        let Ok(entry) = entry else { return true };
        let Ok(kind) = entry.file_type() else { return true };
        if kind.is_socket() || (kind.is_dir() && holds_socket(&entry.path(), budget)) {
            return true;
        }
    }
    false
}

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxPolicyBuilder;

    #[test]
    fn test_generate_args_deny_all() {
        let policy = SandboxPolicyBuilder::new().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert_eq!(args[0], "--die-with-parent");
        assert_eq!(args[1], "--unshare-pid");
        assert!(args.contains(&"--unshare-net".to_string()));
        assert!(args.contains(&"--new-session".to_string()));
        assert!(
            args.windows(2).any(|pair| pair[0] == "--seccomp" && pair[1] == SECCOMP_FD.to_string())
        );
        // No bind mounts
        assert!(!args.contains(&"--ro-bind".to_string()));
        assert!(!args.contains(&"--bind".to_string()));
    }

    #[test]
    fn test_generate_args_read_only_path() {
        let policy = SandboxPolicyBuilder::new().allow_read("/usr/lib").build();
        let args = LinuxEnforcer::generate_args(&policy);

        let ro_idx = args.iter().position(|a| a == "--ro-bind").unwrap();
        assert_eq!(args[ro_idx + 1], "/usr/lib");
        assert_eq!(args[ro_idx + 2], "/usr/lib");
        assert!(!args.contains(&"--bind".to_string()));
    }

    #[test]
    fn test_generate_args_read_write_path() {
        let policy = SandboxPolicyBuilder::new().allow_read_write("/tmp/work").build();
        let args = LinuxEnforcer::generate_args(&policy);

        let bind_idx = args.iter().position(|a| a == "--bind").unwrap();
        assert_eq!(args[bind_idx + 1], "/tmp/work");
        assert_eq!(args[bind_idx + 2], "/tmp/work");
        assert!(!args.contains(&"--ro-bind".to_string()));
    }

    #[test]
    fn test_generate_args_network_allowed() {
        let policy = SandboxPolicyBuilder::new().allow_network().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert!(!args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn test_generate_args_network_denied() {
        let policy = SandboxPolicyBuilder::new().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert!(args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn test_generate_args_process_spawn_allowed() {
        let policy = SandboxPolicyBuilder::new().allow_process_spawn().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert!(!args.contains(&"--seccomp".to_string()));
    }

    /// A shell is the case that allows process spawning, and it is the one that can reach a
    /// controlling terminal, so the new session cannot depend on the seccomp filter.
    #[test]
    fn every_policy_starts_a_new_session() {
        for allow_process_spawn in [false, true] {
            for network in [NetworkAccess::Denied, NetworkAccess::Open] {
                let args =
                    LinuxEnforcer::generate_args_from_paths(&[], &network, allow_process_spawn);
                assert_eq!(
                    args.iter().filter(|arg| *arg == "--new-session").count(),
                    1,
                    "spawn={allow_process_spawn} network={network}: {args:?}"
                );
            }
        }
    }

    #[test]
    fn test_generate_args_process_spawn_denied() {
        let policy = SandboxPolicyBuilder::new().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert!(args.contains(&"--new-session".to_string()));
        let seccomp = args.iter().position(|arg| arg == "--seccomp").expect("--seccomp present");
        assert_eq!(args[seccomp + 1], SECCOMP_FD.to_string());
    }

    #[test]
    fn configure_command_is_a_no_op_when_spawning_is_allowed() {
        let policy = SandboxPolicyBuilder::new().allow_process_spawn().build();
        let mut cmd = tokio::process::Command::new("true");

        LinuxEnforcer::new().configure_command(&mut cmd, &policy).expect("nothing to configure");
    }

    #[test]
    fn configure_command_installs_the_filter_on_supported_architectures() {
        let policy = SandboxPolicyBuilder::new().build();
        let mut cmd = tokio::process::Command::new("true");

        let result = LinuxEnforcer::new().configure_command(&mut cmd, &policy);
        if seccomp::Abi::native().is_some() {
            result.expect("the filter is prepared on a supported architecture");
        } else {
            assert!(matches!(result, Err(SandboxError::EnforcerFailed { .. })), "{result:?}");
        }
    }

    #[test]
    fn test_generate_args_starts_with_die_with_parent() {
        let policy = SandboxPolicyBuilder::new()
            .allow_read("/tmp")
            .allow_network()
            .allow_process_spawn()
            .build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert_eq!(args[0], "--die-with-parent");
    }

    #[test]
    fn test_generate_args_no_empty_strings() {
        let policy = SandboxPolicyBuilder::new()
            .allow_read("/usr/lib")
            .allow_read_write("/tmp")
            .allow_network()
            .build();
        let args = LinuxEnforcer::generate_args(&policy);

        for arg in &args {
            assert!(!arg.is_empty(), "found empty string in bwrap args");
        }
    }

    #[test]
    fn test_name() {
        let enforcer = LinuxEnforcer::new();
        assert_eq!(enforcer.name(), "bubblewrap");
    }

    /// Proxied network access needs the proxy listener inside the sandbox's network namespace,
    /// which this enforcer does not set up; it refuses rather than run with no network or an
    /// open one.
    #[test]
    fn proxied_network_access_is_refused() {
        let endpoint = super::super::ProxyEndpoint { port: 3128 };
        let policy = SandboxPolicyBuilder::new().proxied(endpoint).build();

        let result = LinuxEnforcer::new().wrap_command(OsStr::new("true"), &[], &policy);

        assert!(matches!(result, Err(SandboxError::Unenforceable { .. })), "{result:?}");
    }

    /// A read-only bind does not refuse `connect` on a Unix socket, so the shared socket
    /// directories are refused under a restrictive network; an open network grants them.
    #[test]
    fn socket_directories_are_refused_under_denied_network() {
        let enforcer = LinuxEnforcer::new();
        for path in ["/run", "/tmp"] {
            let denied = SandboxPolicyBuilder::new().allow_read(path).build();
            let result = enforcer.wrap_command(OsStr::new("true"), &[], &denied);
            assert!(
                matches!(result, Err(SandboxError::Unenforceable { .. })),
                "{path}: {result:?}"
            );

            let open = SandboxPolicyBuilder::new().allow_read(path).allow_network().build();
            assert!(enforcer.wrap_command(OsStr::new("true"), &[], &open).is_ok(), "{path}");
        }
        let narrow = SandboxPolicyBuilder::new().allow_read("/usr").build();
        assert!(enforcer.wrap_command(OsStr::new("true"), &[], &narrow).is_ok());
    }

    /// A grant of `/` covers the socket directories with empty read-only `tmpfs` mounts before
    /// a deeper grant, so the workspace below `/tmp` stays reachable and the rest does not.
    #[test]
    fn a_granted_root_covers_the_socket_directories_under_denied_network() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let workspace = std::fs::canonicalize(workspace.path()).expect("canonical");
        let paths = [
            AllowedPath { path: std::path::PathBuf::from("/"), mode: AccessMode::ReadOnly },
            AllowedPath { path: workspace.clone(), mode: AccessMode::ReadWrite },
        ];
        let args = LinuxEnforcer::generate_args_from_paths(&paths, &NetworkAccess::Denied, true);
        let position = |needle: &[&str]| args.windows(needle.len()).position(|w| w == needle);
        let root = position(&["--ro-bind", "/", "/"]).expect("the root is bound");
        let run = position(&["--tmpfs", "/run"]).expect("/run is covered");
        let tmp = position(&["--tmpfs", "/tmp"]).expect("/tmp is covered");
        let workspace = workspace.to_string_lossy().into_owned();
        let bound = position(&["--bind", &workspace, &workspace]).expect("the workspace is bound");
        assert!(root < run && run < bound && tmp < bound, "{args:?}");
        assert!(position(&["--remount-ro", "/run"]).is_some_and(|ro| ro > bound), "{args:?}");

        let open = LinuxEnforcer::generate_args_from_paths(&paths, &NetworkAccess::Open, true);
        assert!(!open.iter().any(|arg| arg == "--tmpfs"), "{open:?}");
    }

    #[test]
    fn test_wrap_command_nonexistent_path_fails() {
        let enforcer = LinuxEnforcer::new();
        let policy =
            SandboxPolicyBuilder::new().allow_read("/nonexistent/path/that/does/not/exist").build();

        let result = enforcer.wrap_command(OsStr::new("echo"), &[], &policy);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, SandboxError::PolicyViolation(_)),
            "expected PolicyViolation, got: {err:?}"
        );
    }
}
