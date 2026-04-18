//! Windows AppContainer sandbox enforcer.
//!
//! Uses the Win32 AppContainer API to create restricted process tokens
//! that limit filesystem, network, and registry access.
//!
//! ## How It Works
//!
//! Unlike macOS/Linux enforcers, the Windows enforcer does NOT wrap the
//! command with a different program. Instead, it:
//!
//! 1. Creates an AppContainer profile with a unique SID
//! 2. Sets ACLs on allowed paths
//! 3. Configures the process token via `configure_command()`
//!
//! The `wrap_command()` method returns the original program and args
//! unchanged. All restrictions are applied via `configure_command()`.

use std::ffi::{OsStr, OsString};

use super::{AllowedPath, SandboxEnforcer, SandboxPolicy, WrappedCommand};
use crate::error::SandboxError;

/// Windows AppContainer sandbox enforcer.
///
/// Configures child processes with AppContainer restrictions via Win32 APIs.
/// Unlike macOS/Linux enforcers, this does not wrap the command — it applies
/// process token restrictions via [`configure_command()`](SandboxEnforcer::configure_command).
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::sandbox::windows::WindowsEnforcer;
/// use adk_sandbox::sandbox::{SandboxEnforcer, SandboxPolicyBuilder};
/// use std::ffi::OsString;
///
/// let enforcer = WindowsEnforcer::new();
/// enforcer.probe()?;
///
/// let policy = SandboxPolicyBuilder::new()
///     .allow_read("C:\\Users\\Public")
///     .allow_read_write("C:\\Temp\\work")
///     .build();
///
/// // wrap_command returns the original program unchanged
/// let wrapped = enforcer.wrap_command(
///     "python.exe".as_ref(),
///     &[OsString::from("-c"), OsString::from("print('hello')")],
///     &policy,
/// )?;
/// assert_eq!(wrapped.program, OsString::from("python.exe"));
///
/// // AppContainer restrictions are applied via configure_command()
/// let mut cmd = tokio::process::Command::new(&wrapped.program);
/// cmd.args(&wrapped.args);
/// enforcer.configure_command(&mut cmd, &policy)?;
/// ```
pub struct WindowsEnforcer;

impl WindowsEnforcer {
    /// Creates a new Windows AppContainer enforcer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxEnforcer for WindowsEnforcer {
    fn name(&self) -> &str {
        "appcontainer"
    }

    fn probe(&self) -> Result<(), SandboxError> {
        // On non-Windows platforms, this enforcer is never available.
        // On Windows, verify the AppContainer API is accessible.
        #[cfg(target_os = "windows")]
        {
            // Attempt to call CreateAppContainerProfile with a probe name.
            // If the API is available (Windows 8+), this succeeds or returns
            // ALREADY_EXISTS — both indicate the API works.
            use windows_sys::Win32::Security::CreateAppContainerProfile;
            // The function exists if we can reference it — link-time check.
            let _ = CreateAppContainerProfile as usize;
            return Ok(());
        }

        #[cfg(not(target_os = "windows"))]
        Err(SandboxError::EnforcerUnavailable {
            enforcer: "appcontainer".to_string(),
            message: "AppContainer is only available on Windows 8 or later.".to_string(),
        })
    }

    fn wrap_command(
        &self,
        program: &OsStr,
        args: &[OsString],
        policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError> {
        // Windows does NOT wrap the command — it runs the original program directly.
        // AppContainer restrictions are applied via configure_command() below.

        // Still canonicalize paths to catch errors early.
        canonicalize_paths(&policy.allowed_paths)?;

        Ok(WrappedCommand { program: program.to_owned(), args: args.to_vec() })
    }

    fn configure_command(
        &self,
        _cmd: &mut tokio::process::Command,
        _policy: &SandboxPolicy,
    ) -> Result<(), SandboxError> {
        #[cfg(target_os = "windows")]
        {
            // TODO: Full Windows implementation:
            // 1. Derive deterministic container name from policy hash
            // 2. Call CreateAppContainerProfile to create/open the container SID
            // 3. For each AllowedPath, call SetNamedSecurityInfo to grant ACLs
            // 4. If !policy.allow_network, omit INTERNET_CLIENT capability
            // 5. Use UpdateProcThreadAttribute with SECURITY_CAPABILITIES
            //
            // This requires careful Win32 API usage and is deferred to a
            // Windows-specific implementation pass.
            return Err(SandboxError::EnforcerFailed {
                enforcer: "appcontainer".to_string(),
                message: "Windows AppContainer configuration not yet implemented. \
                          The enforcer structure is in place; Win32 API calls \
                          will be added in a Windows-specific implementation pass."
                    .to_string(),
            });
        }

        #[cfg(not(target_os = "windows"))]
        Err(SandboxError::EnforcerUnavailable {
            enforcer: "appcontainer".to_string(),
            message: "AppContainer is only available on Windows.".to_string(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let enforcer = WindowsEnforcer::new();
        assert_eq!(enforcer.name(), "appcontainer");
    }

    #[test]
    fn test_probe_fails_on_non_windows() {
        let enforcer = WindowsEnforcer::new();
        let result = enforcer.probe();
        // On macOS/Linux, probe should fail with EnforcerUnavailable
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, SandboxError::EnforcerUnavailable { .. }),
            "expected EnforcerUnavailable, got: {err:?}"
        );
    }

    #[test]
    fn test_wrap_command_returns_original_program() {
        let enforcer = WindowsEnforcer::new();
        let policy = SandboxPolicy {
            allowed_paths: vec![],
            allow_network: false,
            allow_process_spawn: false,
            env: std::collections::HashMap::new(),
        };

        let wrapped = enforcer
            .wrap_command(
                OsStr::new("python.exe"),
                &[OsString::from("-c"), OsString::from("print(1)")],
                &policy,
            )
            .unwrap();

        assert_eq!(wrapped.program, OsString::from("python.exe"));
        assert_eq!(wrapped.args.len(), 2);
        assert_eq!(wrapped.args[0], OsString::from("-c"));
        assert_eq!(wrapped.args[1], OsString::from("print(1)"));
    }
}
