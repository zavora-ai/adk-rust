//! macOS Seatbelt sandbox enforcer.
//!
//! Uses `sandbox-exec -p <profile>` to apply kernel-level restrictions
//! to child processes via the macOS Seatbelt framework.
//!
//! ## How It Works
//!
//! 1. A Seatbelt profile string is generated from the [`SandboxPolicy`]
//! 2. The original command is wrapped: `sandbox-exec -p <profile> <program> <args...>`
//! 3. The kernel enforces the profile restrictions on the child process and every
//!    process it starts
//!
//! ## Policy
//!
//! The profile is deny-by-default: an operation not listed below is refused.
//!
//! | Operation | Allowed |
//! |-----------|---------|
//! | `process-exec` | Always — the wrapped program, and anything it executes in place |
//! | `process-fork` | Only with `allow_process_spawn` |
//! | `file-read*` | The system runtime (`/bin`, `/sbin`, `/usr`, `/System`, `/Library/Apple`, `/Library/Frameworks`, `/opt/homebrew`, `/private/etc` minus credential files, the dyld cache, time zone data), random devices, and the policy's allowed paths |
//! | `file-read-metadata` | Every path — resolving a path needs `stat` and `readlink` on each ancestor, so existence, size, and timestamps are visible everywhere; contents are not |
//! | `file-write*` | `/dev/null`, `/dev/zero`, `/dev/dtracehelper`, and the policy's read-write paths |
//! | `network*` | Only with `allow_network`; `network_rules` are not enforceable and fail closed |
//! | `mach-lookup` | User and group lookup, temp-dir resolution, notifications, logging, and preferences; DNS, trust evaluation, and network configuration only with `allow_network` |
//! | `sysctl-read`, `user-preference-read`, `signal` within the sandbox | Always |
//!
//! Home directories, `/tmp`, `/private/var/folders`, and the rest of the host are not
//! readable unless the policy names them. Programs installed outside the system runtime —
//! a rustup toolchain, a pyenv interpreter, Xcode — need their directories allowed
//! explicitly.
//!
//! Every path is interpolated as an escaped SBPL string literal, so a path containing `"`,
//! `\`, or control characters cannot terminate the literal and inject profile directives.
//!
//! ```text
//! (version 1)
//! (deny default)
//! (allow process-exec)
//! (allow file-read* (subpath "/usr") ...)
//! (allow file-read* (subpath "/opt/data"))
//! (allow file-read* file-write* (subpath "/private/tmp/work"))
//! ```

use std::ffi::{OsStr, OsString};

use super::{AccessMode, AllowedPath, SandboxEnforcer, SandboxPolicy, WrappedCommand};
use crate::error::SandboxError;

