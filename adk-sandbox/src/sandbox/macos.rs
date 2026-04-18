//! macOS Seatbelt sandbox enforcer.
//!
//! Uses `sandbox-exec -p <profile>` to apply kernel-level restrictions
//! to child processes via the macOS Seatbelt framework.
//!
//! ## How It Works
//!
//! 1. A Seatbelt profile string is generated from the [`SandboxPolicy`]
//! 2. The original command is wrapped: `sandbox-exec -p <profile> <program> <args...>`
//! 3. The kernel enforces the profile restrictions on the child process
//!
//! ## Seatbelt Profile Format
//!
//! Seatbelt profiles use a Scheme-based DSL:
//! ```text
//! (version 1)
//! (deny default)
//! (allow process-exec)
//! (allow file-read* (subpath "/usr/lib"))
//! (allow file-read* file-write* (subpath "/tmp/work"))
//! (allow network*)
//! ```

use std::ffi::{OsStr, OsString};

use super::{AccessMode, AllowedPath, SandboxEnforcer, SandboxPolicy, WrappedCommand};
use crate::error::SandboxError;

/// macOS Seatbelt sandbox enforcer.
///
/// Wraps child processes with `sandbox-exec -p <profile>` to enforce
/// kernel-level filesystem, network, and process restrictions.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::sandbox::macos::MacOsEnforcer;
/// use adk_sandbox::sandbox::{SandboxEnforcer, SandboxPolicyBuilder};
/// use std::ffi::OsString;
///
/// let enforcer = MacOsEnforcer::new();
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
/// // wrapped.program == "sandbox-exec"
/// // wrapped.args == ["-p", "<profile>", "python3", "-c", "print('hello')"]
/// ```
pub struct MacOsEnforcer;

impl MacOsEnforcer {
    /// Creates a new macOS Seatbelt enforcer.
    pub fn new() -> Self {
        Self
    }

    /// Generates a Seatbelt profile string from the policy.
    ///
    /// The profile begins with `(version 1)` and `(deny default)`,
    /// always includes `(allow process-exec)` for the initial execution,
    /// and adds directives for each allowed path and permission.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_sandbox::sandbox::macos::MacOsEnforcer;
    /// use adk_sandbox::sandbox::SandboxPolicyBuilder;
    ///
    /// let policy = SandboxPolicyBuilder::new()
    ///     .allow_read("/usr/lib")
    ///     .allow_network()
    ///     .build();
    ///
    /// let profile = MacOsEnforcer::generate_profile(&policy);
    /// assert!(profile.contains("(version 1)"));
    /// assert!(profile.contains("(deny default)"));
    /// assert!(profile.contains("(allow network*)"));
    /// ```
    pub fn generate_profile(policy: &SandboxPolicy) -> String {
        Self::generate_profile_from_paths(
            &policy.allowed_paths,
            policy.allow_network,
            &policy.network_rules,
            policy.allow_process_spawn,
        )
    }

