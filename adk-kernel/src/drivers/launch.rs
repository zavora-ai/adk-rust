//! The stdio launcher of drivers (spec 3.2 §7.7, §10 T1): the OS sandbox through
//! `adk_sandbox::sandbox::sandboxed_command`, an environment cleared to an allowlist, and
//! `secret://` references resolved through the credential broker into files.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{ActingContext, AdkError, CredentialBroker};
use adk_sandbox::SandboxError;
use adk_sandbox::sandbox::{
    AccessMode, AllowedPath, SandboxEnforcer, SandboxPolicy, SandboxPolicyBuilder, WrappedCommand,
};
use adk_tool::mcp::McpServerConfig;

use super::manifest::{DriverManifest, DriverSandbox};
use super::store::DriverIsolation;
use crate::error::{KernelError, Result};

/// Picks the OS enforcer: the platform's, or a stand-in a test sets.
pub(crate) type EnforcerSource =
    Arc<dyn Fn() -> std::result::Result<Arc<dyn SandboxEnforcer>, String> + Send + Sync>;

/// The platform's enforcer, probed.
pub(crate) fn platform_enforcer() -> EnforcerSource {
    Arc::new(|| {
        adk_sandbox::sandbox::get_enforcer().map(Arc::from).map_err(|error| error.to_string())
    })
}

/// Variables of the kernel's environment a driver inherits; nothing else reaches it, so a
/// key in the kernel's environment never reaches a server unless its manifest names it.
const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TZ",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "HOME",
    "TMPDIR",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
];

/// How a driver's process starts, prepared before each start the supervisor makes.
#[derive(Clone)]
pub(crate) struct LaunchPlan {
    /// The child's whole environment.
    pub(crate) env: BTreeMap<String, String>,
    /// The child's working directory.
    cwd: Option<PathBuf>,
    /// How the child is isolated.
    pub(crate) isolation: DriverIsolation,
    /// The enforcer and policy of a sandboxed child.
    sandbox: Option<(Arc<dyn SandboxEnforcer>, SandboxPolicy)>,
}

impl std::fmt::Debug for LaunchPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchPlan")
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("cwd", &self.cwd)
            .field("isolation", &self.isolation)
            .finish_non_exhaustive()
    }
}

impl LaunchPlan {
    /// The command that starts `config`'s server under this plan.
    pub(crate) fn command(
        &self,
        config: &McpServerConfig,
    ) -> adk_core::Result<tokio::process::Command> {
        let args: Vec<OsString> = config.args.iter().map(OsString::from).collect();
        let mut command = match &self.sandbox {
            Some((enforcer, policy)) => adk_sandbox::sandbox::sandboxed_command(
                enforcer.as_ref(),
                OsStr::new(&config.command),
                &args,
                policy,
                self.cwd.as_deref().unwrap_or(Path::new(".")),
            )
            .map_err(|error| {
                AdkError::tool(format!(
                    "the driver's OS sandbox refused its policy: {error}; check the manifest's \
                     allow_read and allow_write paths exist"
                ))
            })?,
            None => {
                let mut command = tokio::process::Command::new(&config.command);
                command.args(&args).env_clear().envs(&self.env);
                if let Some(cwd) = &self.cwd {
                    command.current_dir(cwd);
                }
                command
            }
        };
        command.kill_on_drop(true);
        Ok(command)
    }
}

/// Creates `dir` readable only by the user.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes `secret` to `path`, readable only by the user, replacing an earlier file.
fn write_secret(path: &Path, secret: &str) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file =
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        file.write_all(secret.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, secret)
    }
}

/// `program` resolved as `execvp` would through `path`, canonical.
fn resolve_program(program: &str, path: Option<&str>) -> Option<PathBuf> {
    let as_path = Path::new(program);
    let found = if as_path.components().count() > 1 {
        Some(as_path.to_path_buf())
    } else {
        std::env::split_paths(path?).map(|dir| dir.join(program)).find(|c| c.is_file())
    };
    found.and_then(|path| path.canonicalize().ok())
}

