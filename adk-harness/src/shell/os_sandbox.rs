//! The OS-sandbox shell launcher (spec 3.1 §7.9, feature `sandbox`).
//!
//! `bash` under the OS sandbox runs `sh -c <script>` through
//! `adk_sandbox::sandbox::sandboxed_command` (spec 3.1 §10 S1) as the launcher of the
//! devtools `BashTool` (D1), which keeps its timeout ceiling, output cap, streaming,
//! `setsid`, empty standard input and process-group kill (D3).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::ToolContext;
use adk_devtools::{CommandLauncher, DEFAULT_ENV_ALLOWLIST, DevToolError, Workspace};
use adk_sandbox::SandboxError;
use adk_sandbox::sandbox::{
    AccessMode, AllowedPath, SandboxEnforcer, SandboxPolicy, SandboxPolicyBuilder, WrappedCommand,
    sandboxed_command,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::IsolationStatus;
use super::protect::protected_paths;

/// Configuration of the OS-sandbox shell (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// use adk_harness::OsSandboxConfig;
///
/// let config = OsSandboxConfig::default();
/// assert!(!config.network);
/// assert!(config.allow_process_spawn);
/// assert!(config.read_toolchains);
/// ```
#[derive(Clone)]
pub struct OsSandboxConfig {
    /// Allow network access. `false`: adk-sandbox cannot filter domains.
    pub network: bool,
    /// Allow the sandboxed shell to fork. `true`: `sh -c` forks.
    pub allow_process_spawn: bool,
    /// Extra read-only paths.
    pub extra_read: Vec<PathBuf>,
    /// Extra read-write paths.
    pub extra_write: Vec<PathBuf>,
    /// Read-only bind of the toolchain directories (`PATH`, `~/.cargo`, `~/.rustup`, …).
    pub read_toolchains: bool,
    /// The enforcer; `None` uses `adk_sandbox::sandbox::get_enforcer()`.
    pub enforcer: Option<Arc<dyn SandboxEnforcer>>,
}

impl Default for OsSandboxConfig {
    fn default() -> Self {
        Self {
            network: false,
            allow_process_spawn: true,
            extra_read: Vec::new(),
            extra_write: Vec::new(),
            read_toolchains: true,
            enforcer: None,
        }
    }
}

impl fmt::Debug for OsSandboxConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsSandboxConfig")
            .field("network", &self.network)
            .field("allow_process_spawn", &self.allow_process_spawn)
            .field("extra_read", &self.extra_read)
            .field("extra_write", &self.extra_write)
            .field("read_toolchains", &self.read_toolchains)
            .field("enforcer", &self.enforcer.as_ref().map(|enforcer| enforcer.name().to_string()))
            .finish()
    }
}

/// The workspace access a sandboxed command gets (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// use adk_harness::SandboxAccess;
///
/// assert_eq!(serde_json::to_value(SandboxAccess::ReadOnly).unwrap(), "read_only");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxAccess {
    /// The workspace root read-write, its protected paths read-only.
    ReadWrite,
    /// The workspace root read-only, for Plan and ReadOnly modes.
    ReadOnly,
}

/// Environment variables never passed into the sandbox, whatever the workspace allowlist
/// says: agent sockets (spec 3.1 §9.2 I22).
const AGENT_SOCKETS: &[&str] = &["SSH_AUTH_SOCK", "SSH_AGENT_PID", "GPG_AGENT_INFO"];
/// Name fragments of credential variables never passed into the sandbox (I22).
const CREDENTIAL_MARKERS: &[&str] =
    &["TOKEN", "SECRET", "PASSWORD", "PASSWD", "API_KEY", "CREDENTIAL"];

/// Whether a variable may pass into the sandbox: not an agent socket, not named like a
/// credential (spec 3.1 §9.2 I22).
fn passes_into_sandbox(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    !AGENT_SOCKETS.contains(&upper.as_str())
        && !CREDENTIAL_MARKERS.iter().any(|marker| upper.contains(marker))
}