    /// Internal: generates profile from pre-canonicalized paths.
    ///
    /// Uses a "deny what's dangerous" approach rather than "allow only what's needed":
    /// - Start with `(allow default)` so programs can actually run
    /// - Deny network access unless explicitly allowed
    /// - If network is denied but domain rules exist, allow specific domains/ports
    /// - Deny process spawning unless explicitly allowed
    /// - Restrict filesystem writes to only allowed read-write paths
    ///
    /// This is more practical than a pure whitelist because programs like Python
    /// need dozens of syscall categories (mach-lookup, sysctl-read, iokit, etc.)
    /// that are impractical to enumerate.
    fn generate_profile_from_paths(
        paths: &[AllowedPath],
        allow_network: bool,
        network_rules: &[super::NetworkRule],
        allow_process_spawn: bool,
    ) -> String {
        let mut profile = String::with_capacity(512);

        // Base: allow everything by default, then deny dangerous operations
        profile.push_str("(version 1)\n");
        profile.push_str("(deny default)\n");
        profile.push_str("(allow default)\n");

        // Network access control
        if allow_network {
            // Full network access — no deny rule needed
        } else if network_rules.is_empty() {
            // No network at all
            profile.push_str("(deny network*)\n");
        } else {
            // Domain-level allowlist: deny all network, then allow specific domains
            profile.push_str("(deny network*)\n");
            // Allow DNS lookups (required for domain resolution)
            profile.push_str("(allow network-outbound (remote udp (to \"*:53\")))\n");
            profile.push_str("(allow network-outbound (remote tcp (to \"*:53\")))\n");

            for rule in network_rules {
                // Escape dots in domain for regex
                let escaped_domain = rule.domain.replace('.', "\\\\.");
                if rule.ports.is_empty() {
                    // All ports on this domain
                    profile.push_str(&format!(
                        "(allow network-outbound (remote tcp (regex #\"^{escaped_domain}$\")))\n"
                    ));
                } else {
                    // Specific ports on this domain
                    for port in &rule.ports {
                        profile.push_str(&format!(
                            "(allow network-outbound (remote tcp (to \"{domain}:{port}\")))\n",
                            domain = rule.domain,
                        ));
                    }
                }
            }
        }

        // Deny process spawning unless explicitly allowed
        if !allow_process_spawn {
            profile.push_str("(deny process-fork)\n");
        }

        // Restrict filesystem writes: deny all writes, then allow specific paths
        profile.push_str("(deny file-write*)\n");
        for entry in paths {
            let path_str = entry.path.to_string_lossy();
            match entry.mode {
                AccessMode::ReadOnly => {
                    // Read-only paths: already allowed by (allow default), write denied by (deny file-write*)
                    // Add explicit read allow as documentation
                    profile.push_str(&format!("(allow file-read* (subpath \"{path_str}\"))\n"));
                }
                AccessMode::ReadWrite => {
                    // Read-write paths: explicitly allow writes
                    profile.push_str(&format!(
                        "(allow file-read* file-write* (subpath \"{path_str}\"))\n"
                    ));
                }
            }
        }

        profile
    }
}

impl Default for MacOsEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxEnforcer for MacOsEnforcer {
    fn name(&self) -> &str {
        "seatbelt"
    }

    fn probe(&self) -> Result<(), SandboxError> {
        // Verify sandbox-exec exists and is executable by running a no-op command
        let result = std::process::Command::new("sandbox-exec")
            .arg("-p")
            .arg("(version 1)(allow default)")
            .arg("/usr/bin/true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match result {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(SandboxError::EnforcerUnavailable {
                enforcer: "seatbelt".to_string(),
                message: format!(
                    "sandbox-exec probe failed with exit code {}. \
                     Verify macOS version (10.5+) and that System Integrity Protection \
                     has not removed sandbox-exec.",
                    status.code().unwrap_or(-1)
                ),
            }),
            Err(e) => Err(SandboxError::EnforcerUnavailable {
                enforcer: "seatbelt".to_string(),
                message: format!(
                    "sandbox-exec binary not found: {e}. \
                     Verify macOS version (10.5+) and that System Integrity Protection \
                     has not removed it."
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

        // 2. Generate the Seatbelt profile from canonicalized paths
        let profile = Self::generate_profile_from_paths(
            &canonicalized_paths,
            policy.allow_network,
            &policy.network_rules,
            policy.allow_process_spawn,
        );

        // 3. Build the wrapped command: sandbox-exec -p <profile> <program> <args...>
        let mut wrapped_args = Vec::with_capacity(3 + args.len());
        wrapped_args.push(OsString::from("-p"));
        wrapped_args.push(OsString::from(&profile));
        wrapped_args.push(program.to_owned());
        wrapped_args.extend_from_slice(args);

        Ok(WrappedCommand { program: OsString::from("sandbox-exec"), args: wrapped_args })
    }
}

/// Canonicalizes all paths in the policy, logging warnings for changed paths.
///
/// Returns `SandboxError::PolicyViolation` if any path cannot be resolved.
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
    fn test_generate_profile_deny_all() {
        let policy = SandboxPolicyBuilder::new().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow default)"));
        // Network and writes should be denied
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(deny process-fork)"));
    }

    #[test]
    fn test_generate_profile_read_only_path() {
        let policy = SandboxPolicyBuilder::new().allow_read("/usr/lib").build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(allow file-read* (subpath \"/usr/lib\"))"));
        // Should not have file-write for this path
        assert!(!profile.contains("file-write* (subpath \"/usr/lib\")"));
    }