/// Prepares the start of `manifest`'s server (spec 3.2 §7.7, K-I13).
///
/// Secrets are minted for `acting` from `broker`, one file each under `run_dir/secrets`,
/// and named to the child by `<NAME>_FILE`; the child's environment is the allowlisted
/// part of the kernel's, the manifest's plain values, and those file variables. A
/// sandboxed child gets `run_dir/home` as its home and working directory.
///
/// # Errors
///
/// Returns [`KernelError::Config`] when a secret cannot be resolved, or when the sandbox
/// the manifest asks for is unavailable and `fail_if_unavailable` is set; and
/// [`KernelError::Io`] when the run directory cannot be written.
pub(crate) async fn prepare(
    manifest: &DriverManifest,
    run_dir: &Path,
    broker: Option<&dyn CredentialBroker>,
    acting: &ActingContext,
    fail_if_unavailable: bool,
    enforcer: &EnforcerSource,
) -> Result<LaunchPlan> {
    let refuse = |reason: String| KernelError::Config {
        reason: format!("driver `{}` cannot start: {reason}", manifest.id),
    };
    let secrets_dir = run_dir.join("secrets");
    let home = run_dir.join("home");
    private_dir(&secrets_dir)?;
    private_dir(&home)?;

    let mut env: BTreeMap<String, String> = ENV_ALLOWLIST
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| ((*key).to_string(), value)))
        .collect();
    let references = manifest.secret_references();
    for (name, value) in &manifest.server.env {
        if !references.contains_key(name) {
            env.insert(name.clone(), value.clone());
        }
    }
    for (name, reference) in &references {
        let broker = broker.ok_or_else(|| {
            refuse(format!(
                "env `{name}` names {reference}, and the kernel has no credential broker; \
                 set one with DriverSupervisorBuilder::credential_broker"
            ))
        })?;
        let token = broker.token_for(acting, reference, &[]).await.map_err(|error| {
            refuse(format!("the broker has no secret for {reference}: {error}"))
        })?;
        let secret = token
            .authorize_for(reference)
            .map_err(|error| refuse(format!("the broker's secret for {reference}: {error}")))?;
        let file = secrets_dir.join(name);
        write_secret(&file, secret.expose())?;
        env.remove(name);
        env.insert(format!("{name}_FILE"), file.display().to_string());
    }

    let unsandboxed = |reason: String, env: BTreeMap<String, String>| LaunchPlan {
        env,
        cwd: None,
        isolation: DriverIsolation::Unsandboxed { reason },
        sandbox: None,
    };
    let unavailable = |reason: String, env: BTreeMap<String, String>| {
        if fail_if_unavailable {
            Err(refuse(format!(
                "its `{}` sandbox is unavailable ({reason}) and the kernel refuses unsandboxed \
                 drivers (fail_if_sandbox_unavailable)",
                manifest.sandbox.as_str()
            )))
        } else {
            tracing::warn!(driver.id = %manifest.id, reason = %reason, "driver runs unsandboxed");
            Ok(unsandboxed(reason, env))
        }
    };
    match manifest.sandbox {
        DriverSandbox::Unsandboxed => {
            Ok(unsandboxed("the manifest asks for no sandbox".to_string(), env))
        }
        DriverSandbox::Container => unavailable(
            "drivers have no container launcher; run the server's own container command as the \
             driver's command, or use `sandbox = \"os\"`"
                .to_string(),
            env,
        ),
        DriverSandbox::Os => {
            let enforcer = match enforcer() {
                Ok(enforcer) => enforcer,
                Err(reason) => return unavailable(reason, env),
            };
            let home_dir = home.canonicalize()?;
            let secrets = secrets_dir.canonicalize()?;
            env.insert("HOME".to_string(), home_dir.display().to_string());
            env.insert("TMPDIR".to_string(), home_dir.display().to_string());
            let mut builder = SandboxPolicyBuilder::new().allow_process_spawn();
            if manifest.access.allow_network {
                builder = builder.allow_network();
            }
            for (key, value) in &env {
                builder = builder.env(key.clone(), value.clone());
            }
            let mut policy = builder.build();
            let mut grant = |path: &Path, mode: AccessMode| {
                if path.exists() {
                    policy.allowed_paths.push(AllowedPath { path: path.to_path_buf(), mode });
                }
            };
            if let Some(program) =
                resolve_program(&manifest.server.command, env.get("PATH").map(String::as_str))
                && let Some(dir) = program.parent()
            {
                grant(dir, AccessMode::ReadOnly);
            }
            grant(&secrets, AccessMode::ReadOnly);
            grant(&home_dir, AccessMode::ReadWrite);
            for path in &manifest.access.allow_read {
                grant(path, AccessMode::ReadOnly);
            }
            for path in &manifest.access.allow_write {
                grant(path, AccessMode::ReadWrite);
            }
            let enforcer: Arc<dyn SandboxEnforcer> = if enforcer.name() == "bubblewrap" {
                let mut links = Vec::new();
                for dir in SYSTEM_READ.iter().map(Path::new) {
                    let Ok(metadata) = std::fs::symlink_metadata(dir) else { continue };
                    if !metadata.file_type().is_symlink() {
                        grant(dir, AccessMode::ReadOnly);
                    } else if let (Ok(text), Ok(target)) =
                        (std::fs::read_link(dir), dir.canonicalize())
                    {
                        grant(&target, AccessMode::ReadOnly);
                        links.push((text.into_os_string(), dir.to_path_buf()));
                    }
                }
                Arc::new(SystemRoot { inner: enforcer, links })
            } else {
                enforcer
            };
            Ok(LaunchPlan {
                env,
                cwd: Some(home_dir),
                isolation: DriverIsolation::OsSandbox { enforcer: enforcer.name().to_string() },
                sandbox: Some((enforcer, policy)),
            })
        }
    }
}