/// Builds sandboxed shell commands (spec 3.1 §7.9).
///
/// Each command is `sh -c <script>` under the configured enforcer (Seatbelt on macOS,
/// bubblewrap on Linux), started in the workspace root, with:
///
/// | | Read-write ([`SandboxAccess::ReadWrite`]) | Read-only ([`SandboxAccess::ReadOnly`]) |
/// |---|---|---|
/// | Workspace root | read-write; its existing protected paths read-only | read-only |
/// | Per-session scratch (`HOME`, `TMPDIR`) | read-write | read-write |
/// | Toolchains (`read_toolchains`) | read-only | read-only |
/// | Network | `network` (off by default) | `network` (off by default) |
///
/// The environment is the workspace's `bash` allowlist with `HOME` and `TMPDIR` set to the
/// scratch directory and no agent sockets or credential variables (spec 3.1 §9.2 I22).
/// With `read_toolchains`, `CARGO_HOME`, `RUSTUP_HOME`, `PYENV_ROOT` and `NVM_DIR` keep
/// pointing at the real toolchain directories, and on macOS `DEVELOPER_DIR`, `SDKROOT` and
/// the active developer directory's tool directories on `PATH` let compilers and `git` run
/// without the Xcode shims, which cannot run under Seatbelt.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_devtools::{BashTool, Workspace};
/// use adk_harness::{IsolationStatus, OsSandboxConfig, SandboxLauncher};
///
/// let launcher = SandboxLauncher::new(OsSandboxConfig::default());
/// assert!(matches!(launcher.status(), IsolationStatus::OsSandbox { .. }));
/// let bash = BashTool::new(Workspace::new(".").allow_bash(true)).with_launcher(Arc::new(launcher));
/// # let _ = bash;
/// ```
#[derive(Clone)]
pub struct SandboxLauncher {
    config: OsSandboxConfig,
    enforcer: Result<Arc<dyn SandboxEnforcer>, String>,
    access: SandboxAccess,
    label: String,
}

impl fmt::Debug for SandboxLauncher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SandboxLauncher")
            .field("config", &self.config)
            .field("access", &self.access)
            .field("enforcer", &self.label)
            .finish()
    }
}

impl SandboxLauncher {
    /// A read-write launcher for `config`, resolving its enforcer
    /// (`adk_sandbox::sandbox::get_enforcer()` when the config names none).
    pub fn new(config: OsSandboxConfig) -> Self {
        let enforcer = match &config.enforcer {
            Some(enforcer) => Ok(enforcer.clone()),
            None => adk_sandbox::sandbox::get_enforcer()
                .map(Arc::from)
                .map_err(|error| format!("no sandbox enforcer is available: {error}")),
        };
        let label = match &enforcer {
            Ok(enforcer) => enforcer.name().to_string(),
            Err(_) => "os_sandbox".to_string(),
        };
        Self { config, enforcer, access: SandboxAccess::ReadWrite, label }
    }

    /// This launcher with a read-only workspace (Plan and ReadOnly modes).
    #[must_use]
    pub fn read_only(&self) -> Self {
        Self { access: SandboxAccess::ReadOnly, ..self.clone() }
    }

    /// The workspace access of the commands this launcher builds.
    pub fn access(&self) -> SandboxAccess {
        self.access
    }

    /// The configuration.
    pub fn config(&self) -> &OsSandboxConfig {
        &self.config
    }

    /// Probes the enforcer: [`IsolationStatus::OsSandbox`] when it works,
    /// [`IsolationStatus::Degraded`] with the reason otherwise (spec 3.1 §7.9
    /// "Availability").
    pub fn status(&self) -> IsolationStatus {
        let reason = match &self.enforcer {
            Ok(enforcer) => match enforcer.probe() {
                Ok(()) => {
                    return IsolationStatus::OsSandbox { enforcer: enforcer.name().to_string() };
                }
                Err(error) => error.to_string(),
            },
            Err(reason) => reason.clone(),
        };
        IsolationStatus::Degraded { requested: "os_sandbox".to_string(), reason }
    }

