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
//! - `--unshare-net` — isolate network namespace (no network access)
//! - `--ro-bind <src> <dest>` — read-only filesystem bind mount
//! - `--bind <src> <dest>` — read-write filesystem bind mount
//! - `--new-session` — new session for process isolation

use std::ffi::{OsStr, OsString};

use super::{AccessMode, AllowedPath, SandboxEnforcer, SandboxPolicy, WrappedCommand};
use crate::error::SandboxError;

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
/// //                   "--new-session", "--ro-bind", "/usr/lib", "/usr/lib",
/// //                   "--bind", "/tmp/work", "/tmp/work",
/// //                   "--", "python3", "-c", "print('hello')"]
/// ```
pub struct LinuxEnforcer;

impl LinuxEnforcer {
    /// Creates a new Linux bubblewrap enforcer.
    pub fn new() -> Self {
        Self
    }

    /// Generates bubblewrap arguments from the policy.
    ///
    /// Always starts with `--die-with-parent` and `--unshare-pid`.
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
            policy.allow_network,
            policy.allow_process_spawn,
        )
    }

    /// Internal: generates args from pre-canonicalized paths.
    fn generate_args_from_paths(
        paths: &[AllowedPath],
        allow_network: bool,
        allow_process_spawn: bool,
    ) -> Vec<String> {
        let mut args = Vec::with_capacity(16);

        // Always: terminate child when parent exits
        args.push("--die-with-parent".to_string());

        // Always: isolate PID namespace
        args.push("--unshare-pid".to_string());

        // Network isolation
        if !allow_network {
            args.push("--unshare-net".to_string());
        }

        // Process spawn restriction
        if !allow_process_spawn {
            args.push("--new-session".to_string());
        }

        // Filesystem bind mounts
        for entry in paths {
            let path_str = entry.path.to_string_lossy().to_string();
            match entry.mode {
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
                let ns_check = std::process::Command::new("bwrap")
                    .args(["--unshare-user", "--", "/bin/true"])
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
        // 1. Canonicalize all paths in the policy
        let canonicalized_paths = canonicalize_paths(&policy.allowed_paths)?;

        // 2. Generate bwrap args from canonicalized paths
        let bwrap_args = Self::generate_args_from_paths(
            &canonicalized_paths,
            policy.allow_network,
            policy.allow_process_spawn,
        );

        // 3. Build the wrapped command: bwrap <args> -- <program> <original_args...>
        let mut wrapped_args: Vec<OsString> = bwrap_args.into_iter().map(OsString::from).collect();
        wrapped_args.push(OsString::from("--"));
        wrapped_args.push(program.to_owned());
        wrapped_args.extend_from_slice(args);

        Ok(WrappedCommand { program: OsString::from("bwrap"), args: wrapped_args })
    }
}

/// Canonicalizes all paths in the policy, logging warnings for changed paths.
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

        assert!(!args.contains(&"--new-session".to_string()));
    }

    #[test]
    fn test_generate_args_process_spawn_denied() {
        let policy = SandboxPolicyBuilder::new().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert!(args.contains(&"--new-session".to_string()));
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