/// System directories a server reads under bubblewrap, which starts from an empty root:
/// without them the dynamic loader, libc and the CA certificates are missing.
const SYSTEM_READ: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/etc"];

/// Bubblewrap with `/dev`, `/proc` and the symlinked system directories recreated as links.
///
/// On a merged-`/usr` system `/bin`, `/lib` and `/lib64` are symlinks into `/usr`; binding one
/// mounts its target, so the link path — the dynamic loader's path among them — would be
/// missing inside.
struct SystemRoot {
    inner: Arc<dyn SandboxEnforcer>,
    links: Vec<(OsString, PathBuf)>,
}

impl SandboxEnforcer for SystemRoot {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn probe(&self) -> std::result::Result<(), SandboxError> {
        self.inner.probe()
    }

    fn wrap_command(
        &self,
        program: &OsStr,
        args: &[OsString],
        policy: &SandboxPolicy,
    ) -> std::result::Result<WrappedCommand, SandboxError> {
        let mut wrapped = self.inner.wrap_command(program, args, policy)?;
        let separator =
            wrapped.args.iter().position(|arg| arg == "--").unwrap_or(wrapped.args.len());
        let mut extra: Vec<OsString> =
            ["--dev", "/dev", "--proc", "/proc"].into_iter().map(OsString::from).collect();
        for (text, link) in &self.links {
            extra.extend([
                OsString::from("--symlink"),
                text.clone(),
                link.clone().into_os_string(),
            ]);
        }
        wrapped.args.splice(separator..separator, extra);
        Ok(wrapped)
    }

    fn configure_command(
        &self,
        cmd: &mut tokio::process::Command,
        policy: &SandboxPolicy,
    ) -> std::result::Result<(), SandboxError> {
        self.inner.configure_command(cmd, policy)
    }
}

/// Removes the run directory of a driver: its secrets and scratch home.
pub(crate) fn clear_run_dir(run_dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(run_dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %run_dir.display(), error = %error, "could not remove a driver's run directory");
    }
}

#[cfg(test)]
mod tests {
    use adk_core::{RevocationScope, ScopedToken, SubjectKind, async_trait};

    use super::*;

    #[derive(Debug)]
    struct Vault;

    #[async_trait]
    impl CredentialBroker for Vault {
        async fn token_for(
            &self,
            acting: &ActingContext,
            resource: &url::Url,
            scopes: &[String],
        ) -> adk_core::Result<ScopedToken> {
            Ok(ScopedToken::new(
                acting.agent.id.clone(),
                format!("s3cr3t-for-{}", resource.path()),
                resource.clone(),
                scopes.to_vec(),
            ))
        }

        async fn revoke(&self, _scope: &RevocationScope) -> adk_core::Result<u64> {
            Ok(0)
        }
    }

    fn acting() -> ActingContext {
        ActingContext::derived("app", "driver.email", "kernel", SubjectKind::Service)
    }

    fn unavailable() -> EnforcerSource {
        Arc::new(|| Err("no enforcer in this test".to_string()))
    }