    /// Digest of the sandbox configuration: enforcer, network, process spawning, toolchain
    /// reads and extra paths, as `sha256:<hex>` (spec 3.1 §9.2 I9).
    ///
    /// The `bash` declaration's annotations carry it, so standing rules and held calls bound
    /// to one sandbox configuration never match another, nor the host shell.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{OsSandboxConfig, SandboxLauncher};
    ///
    /// let closed = SandboxLauncher::new(OsSandboxConfig::default());
    /// let open = SandboxLauncher::new(OsSandboxConfig { network: true, ..OsSandboxConfig::default() });
    /// assert_ne!(closed.profile_digest(), open.profile_digest());
    /// ```
    pub fn profile_digest(&self) -> String {
        let mut extra_read: Vec<String> =
            self.config.extra_read.iter().map(|path| path.display().to_string()).collect();
        let mut extra_write: Vec<String> =
            self.config.extra_write.iter().map(|path| path.display().to_string()).collect();
        extra_read.sort();
        extra_write.sort();
        let profile = json!({
            "enforcer": self.label,
            "network": self.config.network,
            "allowProcessSpawn": self.config.allow_process_spawn,
            "readToolchains": self.config.read_toolchains,
            "extraRead": extra_read,
            "extraWrite": extra_write,
        });
        format!("sha256:{}", crate::fsutil::sha256_hex(&profile.to_string()))
    }

    /// The per-session scratch directory: `HOME` and `TMPDIR` inside the sandbox.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the directory cannot be created.
    pub fn scratch_dir(&self, app: &str, user: &str, session: &str) -> std::io::Result<PathBuf> {
        let key = crate::fsutil::sha256_hex(&format!("{app}\u{1f}{user}\u{1f}{session}"));
        let dir = std::env::temp_dir().join("adk-harness-shell").join(&key[..32]);
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        std::fs::canonicalize(&dir)
    }