/// macOS Seatbelt sandbox enforcer.
///
/// Wraps child processes with `sandbox-exec -p <profile>` to enforce a
/// deny-by-default kernel-level filesystem, network, and process policy.
/// See the [module documentation](self) for exactly what the profile allows.
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
    /// The profile begins with `(version 1)` and `(deny default)`, grants the
    /// system runtime described in the [module documentation](self), and adds
    /// one directive per allowed path. Paths are rendered as escaped SBPL string
    /// literals. Paths that are not valid UTF-8 are rendered lossily here;
    /// [`SandboxEnforcer::wrap_command`] rejects them instead.
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
    fn generate_profile_from_paths(
        paths: &[AllowedPath],
        allow_network: bool,
        network_rules: &[super::NetworkRule],
        allow_process_spawn: bool,
    ) -> String {
        let mut profile = String::with_capacity(2048);

        profile.push_str("(version 1)\n");
        profile.push_str("(deny default)\n");
        profile.push_str("(allow process-exec)\n");
        if allow_process_spawn {
            profile.push_str("(allow process-fork)\n");
        }
        profile.push_str("(allow signal (target same-sandbox))\n");
        profile.push_str("(allow sysctl-read)\n");
        profile.push_str("(allow user-preference-read)\n");

        // Path resolution stats and reads links on every ancestor of every path it touches.
        profile.push_str("(allow file-read-metadata)\n");
        profile.push_str("(allow file-read* (literal \"/\"))\n");
        push_path_rule(&mut profile, "allow file-read*", "subpath", SYSTEM_READ_PATHS);
        push_path_rule(
            &mut profile,
            "allow file-read*",
            "literal",
            &["/dev/random", "/dev/urandom"],
        );
        push_path_rule(
            &mut profile,
            "allow file-read* file-write-data",
            "literal",
            &["/dev/null", "/dev/zero"],
        );
        push_path_rule(
            &mut profile,
            "allow file-read* file-write-data file-ioctl",
            "literal",
            &["/dev/dtracehelper"],
        );
        push_path_rule(
            &mut profile,
            "allow file-read-data file-write-data",
            "subpath",
            &["/dev/fd"],
        );
        // After the system grants so these win over `/private/etc`, and before the policy's own
        // paths so a caller can still allow them deliberately.
        push_path_rule(&mut profile, "deny file-read*", "subpath", SYSTEM_READ_DENIALS);

        push_global_names(&mut profile, STARTUP_MACH_SERVICES);
        profile.push_str(
            "(allow ipc-posix-shm-read* (ipc-posix-name \"apple.shm.notification_center\") \
             (ipc-posix-name-prefix \"apple.cfprefs.\"))\n",
        );

        if allow_network {
            profile.push_str("(allow network*)\n");
            push_global_names(&mut profile, NETWORK_MACH_SERVICES);
            push_path_rule(
                &mut profile,
                "allow file-read*",
                "literal",
                &["/private/var/run/resolv.conf"],
            );
        } else if !network_rules.is_empty() {
            // Seatbelt network filters accept only `*` or `localhost` as the host, so a
            // per-domain allowlist cannot be expressed. Denying all network fails closed.
            tracing::warn!(
                rules_count = network_rules.len(),
                "seatbelt cannot filter network access by domain; \
                 network_rules are ignored and all network access is blocked"
            );
        }

        for entry in paths {
            let path = sbpl_string(&entry.path.to_string_lossy());
            match entry.mode {
                AccessMode::ReadOnly => {
                    profile.push_str(&format!("(allow file-read* (subpath {path}))\n"));
                }
                AccessMode::ReadWrite => {
                    profile.push_str(&format!("(allow file-read* file-write* (subpath {path}))\n"));
                }
            }
        }

        profile
    }
}

/// System runtime locations every sandboxed program may read: binaries, the dyld shared
/// cache, system libraries and frameworks, interpreter installs, host configuration, and
/// the `/var/select` links that pick the default shell and developer directory.
const SYSTEM_READ_PATHS: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr",
    "/System",
    "/Library/Apple",
    "/Library/Frameworks",
    "/opt/homebrew",
    "/private/etc",
    "/private/var/db/dyld",
    "/private/var/db/timezone",
    "/private/var/select",
];

/// Credential stores under [`SYSTEM_READ_PATHS`] that stay unreadable.
const SYSTEM_READ_DENIALS: &[&str] =
    &["/private/etc/master.passwd", "/private/etc/sudoers", "/private/etc/sudoers.d"];

/// Mach services that ordinary process startup reaches: user and group lookup
/// (`getpwuid`), `confstr` temp-dir resolution, notifications, logging, and preferences.
const STARTUP_MACH_SERVICES: &[&str] = &[
    "com.apple.system.opendirectoryd.libinfo",
    "com.apple.system.opendirectoryd.membership",
    "com.apple.bsd.dirhelper",
    "com.apple.system.notification_center",
    "com.apple.system.logger",
    "com.apple.logd",
    "com.apple.cfprefsd.daemon",
    "com.apple.cfprefsd.agent",
];

/// Mach services for name resolution, certificate trust, and network configuration.
const NETWORK_MACH_SERVICES: &[&str] = &[
    "com.apple.dnssd.service",
    "com.apple.trustd",
    "com.apple.trustd.agent",
    "com.apple.SystemConfiguration.configd",
    "com.apple.SystemConfiguration.DNSConfiguration",
];

/// Appends `(<action> (<filter> "<path>") ...)` for constant paths.
fn push_path_rule(profile: &mut String, action: &str, filter: &str, paths: &[&str]) {
    profile.push('(');
    profile.push_str(action);
    for path in paths {
        let path = sbpl_string(path);
        profile.push_str(&format!(" ({filter} {path})"));
    }
    profile.push_str(")\n");
}

