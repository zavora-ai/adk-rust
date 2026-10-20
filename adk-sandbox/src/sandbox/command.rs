//! Building a sandboxed [`tokio::process::Command`] from an enforcer and a policy.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use super::{AccessMode, AllowedPath};
use super::{SandboxEnforcer, SandboxPolicy};
use crate::error::SandboxError;

/// Builds a command that runs `program` with `args` under `enforcer` and `policy`, starting in
/// `cwd`.
///
/// Implements spec 3.1 §10 S1. The returned command:
///
/// | Aspect | Value |
/// |--------|-------|
/// | Program | The enforcer's wrapper (`sandbox-exec`, `bwrap`), resolved against the caller's `PATH` |
/// | Arguments | The wrapper arguments from [`SandboxEnforcer::wrap_command`], then `program` and `args` |
/// | Working directory | `cwd` |
/// | Environment | Cleared, then exactly `policy.env` |
/// | Spawn-time state | Whatever [`SandboxEnforcer::configure_command`] installs (the Linux seccomp descriptor) |
///
/// `cwd` is not granted by this function: the enforcers deny every path the policy does not
/// name, so a caller that wants the process to read or write its working directory lists it
/// in `policy.allowed_paths`.
///
/// On macOS, a `program` that resolves through `policy.env`'s `PATH` (default
/// `/usr/bin:/bin`) to an Xcode command-line shim in `/usr/bin`, such as `python3` or `git`,
/// is replaced by the tool behind the shim, and the active developer directory is granted
/// read-only for this command. The shim itself cannot run under Seatbelt.
///
/// Standard streams, process groups, and kill-on-drop are left to the caller.
///
/// # Example
///
/// ```rust,no_run
/// use std::ffi::{OsStr, OsString};
/// use std::path::Path;
///
/// use adk_sandbox::sandbox::{SandboxPolicyBuilder, get_enforcer, sandboxed_command};
///
/// # async fn run() -> Result<(), adk_sandbox::SandboxError> {
/// let workspace = Path::new("/home/me/project");
/// let enforcer = get_enforcer()?;
/// let policy = SandboxPolicyBuilder::new()
///     .allow_read("/usr")
///     .allow_read_write(workspace)
///     .allow_process_spawn()
///     .env("PATH", "/usr/bin:/bin")
///     .build();
///
/// let mut command = sandboxed_command(
///     enforcer.as_ref(),
///     OsStr::new("sh"),
///     &[OsString::from("-c"), OsString::from("ls")],
///     &policy,
///     workspace,
/// )?;
/// let output = command.output().await?;
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns the error from [`SandboxEnforcer::wrap_command`] — `PolicyViolation` when an
/// allowed path cannot be resolved — or from [`SandboxEnforcer::configure_command`].
pub fn sandboxed_command(
    enforcer: &dyn SandboxEnforcer,
    program: &OsStr,
    args: &[OsString],
    policy: &SandboxPolicy,
    cwd: &Path,
) -> Result<tokio::process::Command, SandboxError> {
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut program = program.to_owned();
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut policy = std::borrow::Cow::Borrowed(policy);

    // The program is found through the child's PATH, and an Xcode shim found there cannot run
    // under Seatbelt, so the tool behind it runs instead.
    #[cfg(target_os = "macos")]
    {
        let path_var = policy.env.get("PATH").map_or(OsStr::new(DEFAULT_EXEC_PATH), OsStr::new);
        if let Some(tool) = xcode_shim_target(&program, path_var) {
            policy
                .to_mut()
                .allowed_paths
                .push(AllowedPath { path: tool.developer_dir, mode: AccessMode::ReadOnly });
            program = tool.path.into_os_string();
        }
    }

    let wrapped = enforcer.wrap_command(&program, args, &policy)?;

    // Resolved before `configure_command`, which may attach spawn-time state (the Linux
    // seccomp descriptor, Windows process attributes) that rebuilding the command afterwards
    // would discard.
    let wrapper =
        resolve_program(&wrapped.program).map_or(wrapped.program, PathBuf::into_os_string);
    let mut command = tokio::process::Command::new(wrapper);
    command.args(&wrapped.args).current_dir(cwd).env_clear().envs(&policy.env);
    enforcer.configure_command(&mut command, &policy)?;
    Ok(command)
}

/// Resolves a bare program name to an absolute path using the caller's `PATH`.
///
/// Returns `None` when the name already contains a path separator, or when nothing on
/// `PATH` matches — in which case the command is left as it was so the spawn error still
/// names the program the caller asked for.
pub(crate) fn resolve_program(program: &OsStr) -> Option<PathBuf> {
    let as_path = Path::new(program);
    if as_path.components().count() > 1 {
        return None;
    }

    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(program);
        candidate.is_file().then_some(candidate)
    })
}

/// The default search path `execvp` uses when the environment has no `PATH`.
#[cfg(target_os = "macos")]
pub(crate) const DEFAULT_EXEC_PATH: &str = "/usr/bin:/bin";