    fn manifest(sandbox: &str) -> DriverManifest {
        DriverManifest::from_toml(&format!(
            "id = \"email\"\ncommand = \"mcp-email\"\nsandbox = \"{sandbox}\"\n\
             env = {{ EMAIL_TOKEN = \"secret://email/token\", LOG_LEVEL = \"info\" }}\n"
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn a_secret_reaches_the_child_as_a_file_and_never_as_a_value() {
        let dir = tempfile::tempdir().unwrap();
        let plan = prepare(
            &manifest("unsandboxed"),
            dir.path(),
            Some(&Vault),
            &acting(),
            false,
            &unavailable(),
        )
        .await
        .unwrap();
        let command = plan.command(&manifest("unsandboxed").server).unwrap();
        let envs: BTreeMap<String, Option<String>> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(!envs.contains_key("EMAIL_TOKEN"), "{envs:?}");
        assert!(
            envs.values().flatten().all(|value| !value.contains("s3cr3t")),
            "a secret reached the environment: {envs:?}"
        );
        // Cargo sets it for the test process; the kernel's environment stays with the kernel.
        assert!(!envs.contains_key("CARGO_MANIFEST_DIR"), "ambient variables leak");
        assert_eq!(envs["LOG_LEVEL"].as_deref(), Some("info"));
        let file = PathBuf::from(envs["EMAIL_TOKEN_FILE"].clone().unwrap());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "s3cr3t-for-/token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(
            plan.isolation,
            DriverIsolation::Unsandboxed { reason: "the manifest asks for no sandbox".into() }
        );
        clear_run_dir(dir.path());
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn a_secret_without_a_broker_refuses_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let error = prepare(&manifest("os"), dir.path(), None, &acting(), false, &unavailable())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no credential broker"), "{error}");
    }

    #[tokio::test]
    async fn an_unavailable_sandbox_is_recorded_or_refused() {
        let dir = tempfile::tempdir().unwrap();
        let plan =
            prepare(&manifest("os"), dir.path(), Some(&Vault), &acting(), false, &unavailable())
                .await
                .unwrap();
        assert_eq!(
            plan.isolation,
            DriverIsolation::Unsandboxed { reason: "no enforcer in this test".into() }
        );
        let refused =
            prepare(&manifest("os"), dir.path(), Some(&Vault), &acting(), true, &unavailable())
                .await
                .unwrap_err();
        assert!(refused.to_string().contains("fail_if_sandbox_unavailable"), "{refused}");
        let container = prepare(
            &manifest("container"),
            dir.path(),
            Some(&Vault),
            &acting(),
            false,
            &unavailable(),
        )
        .await
        .unwrap();
        assert!(matches!(
            container.isolation,
            DriverIsolation::Unsandboxed { reason } if reason.contains("container")
        ));
    }

    /// A bubblewrap stand-in that binds nothing of the system itself, as bubblewrap does not.
    struct EmptyRoot;

    impl SandboxEnforcer for EmptyRoot {
        fn name(&self) -> &str {
            "bubblewrap"
        }
        fn probe(&self) -> std::result::Result<(), SandboxError> {
            Ok(())
        }
        fn wrap_command(
            &self,
            program: &OsStr,
            args: &[OsString],
            _policy: &SandboxPolicy,
        ) -> std::result::Result<WrappedCommand, SandboxError> {
            let mut wrapped = vec![OsString::from("--unshare-pid"), OsString::from("--")];
            wrapped.push(program.to_owned());
            wrapped.extend_from_slice(args);
            Ok(WrappedCommand { program: OsString::from("bwrap"), args: wrapped })
        }
        fn configure_command(
            &self,
            _cmd: &mut tokio::process::Command,
            _policy: &SandboxPolicy,
        ) -> std::result::Result<(), SandboxError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn bubblewrap_gets_the_system_directories_devices_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let empty_root: EnforcerSource =
            Arc::new(|| Ok(Arc::new(EmptyRoot) as Arc<dyn SandboxEnforcer>));
        let plan = prepare(&manifest("os"), dir.path(), Some(&Vault), &acting(), true, &empty_root)
            .await
            .unwrap();
        assert_eq!(plan.isolation, DriverIsolation::OsSandbox { enforcer: "bubblewrap".into() });
        let (_, policy) = plan.sandbox.as_ref().unwrap();
        let granted = |path: &Path| {
            policy
                .allowed_paths
                .iter()
                .any(|allowed| allowed.path == path && allowed.mode == AccessMode::ReadOnly)
        };
        let args: Vec<String> = plan
            .command(&manifest("os").server)
            .unwrap()
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let separator = args.iter().position(|arg| arg == "--").unwrap();
        let before = args[..separator].join(" ");
        assert!(before.contains("--dev /dev --proc /proc"), "{args:?}");
        for system in SYSTEM_READ.iter().map(Path::new) {
            let Ok(metadata) = std::fs::symlink_metadata(system) else { continue };
            if metadata.file_type().is_symlink() {
                let text = std::fs::read_link(system).unwrap();
                assert!(granted(&system.canonicalize().unwrap()), "{system:?} target");
                assert!(
                    before.contains(&format!("--symlink {} {}", text.display(), system.display())),
                    "{system:?} is recreated as a link: {args:?}"
                );
            } else {
                assert!(granted(system), "{system:?} is readable");
            }
        }
    }
}
