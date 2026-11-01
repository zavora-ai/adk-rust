//! [`CommandLauncher`] — how `bash` turns a script into a process (spec 3.1 §10 D1).

use adk_core::ToolContext;
use adk_core::egress::ToolEgress;

use crate::error::DevToolError;
use crate::workspace::Workspace;

/// Builds the process that runs one `bash` script.
///
/// [`BashTool`](crate::BashTool) owns everything around the process: the timeout ceiling,
/// the output cap, streaming, standard input, the new session and the process-group kill.
/// A launcher decides only what runs and where: the program and its arguments, the working
/// directory and the environment. That lets a sandbox wrap the script without forking the
/// tool. [`HostLauncher`] is the default and runs `sh -c <script>` on the host.
///
/// The returned command must not set a process group or a session: `BashTool` starts it in
/// a new session (`setsid`), which fails for a process that already leads a group. It also
/// replaces the command's standard streams and sets `kill_on_drop`. On Windows it replaces
/// the command's creation flags with `CREATE_SUSPENDED`, so the process runs only once it is
/// in the call's Job Object (spec 3.2 §10 W1). A Windows launcher whose confinement cannot
/// be set on a command, such as a restricted token, starts the process itself through
/// `spawn_contained` instead (spec 3.2 §10 W5).
///
/// Implements spec 3.1 §10 D1.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::ToolContext;
/// use adk_devtools::{BashTool, CommandLauncher, DevToolError, Workspace};
///
/// /// Runs every script under `nice`, otherwise as the host launcher does.
/// struct NiceLauncher;
///
/// impl CommandLauncher for NiceLauncher {
///     fn command(
///         &self,
///         ws: &Workspace,
///         script: &str,
///         _ctx: &dyn ToolContext,
///     ) -> Result<tokio::process::Command, DevToolError> {
///         let mut cmd = tokio::process::Command::new("nice");
///         cmd.args(["sh", "-c", script]).current_dir(ws.root());
///         if !ws.inherits_env() {
///             cmd.env_clear().envs(ws.bash_env());
///         }
///         Ok(cmd)
///     }
///
///     fn isolation(&self) -> &str {
///         "host-nice"
///     }
/// }
///
/// let workspace = Workspace::new("./my-repo").allow_bash(true);
/// let bash = BashTool::new(workspace).with_launcher(Arc::new(NiceLauncher));
/// # let _ = bash;
/// ```
pub trait CommandLauncher: Send + Sync {
    /// Builds the command that runs `script` for the call described by `ctx`.
    ///
    /// # Errors
    ///
    /// Returns a [`DevToolError`] when the command cannot be built, for example when a
    /// sandbox the launcher requires is unavailable. The call then fails without running
    /// anything.
    fn command(
        &self,
        ws: &Workspace,
        script: &str,
        ctx: &dyn ToolContext,
    ) -> Result<tokio::process::Command, DevToolError>;

    /// A short label for the isolation this launcher provides, such as `"host"`.
    fn isolation(&self) -> &str;

    /// Builds the command as [`command`](Self::command) does, with what must live until its
    /// process has ended, such as the egress proxy the process reaches the network through.
    /// [`BashTool`](crate::BashTool) holds the guard until the process exits. The default keeps
    /// nothing; a wrapping launcher forwards its inner launcher's guard.
    ///
    /// # Errors
    ///
    /// As for [`command`](Self::command).
    fn guarded_command(
        &self,
        ws: &Workspace,
        script: &str,
        ctx: &dyn ToolContext,
    ) -> Result<(tokio::process::Command, Option<LaunchGuard>), DevToolError> {
        Ok((self.command(ws, script, ctx)?, None))
    }

    /// What the commands this launcher builds reach on the network, which `bash` declares as
    /// its [`Tool::egress`](adk_core::Tool::egress). The default,
    /// [`ToolEgress::Undeclared`], is refused under a run egress policy; a wrapping launcher
    /// forwards its inner launcher's.
    fn egress(&self) -> ToolEgress {
        ToolEgress::Undeclared
    }

    /// Starts the process that runs `script` itself, for a launcher whose confinement is set
    /// when the process is created, such as a restricted token on Windows (spec 3.2 §10 W5).
    ///
    /// `None`, the default, has [`BashTool`](crate::BashTool) spawn
    /// [`command`](Self::command)'s result instead. A launcher that returns a child must
    /// have started its process in the child's Job Object, with its standard streams on the
    /// child's pipes and `env` added to its environment last; `env` carries what a wrapping
    /// launcher adds, as it would add it to a command, and `BashTool` passes none. `BashTool`
    /// closes standard input at once and keeps the timeout ceiling, output cap, streaming and
    /// the job's kill around the process, as for a command it spawns. The call runs on Tokio's
    /// blocking pool, since starting a confined process can take long.
    ///
    /// # Errors
    ///
    /// `Some(Err(..))` fails the call without running anything, for example when the
    /// sandbox cannot start the process.
    #[cfg(windows)]
    fn spawn_contained(
        &self,
        ws: &Workspace,
        script: &str,
        env: &[(String, String)],
        ctx: &dyn ToolContext,
    ) -> Option<Result<crate::ContainedChild, DevToolError>> {
        let _ = (ws, script, env, ctx);
        None
    }
}

/// What a [`CommandLauncher`] keeps alive until a command's process has ended
/// ([`CommandLauncher::guarded_command`]).
pub type LaunchGuard = Box<dyn std::any::Any + Send + Sync>;

/// Runs a script on the host as `sh -c <script>`, the default [`CommandLauncher`].
///
/// The working directory is the workspace root, and the environment follows the workspace
/// policy: cleared, then [`Workspace::bash_env`], unless [`Workspace::inherit_env`] is set.
/// The working directory is not an OS boundary; a command can reach absolute paths and the
/// network.
///
/// Implements spec 3.1 §10 D1.
///
/// # Example
///
/// ```rust
/// use adk_devtools::{CommandLauncher, HostLauncher};
///
/// assert_eq!(HostLauncher.isolation(), "host");
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct HostLauncher;

impl CommandLauncher for HostLauncher {
    fn command(
        &self,
        ws: &Workspace,
        script: &str,
        _ctx: &dyn ToolContext,
    ) -> Result<tokio::process::Command, DevToolError> {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(script).current_dir(ws.root());
        // The parent environment of an agent process routinely holds provider API keys,
        // and `env` would print them. Pass through only what tools need, unless the
        // workspace opts into inheriting.
        if !ws.inherits_env() {
            cmd.env_clear();
            for (key, value) in ws.bash_env() {
                cmd.env(key, value);
            }
        }
        Ok(cmd)
    }

    fn isolation(&self) -> &str {
        "host"
    }
}