/// The tool an Xcode command-line shim runs, and the developer directory that holds it.
///
/// `/usr/bin/python3`, `git`, `clang`, and the other shims find the active Xcode or Command
/// Line Tools through `libxcrun`, which loads from the developer directory and caches in the
/// user's temporary directory, so a shim fails under Seatbelt. The tool itself needs only to
/// read the developer directory, which holds Apple's toolchain and no user data.
#[cfg(target_os = "macos")]
#[derive(Clone)]
struct XcodeTool {
    path: PathBuf,
    developer_dir: PathBuf,
}

/// Resolves `program` as `execvp` would through `path_var`, and returns the tool behind it
/// when that is an Xcode shim in `/usr/bin`.
///
/// Results are cached per shim path, since each check runs `xcrun`.
#[cfg(target_os = "macos")]
fn xcode_shim_target(program: &OsStr, path_var: &OsStr) -> Option<XcodeTool> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CHECKED: OnceLock<Mutex<HashMap<PathBuf, Option<XcodeTool>>>> = OnceLock::new();

    let as_path = Path::new(program);
    let resolved = if as_path.components().count() > 1 {
        as_path.to_path_buf()
    } else {
        std::env::split_paths(path_var)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())?
    };
    if resolved.parent() != Some(Path::new("/usr/bin")) {
        return None;
    }
    let checked = CHECKED.get_or_init(Mutex::default);
    if let Some(known) = checked.lock().unwrap_or_else(|e| e.into_inner()).get(&resolved) {
        return known.clone();
    }

    let tool = find_xcode_tool(&resolved);
    if let Some(tool) = &tool {
        tracing::debug!(
            sandbox.program = %resolved.display(),
            sandbox.resolved = %tool.path.display(),
            "running the tool behind an Xcode shim directly"
        );
    }
    checked.lock().unwrap_or_else(|e| e.into_inner()).insert(resolved, tool.clone());
    tool
}