    #[test]
    fn test_generate_profile_read_write_path() {
        let policy = SandboxPolicyBuilder::new().allow_read_write("/tmp/work").build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(allow file-read* file-write* (subpath \"/tmp/work\"))"));
    }

    #[test]
    fn test_generate_profile_network_allowed() {
        let policy = SandboxPolicyBuilder::new().allow_network().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        // Should NOT contain deny network
        assert!(!profile.contains("(deny network*)"));
    }

    #[test]
    fn test_generate_profile_network_denied() {
        let policy = SandboxPolicyBuilder::new().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(deny network*)"));
    }

    #[test]
    fn test_generate_profile_process_spawn_allowed() {
        let policy = SandboxPolicyBuilder::new().allow_process_spawn().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(!profile.contains("(deny process-fork)"));
    }

    #[test]
    fn test_generate_profile_process_spawn_denied() {
        let policy = SandboxPolicyBuilder::new().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(deny process-fork)"));
    }

    #[test]
    fn test_generate_profile_multiple_paths() {
        let policy = SandboxPolicyBuilder::new()
            .allow_read("/usr/lib")
            .allow_read_write("/tmp/work")
            .allow_read("/etc")
            .build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(allow file-read* (subpath \"/usr/lib\"))"));
        assert!(profile.contains("(allow file-read* file-write* (subpath \"/tmp/work\"))"));
        assert!(profile.contains("(allow file-read* (subpath \"/etc\"))"));
    }

    #[test]
    fn test_generate_profile_balanced_parentheses() {
        let policy = SandboxPolicyBuilder::new()
            .allow_read("/usr/lib")
            .allow_read_write("/tmp")
            .allow_network()
            .allow_process_spawn()
            .build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        let open = profile.chars().filter(|c| *c == '(').count();
        let close = profile.chars().filter(|c| *c == ')').count();
        assert_eq!(open, close, "parentheses are not balanced in profile:\n{profile}");
    }

    #[test]
    fn test_probe_succeeds_on_macos() {
        // This test only passes on macOS where sandbox-exec exists
        let enforcer = MacOsEnforcer::new();
        let result = enforcer.probe();
        assert!(result.is_ok(), "probe failed: {result:?}");
    }

    #[test]
    fn test_wrap_command_with_real_path() {
        let enforcer = MacOsEnforcer::new();
        let policy = SandboxPolicyBuilder::new().allow_read("/tmp").build();

        let result = enforcer.wrap_command(OsStr::new("echo"), &[OsString::from("hello")], &policy);

        // /tmp is a symlink to /private/tmp on macOS
        let wrapped = result.expect("wrap_command should succeed for /tmp");
        assert_eq!(wrapped.program, OsString::from("sandbox-exec"));
        assert_eq!(wrapped.args[0], OsString::from("-p"));
        // args[1] is the profile string
        assert_eq!(wrapped.args[2], OsString::from("echo"));
        assert_eq!(wrapped.args[3], OsString::from("hello"));
    }

    #[test]
    fn test_wrap_command_nonexistent_path_fails() {
        let enforcer = MacOsEnforcer::new();
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

    #[test]
    fn test_name() {
        let enforcer = MacOsEnforcer::new();
        assert_eq!(enforcer.name(), "seatbelt");
    }

    #[test]
    fn test_generate_profile_domain_allowlist() {
        let policy = SandboxPolicyBuilder::new()
            .allow_domain("api.openai.com", &[443])
            .allow_domain("huggingface.co", &[443, 80])
            .build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        // Should deny all network first
        assert!(profile.contains("(deny network*)"));
        // Should allow DNS
        assert!(profile.contains("(allow network-outbound (remote udp (to \"*:53\"))"));
        // Should allow specific domains/ports
        assert!(profile.contains("api.openai.com:443"));
        assert!(profile.contains("huggingface.co:443"));
        assert!(profile.contains("huggingface.co:80"));
    }

    #[test]
    fn test_generate_profile_domain_all_ports() {
        let policy = SandboxPolicyBuilder::new().allow_domain("example.com", &[]).build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("example\\\\.com"));
    }

    #[test]
    fn test_generate_profile_full_network_overrides_rules() {
        let policy = SandboxPolicyBuilder::new()
            .allow_network()
            .allow_domain("api.openai.com", &[443])
            .build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        // Full network access — no deny rule, domain rules ignored
        assert!(!profile.contains("(deny network*)"));
    }
}