    /// The environment a sandboxed command gets for `ws` with `scratch` as its home
    /// (spec 3.1 §7.9, §9.2 I22).
    pub fn environment(&self, ws: &Workspace, scratch: &Path) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = if ws.inherits_env() {
            DEFAULT_ENV_ALLOWLIST
                .iter()
                .filter_map(|key| std::env::var(key).ok().map(|value| ((*key).to_string(), value)))
                .collect()
        } else {
            ws.bash_env().into_iter().collect()
        };
        env.retain(|key, _| passes_into_sandbox(key));
        let scratch = scratch.display().to_string();
        env.insert("HOME".to_string(), scratch.clone());
        env.insert("TMPDIR".to_string(), scratch);
        if self.config.read_toolchains {
            let toolchains = Toolchains::detect(env.get("PATH").map(String::as_str), ws.root());
            for (key, value) in toolchains.env {
                env.insert(key, value);
            }
            if !toolchains.path_prefix.is_empty() {
                let rest = env.get("PATH").cloned().unwrap_or_else(|| "/usr/bin:/bin".to_string());
                let mut dirs = toolchains.path_prefix;
                dirs.extend(std::env::split_paths(&rest));
                if let Ok(joined) = std::env::join_paths(dirs) {
                    env.insert("PATH".to_string(), joined.to_string_lossy().into_owned());
                }
            }
        }
        env
    }

    /// The sandbox policy of a command over `ws` with `scratch` as its home.
    ///
    /// Only paths that exist are listed, since an enforcer refuses a policy naming a
    /// missing path.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_harness::{OsSandboxConfig, SandboxLauncher};
    ///
    /// let launcher = SandboxLauncher::new(OsSandboxConfig::default());
    /// let ws = adk_devtools::Workspace::new(".").allow_bash(true);
    /// let scratch = launcher.scratch_dir("app", "alice", "s1")?;
    /// let policy = launcher.policy(&ws, &scratch);
    /// assert!(!policy.allow_network);
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn policy(&self, ws: &Workspace, scratch: &Path) -> SandboxPolicy {
        let mut builder = SandboxPolicyBuilder::new();
        for (key, value) in self.environment(ws, scratch) {
            builder = builder.env(key, value);
        }
        if self.config.allow_process_spawn {
            builder = builder.allow_process_spawn();
        }
        if self.config.network {
            builder = builder.allow_network();
        }
        let mut policy = builder.build();
        let path_var = policy.env.get("PATH").cloned();
        let mut grant = |path: &Path, mode: AccessMode| {
            if path.exists() {
                policy.allowed_paths.push(AllowedPath { path: path.to_path_buf(), mode });
            }
        };
        let root = ws.root();
        match self.access {
            SandboxAccess::ReadWrite => {
                grant(root, AccessMode::ReadWrite);
                for protected in protected_paths(root).present {
                    grant(&protected, AccessMode::ReadOnly);
                }
            }
            SandboxAccess::ReadOnly => grant(root, AccessMode::ReadOnly),
        }
        grant(scratch, AccessMode::ReadWrite);
        for path in &self.config.extra_read {
            grant(path, AccessMode::ReadOnly);
        }
        for path in &self.config.extra_write {
            grant(path, AccessMode::ReadWrite);
        }
        if self.config.read_toolchains {
            for path in Toolchains::detect(path_var.as_deref(), root).read {
                grant(&path, AccessMode::ReadOnly);
            }
        }
        policy
    }

    /// The enforcer that builds commands: on bubblewrap, one that also recreates
    /// symlinked toolchain directories as symlinks and mounts `/dev` and `/proc`.
    fn command_enforcer(
        &self,
        enforcer: &Arc<dyn SandboxEnforcer>,
        policy: &SandboxPolicy,
        root: &Path,
    ) -> Arc<dyn SandboxEnforcer> {
        if enforcer.name() != "bubblewrap" {
            return enforcer.clone();
        }
        let links = if self.config.read_toolchains {
            Toolchains::detect(policy.env.get("PATH").map(String::as_str), root).links
        } else {
            Vec::new()
        };
        Arc::new(LinkingEnforcer { inner: enforcer.clone(), links })
    }

    /// Whether the enforcer keeps a read-only path inside a read-write one read-only
    /// (adk-sandbox S3), proven by running a conformance script under it.
    ///
    /// # Errors
    ///
    /// Returns why the enforcer fails the check: unavailable, a write that went through,
    /// or ordinary work that did not.
    pub(crate) fn nested_read_only_conformance(&self) -> Result<(), String> {
        let enforcer = self.enforcer.clone()?;
        let scratch =
            tempfile_dir().map_err(|error| format!("no conformance directory: {error}"))?;
        let ws = scratch.join("ws");
        for dir in [".git/hooks", "sub/.git"] {
            std::fs::create_dir_all(ws.join(dir)).map_err(|error| error.to_string())?;
        }
        for (file, content) in [(".git/config", "c"), ("AGENTS.md", "a"), ("sub/.git/config", "s")]
        {
            std::fs::write(ws.join(file), content).map_err(|error| error.to_string())?;
        }
        let home = scratch.join("home");
        std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
        let workspace = Workspace::new(&ws).allow_bash(true);
        // The probe judges the enforcer, so the configured extra paths stay out.
        let probe = Self {
            config: OsSandboxConfig {
                extra_read: Vec::new(),
                extra_write: Vec::new(),
                ..self.config.clone()
            },
            access: SandboxAccess::ReadWrite,
            ..self.clone()
        };
        let policy = probe.policy(&workspace, &home);
        let script = "\
            for attempt in 'echo x > AGENTS.md' 'echo x > .git/config' 'touch .git/hooks/pre-commit' \
              'mv .git .moved' 'rm AGENTS.md' 'mv sub sub2' 'echo x > sub/.git/config' 'ln -s x .git/link'; do \
              if sh -c \"$attempt\" 2>/dev/null; then echo \"BREACH $attempt\"; fi; \
            done; echo ok > beside.txt && echo DONE";
        let command = sandboxed_command(
            self.command_enforcer(&enforcer, &policy, &ws).as_ref(),
            OsStr::new("sh"),
            &[OsString::from("-c"), OsString::from(script)],
            &policy,
            &ws,
        );
        let result = command.map_err(|error| error.to_string()).and_then(|mut command| {
            command
                .as_std_mut()
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|error| error.to_string())
        });
        let _ = std::fs::remove_dir_all(&scratch);
        let output = result?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(breach) = stdout.lines().find(|line| line.starts_with("BREACH")) {
            return Err(format!(
                "the {} enforcer let a sandboxed command write a protected path ({})",
                enforcer.name(),
                breach.trim_start_matches("BREACH ")
            ));
        }
        if !stdout.contains("DONE") {
            return Err(format!(
                "the {} enforcer conformance script did not complete: {}",
                enforcer.name(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

/// A fresh private directory for the conformance script.
fn tempfile_dir() -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir()
        .join("adk-harness-shell")
        .join(format!("conformance-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir)?;
    std::fs::canonicalize(dir)
}

impl CommandLauncher for SandboxLauncher {
    fn command(
        &self,
        ws: &Workspace,
        script: &str,
        ctx: &dyn ToolContext,
    ) -> Result<tokio::process::Command, DevToolError> {
        let enforcer = self.enforcer.clone().map_err(DevToolError::Other)?;
        let scratch = self.scratch_dir(ctx.app_name(), ctx.user_id(), ctx.session_id())?;
        let policy = self.policy(ws, &scratch);
        sandboxed_command(
            self.command_enforcer(&enforcer, &policy, ws.root()).as_ref(),
            OsStr::new("sh"),
            &[OsString::from("-c"), OsString::from(script)],
            &policy,
            ws.root(),
        )
        .map_err(|error| {
            DevToolError::Other(format!("the sandboxed command could not be built: {error}"))
        })
    }

    fn isolation(&self) -> &str {
        &self.label
    }
}

/// The toolchain directories a sandboxed command may read (spec 3.1 §7.9 `read_toolchains`).
#[derive(Debug, Default)]
struct Toolchains {
    /// Directories granted read-only.
    read: Vec<PathBuf>,
    /// Symlinked toolchain paths, as (link text, link path), for enforcers that rebuild the
    /// file system (bubblewrap).
    links: Vec<(OsString, PathBuf)>,
    /// Variables that keep toolchains working with `HOME` moved to the scratch directory.
    env: Vec<(String, String)>,
    /// Directories put first on `PATH`.
    path_prefix: Vec<PathBuf>,
}

impl Toolchains {
    fn detect(path_var: Option<&str>, root: &Path) -> Self {
        let home = dirs::home_dir();
        let mut found = Self::default();
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(path_var) = path_var {
            candidates.extend(
                std::env::split_paths(path_var)
                    .filter(|dir| path_dir_is_grantable(dir, home.as_deref(), root)),
            );
        }
        let home_dir = |var: &str, default: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(default)))
        };
        for (var, default) in [
            ("CARGO_HOME", ".cargo"),
            ("RUSTUP_HOME", ".rustup"),
            ("PYENV_ROOT", ".pyenv"),
            ("NVM_DIR", ".nvm"),
        ] {
            if let Some(dir) = home_dir(var, default).filter(|dir| dir.is_dir()) {
                found.env.push((var.to_string(), dir.display().to_string()));
                candidates.push(dir);
            }
        }
        candidates.extend(["/opt/homebrew", "/usr/local"].map(PathBuf::from));
        if cfg!(target_os = "linux") {
            candidates.extend(["/usr", "/bin", "/lib", "/lib64", "/etc"].map(PathBuf::from));
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(developer) = macos_developer() {
                found.env.push(("DEVELOPER_DIR".to_string(), developer.dir.display().to_string()));
                if let Some(sdk) = &developer.sdk {
                    found.env.push(("SDKROOT".to_string(), sdk.display().to_string()));
                    candidates.push(sdk.clone());
                }
                found.path_prefix = developer.bin_dirs.clone();
                candidates.push(developer.dir.clone());
            }
        }
        for candidate in candidates {
            let Ok(metadata) = std::fs::symlink_metadata(&candidate) else { continue };
            if metadata.file_type().is_symlink() {
                let (Ok(text), Ok(target)) =
                    (std::fs::read_link(&candidate), std::fs::canonicalize(&candidate))
                else {
                    continue;
                };
                found.read.push(target);
                found.links.push((text.into_os_string(), candidate));
            } else {
                found.read.push(candidate);
            }
        }
        // A link inside a directory that is itself granted already exists there.
        let read = found.read.clone();
        let links: Vec<PathBuf> = found.links.iter().map(|(_, link)| link.clone()).collect();
        found.links.retain(|(_, link)| {
            !read.iter().chain(&links).any(|dir| link != dir && link.starts_with(dir))
        });
        found.read.sort();
        found.read.dedup();
        found
    }
}

/// Whether a `PATH` directory may be granted read-only: absolute, not `/`, not the home
/// directory or one above it, and not one that holds the workspace (a projects directory
/// on `PATH` would expose every other project's files).
fn path_dir_is_grantable(dir: &Path, home: Option<&Path>, root: &Path) -> bool {
    dir.is_absolute()
        && dir.parent().is_some()
        && !home.is_some_and(|home| home.starts_with(dir))
        && !root.starts_with(dir)
        && !dir.starts_with(root)
}

/// The active Xcode or Command Line Tools developer directory.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct MacDeveloper {
    dir: PathBuf,
    sdk: Option<PathBuf>,
    bin_dirs: Vec<PathBuf>,
}

/// Asks `xcode-select` and `xcrun`, once, for the developer directory, the SDK and the
/// directories that hold the real compilers and `git`. The `/usr/bin` shims run `xcrun`,
/// which writes a cache to the per-user temporary directory and loads Xcode's private
/// frameworks, so they fail under Seatbelt; the tools behind them do not.
#[cfg(target_os = "macos")]
fn macos_developer() -> Option<MacDeveloper> {
    use std::sync::OnceLock;
    static DEVELOPER: OnceLock<Option<MacDeveloper>> = OnceLock::new();
    DEVELOPER
        .get_or_init(|| {
            let run = |program: &str, args: &[&str]| {
                std::process::Command::new(program)
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output()
                    .ok()
                    .filter(|output| output.status.success())
                    .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
                    .and_then(|path| std::fs::canonicalize(path).ok())
            };
            let dir = run("/usr/bin/xcode-select", &["--print-path"])?;
            let sdk = run("/usr/bin/xcrun", &["--show-sdk-path"]);
            let bin_dirs =
                [dir.join("Toolchains/XcodeDefault.xctoolchain/usr/bin"), dir.join("usr/bin")]
                    .into_iter()
                    .filter(|bin| bin.is_dir())
                    .collect();
            Some(MacDeveloper { dir, sdk, bin_dirs })
        })
        .clone()
}

/// Bubblewrap with toolchain symlinks and the `/dev` and `/proc` mounts the shell needs.
///
/// bubblewrap starts from an empty root. On a merged-`/usr` system `/bin`, `/lib`, `/lib64`
/// and `/sbin` are symlinks into `/usr`; binding them resolves the link and mounts the target
/// at the target's path, so the link path — and the dynamic loader's path — would be
/// missing inside. Each symlinked toolchain path is recreated with `--symlink` instead.
struct LinkingEnforcer {
    inner: Arc<dyn SandboxEnforcer>,
    links: Vec<(OsString, PathBuf)>,
}

impl SandboxEnforcer for LinkingEnforcer {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn probe(&self) -> Result<(), SandboxError> {
        self.inner.probe()
    }

    fn wrap_command(
        &self,
        program: &OsStr,
        args: &[OsString],
        policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError> {
        let mut wrapped = self.inner.wrap_command(program, args, policy)?;
        let separator =
            wrapped.args.iter().position(|arg| arg == "--").unwrap_or(wrapped.args.len());
        let mut extra: Vec<OsString> =
            ["--dev", "/dev", "--proc", "/proc"].into_iter().map(OsString::from).collect();
        for (text, link) in &self.links {
            extra.push(OsString::from("--symlink"));
            extra.push(text.clone());
            extra.push(link.clone().into_os_string());
        }
        wrapped.args.splice(separator..separator, extra);
        Ok(wrapped)
    }

    fn configure_command(
        &self,
        cmd: &mut tokio::process::Command,
        policy: &SandboxPolicy,
    ) -> Result<(), SandboxError> {
        self.inner.configure_command(cmd, policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeBwrap;

    impl SandboxEnforcer for FakeBwrap {
        fn name(&self) -> &str {
            "bubblewrap"
        }
        fn probe(&self) -> Result<(), SandboxError> {
            Ok(())
        }
        fn wrap_command(
            &self,
            program: &OsStr,
            args: &[OsString],
            _policy: &SandboxPolicy,
        ) -> Result<WrappedCommand, SandboxError> {
            let mut wrapped: Vec<OsString> =
                ["--ro-bind", "/usr", "/usr", "--"].into_iter().map(OsString::from).collect();
            wrapped.push(program.to_owned());
            wrapped.extend_from_slice(args);
            Ok(WrappedCommand { program: "bwrap".into(), args: wrapped })
        }
    }

    #[test]
    fn bubblewrap_gets_symlinks_and_device_mounts_before_the_separator() {
        let enforcer = LinkingEnforcer {
            inner: Arc::new(FakeBwrap),
            links: vec![
                (OsString::from("usr/bin"), PathBuf::from("/bin")),
                (OsString::from("usr/lib64"), PathBuf::from("/lib64")),
            ],
        };
        let wrapped = enforcer
            .wrap_command(
                OsStr::new("sh"),
                &[OsString::from("-c"), OsString::from("true")],
                &SandboxPolicyBuilder::new().build(),
            )
            .unwrap();
        let args: Vec<String> =
            wrapped.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            [
                "--ro-bind",
                "/usr",
                "/usr",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--symlink",
                "usr/bin",
                "/bin",
                "--symlink",
                "usr/lib64",
                "/lib64",
                "--",
                "sh",
                "-c",
                "true",
            ]
        );
    }

    #[test]
    fn path_directories_that_hold_home_or_the_workspace_are_not_granted() {
        let home = Path::new("/Users/me");
        let root = Path::new("/Users/me/Developer/project");
        assert!(path_dir_is_grantable(Path::new("/Users/me/.cargo/bin"), Some(home), root));
        assert!(path_dir_is_grantable(Path::new("/opt/homebrew/bin"), Some(home), root));
        assert!(!path_dir_is_grantable(Path::new("/Users/me/Developer"), Some(home), root));
        assert!(!path_dir_is_grantable(Path::new("/Users/me"), Some(home), root));
        assert!(!path_dir_is_grantable(Path::new("/Users"), Some(home), root));
        assert!(!path_dir_is_grantable(Path::new("/"), Some(home), root));
        assert!(!path_dir_is_grantable(Path::new("bin"), Some(home), root));
        assert!(!path_dir_is_grantable(root, Some(home), root));
        assert!(!path_dir_is_grantable(&root.join("node_modules/.bin"), Some(home), root));
    }

    #[test]
    fn agent_sockets_and_credentials_never_reach_the_sandbox() {
        for key in [
            "SSH_AUTH_SOCK",
            "ssh_agent_pid",
            "GPG_AGENT_INFO",
            "GITHUB_TOKEN",
            "MY_API_KEY",
            "DB_PASSWORD",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            assert!(!passes_into_sandbox(key), "{key} would reach the sandbox");
        }
        for key in ["PATH", "LANG", "TERM", "CARGO_HOME"] {
            assert!(passes_into_sandbox(key), "{key} is needed in the sandbox");
        }
        let launcher = SandboxLauncher::new(OsSandboxConfig {
            read_toolchains: false,
            ..OsSandboxConfig::default()
        });
        let env =
            launcher.environment(&Workspace::new(".").allow_bash(true), Path::new("/scratch"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/scratch"));
        assert_eq!(env.get("TMPDIR").map(String::as_str), Some("/scratch"));
    }
}