/// Asks `xcrun` for the tool behind the shim at `shim`, and `xcode-select` for the developer
/// directory it must be inside.
#[cfg(target_os = "macos")]
fn find_xcode_tool(shim: &Path) -> Option<XcodeTool> {
    let run = |command: &str, args: &[&OsStr]| {
        std::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
    };
    let path = run("/usr/bin/xcrun", &[OsStr::new("--find"), shim.file_name()?])?;
    let developer_dir = run("/usr/bin/xcode-select", &[OsStr::new("--print-path")])?;
    let (real, developer_dir) = (path.canonicalize().ok()?, developer_dir.canonicalize().ok()?);
    // A tool `xcrun` finds on `PATH` instead, such as `/usr/bin/ruby`, is not a shim.
    (real.starts_with(&developer_dir) && path != shim).then_some(XcodeTool { path, developer_dir })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::sandbox::{SandboxPolicyBuilder, WrappedCommand};

    /// Records what it was asked to wrap and prefixes a fixed wrapper.
    #[derive(Default)]
    struct RecordingEnforcer {
        wrapped: Mutex<Option<(OsString, Vec<OsString>, SandboxPolicy)>>,
        configured: Mutex<Option<SandboxPolicy>>,
        /// When set, the program runs unwrapped, so a test can execute the result.
        passthrough: bool,
    }

    impl SandboxEnforcer for RecordingEnforcer {
        fn name(&self) -> &str {
            "recording"
        }

        fn probe(&self) -> Result<(), SandboxError> {
            Ok(())
        }

        fn wrap_command(
            &self,
            program: &OsStr,
            args: &[OsString],
            policy: &SandboxPolicy,
        ) -> Result<WrappedCommand, SandboxError> {
            *self.wrapped.lock().unwrap() =
                Some((program.to_owned(), args.to_vec(), policy.clone()));
            if self.passthrough {
                return Ok(WrappedCommand { program: program.to_owned(), args: args.to_vec() });
            }
            let mut wrapped_args = vec![OsString::from("--wrapper-flag"), OsString::from("--")];
            wrapped_args.push(program.to_owned());
            wrapped_args.extend_from_slice(args);
            Ok(WrappedCommand { program: OsString::from("/opt/test/wrapper"), args: wrapped_args })
        }

        fn configure_command(
            &self,
            cmd: &mut tokio::process::Command,
            policy: &SandboxPolicy,
        ) -> Result<(), SandboxError> {
            *self.configured.lock().unwrap() = Some(policy.clone());
            cmd.env("CONFIGURED_BY_ENFORCER", "1");
            Ok(())
        }
    }

    /// An enforcer whose `wrap_command` fails, as a policy with a missing path does.
    struct FailingEnforcer;

    impl SandboxEnforcer for FailingEnforcer {
        fn name(&self) -> &str {
            "failing"
        }

        fn probe(&self) -> Result<(), SandboxError> {
            Ok(())
        }

        fn wrap_command(
            &self,
            _program: &OsStr,
            _args: &[OsString],
            _policy: &SandboxPolicy,
        ) -> Result<WrappedCommand, SandboxError> {
            Err(SandboxError::PolicyViolation("missing path".to_string()))
        }
    }

    fn os(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_command_is_the_wrapped_argv_in_the_given_working_directory() {
        let enforcer = RecordingEnforcer::default();
        let workspace = Path::new("/srv/workspace");
        let policy = SandboxPolicyBuilder::new()
            .allow_read_write(workspace)
            .env("PATH", "/opt/test/bin")
            .build();

        let command = sandboxed_command(
            &enforcer,
            OsStr::new("/opt/test/bin/tool"),
            &os(&["-c", "echo hi"]),
            &policy,
            workspace,
        )
        .expect("the command builds");
        let std_command = command.as_std();

        assert_eq!(std_command.get_program(), OsStr::new("/opt/test/wrapper"));
        assert_eq!(
            std_command.get_args().collect::<Vec<_>>(),
            ["--wrapper-flag", "--", "/opt/test/bin/tool", "-c", "echo hi"]
        );
        assert_eq!(std_command.get_current_dir(), Some(workspace));
        let recorded = enforcer.wrapped.lock().unwrap().clone().expect("wrap_command ran");
        assert_eq!(
            recorded,
            (OsString::from("/opt/test/bin/tool"), os(&["-c", "echo hi"]), policy)
        );
    }

    #[test]
    fn the_environment_is_the_policy_environment_then_the_enforcer_configuration() {
        let enforcer = RecordingEnforcer::default();
        let policy = SandboxPolicyBuilder::new()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", "/srv/scratch")
            .env("TMPDIR", "/srv/scratch/tmp")
            .build();

        let command =
            sandboxed_command(&enforcer, OsStr::new("/bin/sh"), &[], &policy, Path::new("/"))
                .expect("the command builds");

        let environment: HashMap<String, Option<String>> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let expected: HashMap<String, Option<String>> = [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/srv/scratch"),
            ("TMPDIR", "/srv/scratch/tmp"),
            ("CONFIGURED_BY_ENFORCER", "1"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), Some(value.to_string())))
        .collect();
        assert_eq!(environment, expected);
        assert!(enforcer.configured.lock().unwrap().is_some(), "configure_command ran");
    }

    #[test]
    fn a_wrap_failure_is_returned() {
        let result = sandboxed_command(
            &FailingEnforcer,
            OsStr::new("sh"),
            &[],
            &SandboxPolicyBuilder::new().build(),
            Path::new("/"),
        );
        assert!(matches!(result, Err(SandboxError::PolicyViolation(_))), "{result:?}");
    }

    /// Spawning the command proves what `get_envs` cannot show: the parent environment is
    /// cleared, and the process starts in `cwd`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_spawned_process_sees_only_the_policy_environment_in_cwd() {
        let enforcer = RecordingEnforcer { passthrough: true, ..Default::default() };
        let directory = tempfile::tempdir().expect("tempdir");
        let cwd = std::fs::canonicalize(directory.path()).expect("canonicalize");
        let policy = SandboxPolicyBuilder::new().env("ONLY_THIS", "visible").build();

        let output = sandboxed_command(
            &enforcer,
            OsStr::new("/bin/sh"),
            &os(&["-c", "/bin/pwd; /usr/bin/env"]),
            &policy,
            &cwd,
        )
        .expect("the command builds")
        .output()
        .await
        .expect("the command runs");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        assert_eq!(lines.next(), Some(cwd.to_str().expect("utf-8 path")), "{stdout}");
        let mut environment: Vec<&str> =
            // The shell exports these itself.
            lines.filter(|line| !["PWD=", "SHLVL=", "_="].iter().any(|own| line.starts_with(own))).collect();
        environment.sort_unstable();
        assert_eq!(environment, ["CONFIGURED_BY_ENFORCER=1", "ONLY_THIS=visible"], "{stdout}");
    }

    #[test]
    fn a_bare_wrapper_name_resolves_on_the_caller_path() {
        assert_eq!(resolve_program(OsStr::new("/already/absolute")), None);
        assert_eq!(resolve_program(OsStr::new("no-such-program-adk-sandbox")), None);
        #[cfg(unix)]
        {
            let resolved = resolve_program(OsStr::new("sh")).expect("sh is on PATH");
            assert!(resolved.is_absolute(), "{resolved:?}");
        }
    }

    /// When the requested program is an Xcode shim, the tool behind it is wrapped and the
    /// developer directory is granted for this command only.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_xcode_shim_is_replaced_by_its_tool() {
        let Some(tool) = xcode_shim_target(OsStr::new("git"), OsStr::new(DEFAULT_EXEC_PATH)) else {
            eprintln!("no Xcode developer directory on this host; nothing to assert");
            return;
        };
        let enforcer = RecordingEnforcer::default();
        let policy = SandboxPolicyBuilder::new().build();

        sandboxed_command(&enforcer, OsStr::new("git"), &[], &policy, Path::new("/"))
            .expect("the command builds");

        let (program, _, wrapped_policy) =
            enforcer.wrapped.lock().unwrap().clone().expect("wrap_command ran");
        assert_eq!(program, tool.path.into_os_string());
        assert_eq!(
            wrapped_policy.allowed_paths,
            [AllowedPath { path: tool.developer_dir, mode: AccessMode::ReadOnly }]
        );
        assert!(policy.allowed_paths.is_empty(), "the caller's policy is not modified");
    }
}