/// Appends a `mach-lookup` allowance for the given global service names.
fn push_global_names(profile: &mut String, names: &[&str]) {
    profile.push_str("(allow mach-lookup");
    for name in names {
        let name = sbpl_string(name);
        profile.push_str(&format!(" (global-name {name})"));
    }
    profile.push_str(")\n");
}

/// Renders `value` as an SBPL string literal, surrounding quotes included.
///
/// `\` and `"` are backslash-escaped and ASCII control characters become `\xHH`
/// escapes, so no input can end the literal early: a directory named
/// `w"))(allow network*)(allow file-write* (subpath "/` stays one path instead of
/// becoming three profile directives.
fn sbpl_string(value: &str) -> String {
    use std::fmt::Write as _;

    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            control if control.is_ascii_control() => {
                let code = u32::from(control);
                // Writing to a String cannot fail.
                let _ = write!(literal, "\\x{code:02x}");
            }
            other => literal.push(other),
        }
    }
    literal.push('"');
    literal
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
        // Run a no-op under the most restrictive profile this enforcer generates, so a
        // macOS release that rejects any of its directives fails here rather than at run time.
        let profile = Self::generate_profile_from_paths(&[], false, &[], false);
        let result = std::process::Command::new("sandbox-exec")
            .arg("-p")
            .arg(&profile)
            .arg("/usr/bin/true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match result {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(SandboxError::EnforcerUnavailable {
                enforcer: "seatbelt".to_string(),
                message: format!(
                    "sandbox-exec could not run /usr/bin/true under the base profile \
                     (exit code {}). Verify macOS version (10.5+), that System Integrity \
                     Protection has not removed sandbox-exec, and that this process is not \
                     already running inside a sandbox.",
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
        // 1. Canonicalize all paths in the policy. Seatbelt matches the resolved path, so
        //    `/tmp` must be granted as `/private/tmp`.
        let canonicalized_paths = canonicalize_paths(&policy.allowed_paths)?;
        if let Some(entry) = canonicalized_paths.iter().find(|entry| entry.path.to_str().is_none())
        {
            return Err(SandboxError::PolicyViolation(format!(
                "allowed path '{}' is not valid UTF-8 and cannot be expressed in a Seatbelt \
                 profile. Rename the directory or allow a parent directory instead.",
                entry.path.display()
            )));
        }

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

    /// Removes every SBPL string literal, leaving only the profile's structure.
    ///
    /// Anything a hostile path managed to inject would survive this as a bare form.
    fn strip_string_literals(profile: &str) -> String {
        let mut structure = String::with_capacity(profile.len());
        let mut chars = profile.chars();
        while let Some(ch) = chars.next() {
            if ch != '"' {
                structure.push(ch);
                continue;
            }
            structure.push_str("\"\"");
            while let Some(inner) = chars.next() {
                match inner {
                    '\\' => {
                        chars.next();
                    }
                    '"' => break,
                    _ => {}
                }
            }
        }
        structure
    }

    #[test]
    fn test_generate_profile_deny_all() {
        let policy = SandboxPolicyBuilder::new().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(
            !profile.contains("(allow default)"),
            "an allow-default rule overrides the deny-default base:\n{profile}"
        );
        assert!(!profile.contains("(allow network*)"));
        assert!(!profile.contains("(allow process-fork)"));
        assert!(!profile.contains("file-write*"), "nothing is writable:\n{profile}");
        assert!(profile.contains("(allow process-exec)"));
    }

    #[test]
    fn test_generate_profile_never_grants_blanket_reads() {
        let policy = SandboxPolicyBuilder::new().allow_network().allow_process_spawn().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(!profile.contains("(allow file-read*)\n"), "blanket read grant:\n{profile}");
        for line in profile.lines() {
            assert!(
                !line.contains("(subpath \"/Users\")")
                    && !line.contains("(subpath \"/private/var\")"),
                "home directories and temp space must not be granted by default: {line}"
            );
        }
    }

    #[test]
    fn test_generate_profile_denies_credential_stores_before_policy_paths() {
        let policy = SandboxPolicyBuilder::new().allow_read("/opt/data").build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        let system = profile.find("(subpath \"/private/etc\")").expect("system grant");
        let denial = profile.find("(deny file-read*").expect("credential denial");
        let policy_path = profile.find("(subpath \"/opt/data\")").expect("policy grant");
        assert!(system < denial && denial < policy_path, "rule order is wrong:\n{profile}");
        assert!(profile.contains("(subpath \"/private/etc/master.passwd\")"));
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

        assert!(profile.contains("(allow network*)"));
        assert!(profile.contains("(global-name \"com.apple.dnssd.service\")"));
    }

    #[test]
    fn test_generate_profile_network_denied() {
        let policy = SandboxPolicyBuilder::new().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(!profile.contains("network"), "no network rule may appear:\n{profile}");
        assert!(!profile.contains("com.apple.dnssd.service"));
    }

    #[test]
    fn test_generate_profile_process_spawn_allowed() {
        let policy = SandboxPolicyBuilder::new().allow_process_spawn().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(allow process-fork)"));
    }

    #[test]
    fn test_generate_profile_process_spawn_denied() {
        let policy = SandboxPolicyBuilder::new().build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(!profile.contains("process-fork"));
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
    fn sbpl_string_escapes_quotes_backslashes_and_control_characters() {
        assert_eq!(sbpl_string("/plain/path"), "\"/plain/path\"");
        assert_eq!(sbpl_string("/it's \"quoted\""), "\"/it's \\\"quoted\\\"\"");
        assert_eq!(sbpl_string("/back\\slash"), "\"/back\\\\slash\"");
        assert_eq!(sbpl_string("/a\nb\tc\u{7f}"), "\"/a\\x0ab\\x09c\\x7f\"");
        assert_eq!(sbpl_string("/ünïcode"), "\"/ünïcode\"");
    }

    /// The directory name from the finding: unescaped, it closes the subpath literal and
    /// appends `(allow network*)` and a write grant on `/`.
    #[test]
    fn a_hostile_directory_name_cannot_inject_profile_directives() {
        let hostile = "/tmp/w\"))(allow network*)(allow file-write* (subpath \"/";
        let policy = SandboxPolicyBuilder::new().allow_read_write(hostile).build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        let structure = strip_string_literals(&profile);
        assert!(
            !structure.contains("(allow network*)"),
            "the path injected a network grant:\n{profile}"
        );
        assert_eq!(
            structure.matches("file-write*").count(),
            1,
            "the path injected a write grant:\n{profile}"
        );
        assert!(profile.contains(
            "(allow file-read* file-write* (subpath \
             \"/tmp/w\\\"))(allow network*)(allow file-write* (subpath \\\"/\"))"
        ));
        let open = structure.matches('(').count();
        let close = structure.matches(')').count();
        assert_eq!(open, close, "unbalanced structure:\n{structure}");
    }

    #[test]
    fn a_newline_in_a_path_cannot_start_a_new_directive() {
        let policy =
            SandboxPolicyBuilder::new().allow_read("/tmp/x\")\n(allow network*)\n(\"").build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(!strip_string_literals(&profile).contains("(allow network*)"), "{profile}");
        assert!(!profile.contains("\n(allow network*)"), "{profile}");
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
        assert!(
            wrapped.args[1]
                .to_string_lossy()
                .contains("(allow file-read* (subpath \"/private/tmp\"))"),
            "the canonical path must be granted"
        );
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

    /// Seatbelt rejects hostnames in network filters (`host must be * or localhost`), so the
    /// rules previously produced a profile `sandbox-exec` refused to parse. They now fail
    /// closed to no network at all.
    #[test]
    fn test_generate_profile_domain_rules_fail_closed() {
        let policy = SandboxPolicyBuilder::new()
            .allow_domain("api.openai.com", &[443])
            .allow_domain("huggingface.co", &[443, 80])
            .allow_domain("example.com", &[])
            .build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(!profile.contains("network"), "domain rules must not grant network:\n{profile}");
        assert!(!profile.contains("api.openai.com"));
        assert!(!profile.contains("example"));
    }

    #[test]
    fn test_generate_profile_full_network_overrides_rules() {
        let policy = SandboxPolicyBuilder::new()
            .allow_network()
            .allow_domain("api.openai.com", &[443])
            .build();
        let profile = MacOsEnforcer::generate_profile(&policy);

        assert!(profile.contains("(allow network*)"));
        assert!(!profile.contains("api.openai.com"));
    }
}
