//! The stdio launcher of drivers (spec 3.2 §7.7, §10 T1): the OS sandbox through
//! `adk_sandbox::sandbox::sandboxed_command`, an environment cleared to an allowlist, and
//! `secret://` references resolved through the credential broker into files.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{ActingContext, AdkError, CredentialBroker};
use adk_sandbox::SandboxError;
use adk_sandbox::sandbox::{
    AccessMode, AllowedPath, NetworkSupport, SandboxEnforcer, SandboxPolicy, SandboxPolicyBuilder,
    WrappedCommand,
};
use adk_tool::mcp::McpServerConfig;

use super::manifest::{DriverManifest, DriverSandbox};
use super::store::{DriverIsolation, DriverProcessTree};
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
    /// What holds the child's process tree: the Job Object on Windows (spec 3.2 §10 W1), and
    /// the process group on Unix (§7.7, K-I5), that `McpServerManager` starts every stdio
    /// server in.
    pub(crate) process_tree: Option<DriverProcessTree>,
    /// The enforcer and policy of a sandboxed child.
    sandbox: Option<(Arc<dyn SandboxEnforcer>, SandboxPolicy)>,
    /// The file the command resolves to, run in its place under the sandbox, which opens
    /// where a link points and not the link.
    program: Option<PathBuf>,
}

impl std::fmt::Debug for LaunchPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchPlan")
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("cwd", &self.cwd)
            .field("isolation", &self.isolation)
            .field("process_tree", &self.process_tree)
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
                self.program.as_deref().map_or(OsStr::new(&config.command), Path::as_os_str),
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
        process_tree: DriverProcessTree::platform(),
        sandbox: None,
        program: None,
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
            let search = env.get("PATH").map(String::as_str);
            let program = resolve_program(&manifest.server.command, search);
            let program_paths = program
                .as_deref()
                .map(|program| program_paths(program, search))
                .unwrap_or_default();
            for path in &program_paths {
                grant(path, AccessMode::ReadOnly);
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
                isolation: DriverIsolation::OsSandbox {
                    enforcer: enforcer.name().to_string(),
                    program_paths,
                },
                process_tree: DriverProcessTree::platform(),
                sandbox: Some((enforcer, policy)),
                program,
            })
        }
    }
}

/// System directories a server reads under bubblewrap, which starts from an empty root:
/// without them the dynamic loader, libc and the CA certificates are missing.
const SYSTEM_READ: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/etc"];

/// The Nix store, opened whole when a program's loader or libraries live in it: store paths
/// reference each other, and the store is read-only.
const NIX_STORE: &str = "/nix/store";

/// The directories `program` needs to run, read-only: its own, a script's interpreter's,
/// and an ELF program's dynamic loader's and run paths, followed through each interpreter.
/// Directories under [`SYSTEM_READ`] are left out, since every sandbox reads them; a Nix
/// store path opens [`NIX_STORE`].
fn program_paths(program: &Path, search: Option<&str>) -> Vec<PathBuf> {
    let mut opened = BTreeSet::new();
    let mut open = |dir: &Path| {
        let Ok(dir) = dir.canonicalize() else { return };
        if dir.starts_with(NIX_STORE) {
            opened.insert(PathBuf::from(NIX_STORE));
        } else if !SYSTEM_READ.iter().map(Path::new).any(|system| {
            dir.starts_with(system) || system.canonicalize().is_ok_and(|real| dir.starts_with(real))
        }) {
            opened.insert(dir);
        }
    };
    let mut pending = vec![program.to_path_buf()];
    let mut seen = BTreeSet::new();
    // An interpreter chain is short; the bound stops a loop of scripts naming each other.
    while let Some(file) = pending.pop() {
        if seen.len() >= 8 || !seen.insert(file.clone()) {
            continue;
        }
        if let Some(dir) = file.parent() {
            open(dir);
        }
        let (interpreters, run_paths) = needs(&file, search);
        for dir in &run_paths {
            open(dir);
        }
        pending.extend(interpreters.into_iter().filter_map(|path| path.canonicalize().ok()));
    }
    opened.into_iter().collect()
}

/// What executable `file` loads before it runs: the interpreters to follow (a script's
/// `#!` program and what `env` finds for it, or an ELF program's dynamic loader) and the
/// directories its libraries come from (an ELF program's `RUNPATH` or `RPATH`).
fn needs(file: &Path, search: Option<&str>) -> (Vec<PathBuf>, Vec<PathBuf>) {
    use std::io::Read as _;
    let Ok(mut handle) = std::fs::File::open(file) else { return (Vec::new(), Vec::new()) };
    let mut head = [0_u8; 512];
    let Ok(read) = handle.read(&mut head) else { return (Vec::new(), Vec::new()) };
    let head = &head[..read];
    if let Some(line) = head.strip_prefix(b"#!") {
        let line = line.split(|byte| *byte == b'\n').next().unwrap_or_default();
        let line = String::from_utf8_lossy(line);
        let mut words = line.split_whitespace();
        let Some(interpreter) = words.next() else { return (Vec::new(), Vec::new()) };
        let mut interpreters = vec![PathBuf::from(interpreter)];
        if Path::new(interpreter).file_name() == Some(OsStr::new("env"))
            && let Some(name) = words.find(|word| !word.starts_with('-') && !word.contains('='))
            && let Some(found) = resolve_program(name, search)
        {
            interpreters.push(found);
        }
        return (interpreters, Vec::new());
    }
    if head.starts_with(b"\x7fELF") {
        return elf::needs(&handle, head, file);
    }
    (Vec::new(), Vec::new())
}

/// The parts of an ELF file the dynamic loader reads first: `PT_INTERP`, and `DT_RUNPATH`
/// or `DT_RPATH` through `PT_DYNAMIC` and the string table a `PT_LOAD` segment maps.
#[cfg(unix)]
mod elf {
    use std::fs::File;
    use std::os::unix::fs::FileExt as _;
    use std::path::{Path, PathBuf};

    const PT_LOAD: u32 = 1;
    const PT_DYNAMIC: u32 = 2;
    const PT_INTERP: u32 = 3;
    const DT_STRTAB: u64 = 5;
    const DT_RPATH: u64 = 15;
    const DT_RUNPATH: u64 = 29;

    /// Reads integers of the file's class and byte order.
    struct Layout {
        wide: bool,
        little: bool,
    }

    impl Layout {
        fn int(&self, bytes: &[u8], at: usize, size: usize) -> Option<u64> {
            let bytes = bytes.get(at..at.checked_add(size)?)?;
            let mut word = [0_u8; 8];
            Some(if self.little {
                word.get_mut(..size)?.copy_from_slice(bytes);
                u64::from_le_bytes(word)
            } else {
                word.get_mut(8_usize.checked_sub(size)?..)?.copy_from_slice(bytes);
                u64::from_be_bytes(word)
            })
        }

        fn word(&self, bytes: &[u8], at: usize) -> Option<u64> {
            self.int(bytes, at, if self.wide { 8 } else { 4 })
        }
    }

    fn read(file: &File, offset: u64, len: u64, limit: u64) -> Option<Vec<u8>> {
        let mut bytes = vec![0; usize::try_from(len.min(limit)).ok()?];
        file.read_exact_at(&mut bytes, offset).ok()?;
        Some(bytes)
    }

    /// The loader and run paths of the ELF file `path`, whose first bytes are `head`; none
    /// where its headers cannot be read.
    pub(super) fn needs(file: &File, head: &[u8], path: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
        parse(file, head, path).unwrap_or_default()
    }

    fn parse(file: &File, head: &[u8], path: &Path) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
        let layout = Layout { wide: *head.get(4)? == 2, little: *head.get(5)? == 1 };
        let (phoff, phentsize, phnum) = if layout.wide {
            (layout.int(head, 0x20, 8)?, layout.int(head, 0x36, 2)?, layout.int(head, 0x38, 2)?)
        } else {
            (layout.int(head, 0x1c, 4)?, layout.int(head, 0x2a, 2)?, layout.int(head, 0x2c, 2)?)
        };
        let headers = read(file, phoff, phentsize * phnum, 64 * 1024)?;
        // (type, file offset, virtual address, file size) of each program header.
        let segments: Vec<(u32, u64, u64, u64)> = headers
            .chunks(usize::try_from(phentsize).ok()?.max(1))
            .filter_map(|entry| {
                let kind = u32::try_from(layout.int(entry, 0, 4)?).ok()?;
                Some(if layout.wide {
                    (
                        kind,
                        layout.int(entry, 8, 8)?,
                        layout.int(entry, 16, 8)?,
                        layout.int(entry, 32, 8)?,
                    )
                } else {
                    (
                        kind,
                        layout.int(entry, 4, 4)?,
                        layout.int(entry, 8, 4)?,
                        layout.int(entry, 16, 4)?,
                    )
                })
            })
            .collect();
        let mut interpreters = Vec::new();
        let mut run_paths = Vec::new();
        for &(kind, offset, _, size) in &segments {
            if kind == PT_INTERP
                && let Some(bytes) = read(file, offset, size, 4096)
            {
                let text = String::from_utf8_lossy(&bytes);
                interpreters.push(PathBuf::from(text.trim_end_matches('\0')));
            }
        }
        let Some(&(_, offset, _, size)) = segments.iter().find(|segment| segment.0 == PT_DYNAMIC)
        else {
            return Some((interpreters, run_paths));
        };
        let dynamic = read(file, offset, size, 1024 * 1024)?;
        let entry = if layout.wide { 16 } else { 8 };
        let (mut strtab, mut names) = (None, Vec::new());
        for at in (0..dynamic.len()).step_by(entry) {
            let (Some(tag), Some(value)) =
                (layout.word(&dynamic, at), layout.word(&dynamic, at + entry / 2))
            else {
                break;
            };
            match tag {
                0 => break,
                DT_STRTAB => strtab = Some(value),
                DT_RPATH | DT_RUNPATH => names.push(value),
                _ => {}
            }
        }
        let strtab = strtab.and_then(|address| {
            segments.iter().find_map(|&(kind, offset, vaddr, size)| {
                let within = address.checked_sub(vaddr).filter(|delta| *delta < size)?;
                if kind == PT_LOAD { offset.checked_add(within) } else { None }
            })
        });
        let origin = path.parent().unwrap_or(Path::new("/")).display().to_string();
        for name in names {
            let Some(bytes) = strtab.and_then(|table| {
                let mut bytes = vec![0; 4096];
                let read = file.read_at(&mut bytes, table.checked_add(name)?).ok()?;
                bytes.truncate(read);
                Some(bytes)
            }) else {
                continue;
            };
            let text =
                String::from_utf8_lossy(bytes.split(|byte| *byte == 0).next().unwrap_or_default());
            for entry in text.split(':') {
                let entry = entry.replace("${ORIGIN}", &origin).replace("$ORIGIN", &origin);
                if entry.starts_with('/') {
                    run_paths.push(PathBuf::from(entry));
                }
            }
        }
        Some((interpreters, run_paths))
    }
}

/// No ELF loader exists off Unix.
#[cfg(not(unix))]
mod elf {
    use std::fs::File;
    use std::path::{Path, PathBuf};

    pub(super) fn needs(_file: &File, _head: &[u8], _path: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
        (Vec::new(), Vec::new())
    }
}

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

    fn network_support(&self) -> NetworkSupport {
        self.inner.network_support()
    }

    fn proxy_listener(&self) -> adk_sandbox::sandbox::ProxyListener {
        self.inner.proxy_listener()
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

    /// On Windows every driver runs in a Job Object, sandboxed or not, and the plan records
    /// it; elsewhere nothing holds the tree (spec 3.2 §10 W1).
    #[tokio::test]
    async fn the_plan_records_what_holds_the_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let expected = if cfg!(windows) {
            Some(DriverProcessTree::JobObject)
        } else {
            Some(DriverProcessTree::ProcessGroup)
        };
        for sandbox in ["unsandboxed", "os", "container"] {
            let plan = prepare(
                &manifest(sandbox),
                dir.path(),
                Some(&Vault),
                &acting(),
                false,
                &unavailable(),
            )
            .await
            .unwrap();
            assert_eq!(plan.process_tree, expected, "sandbox = {sandbox}");
        }
        let empty_root: EnforcerSource =
            Arc::new(|| Ok(Arc::new(EmptyRoot) as Arc<dyn SandboxEnforcer>));
        let plan = prepare(&manifest("os"), dir.path(), Some(&Vault), &acting(), true, &empty_root)
            .await
            .unwrap();
        assert!(plan.isolation.is_sandboxed());
        assert_eq!(plan.process_tree, expected, "a sandboxed driver");
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
        fn network_support(&self) -> NetworkSupport {
            NetworkSupport { denied: true, proxied: false }
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
        assert_eq!(
            plan.isolation,
            DriverIsolation::OsSandbox { enforcer: "bubblewrap".into(), program_paths: Vec::new() }
        );
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

    #[cfg(unix)]
    fn executable(path: &Path, contents: &[u8]) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, contents).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn canonical(paths: &[&Path]) -> Vec<PathBuf> {
        let set: BTreeSet<PathBuf> =
            paths.iter().map(|path| path.canonicalize().unwrap()).collect();
        set.into_iter().collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_script_opens_its_interpreter_and_what_env_finds_for_it() {
        let (scripts, toolchain) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let tool = toolchain.path().join("mytool");
        executable(&tool, b"");
        let direct = scripts.path().join("direct");
        executable(&direct, format!("#!{} -u\nprint()\n", tool.display()).as_bytes());
        let through_env = scripts.path().join("through-env");
        executable(&through_env, b"#!/usr/bin/env -S mytool --flag\n");
        let system = scripts.path().join("system");
        executable(&system, b"#!/bin/sh\nexit 0\n");
        let search = toolchain.path().display().to_string();

        let both = canonical(&[scripts.path(), toolchain.path()]);
        assert_eq!(program_paths(&direct, None), both);
        assert_eq!(program_paths(&through_env, Some(&search)), both);
        assert_eq!(
            program_paths(&system, None),
            canonical(&[scripts.path()]),
            "/bin is a system directory"
        );
    }

    #[cfg(unix)]
    /// A 64-bit little-endian ELF file with `PT_INTERP` `interpreter` and `DT_RUNPATH`
    /// `run_path`, mapped by one `PT_LOAD` segment at 0x1000. Each string gets the room
    /// its length needs, since a temporary directory's path on macOS is longer than 64 bytes.
    fn elf(interpreter: &str, run_path: &str) -> Vec<u8> {
        // The headers, the dynamic section at 256, the interpreter at 320, then the string
        // table, whose string 1 is the run path.
        let (dynamic, interp) = (256, 320);
        let strtab = interp + interpreter.len() + 1;
        let mut bytes = vec![0_u8; strtab + run_path.len() + 2];
        let put = |bytes: &mut Vec<u8>, at: usize, value: u64, size: usize| {
            bytes[at..at + size].copy_from_slice(&value.to_le_bytes()[..size]);
        };
        bytes[..4].copy_from_slice(b"\x7fELF");
        (bytes[4], bytes[5], bytes[6]) = (2, 1, 1);
        put(&mut bytes, 0x20, 64, 8);
        put(&mut bytes, 0x36, 56, 2);
        put(&mut bytes, 0x38, 3, 2);
        // (type, offset, vaddr, size): PT_LOAD of the file, PT_INTERP, PT_DYNAMIC.
        let segments = [
            (1, 0, 0x1000, bytes.len() as u64),
            (3, interp as u64, 0x1000 + interp as u64, interpreter.len() as u64 + 1),
            (2, dynamic as u64, 0x1000 + dynamic as u64, 48),
        ];
        for (index, (kind, offset, vaddr, size)) in segments.into_iter().enumerate() {
            let at = 64 + index * 56;
            put(&mut bytes, at, kind, 4);
            put(&mut bytes, at + 8, offset, 8);
            put(&mut bytes, at + 16, vaddr, 8);
            put(&mut bytes, at + 32, size, 8);
        }
        bytes[interp..interp + interpreter.len()].copy_from_slice(interpreter.as_bytes());
        bytes[strtab + 1..strtab + 1 + run_path.len()].copy_from_slice(run_path.as_bytes());
        // DT_STRTAB, DT_RUNPATH at string 1, DT_NULL.
        for (index, (tag, value)) in
            [(5, 0x1000 + strtab as u64), (29, 1), (0, 0)].into_iter().enumerate()
        {
            put(&mut bytes, dynamic + index * 16, tag, 8);
            put(&mut bytes, dynamic + index * 16 + 8, value, 8);
        }
        bytes
    }

    #[cfg(unix)]
    #[test]
    fn an_elf_program_opens_its_loader_and_run_paths() {
        let root = tempfile::tempdir().unwrap();
        let (bin, lib, loader) =
            (root.path().join("bin"), root.path().join("lib"), root.path().join("loader"));
        for dir in [&bin, &lib, &loader] {
            std::fs::create_dir(dir).unwrap();
        }
        let ld = loader.join("ld.so");
        executable(&ld, b"");
        let server = bin.join("server");
        executable(&server, &elf(&ld.display().to_string(), "$ORIGIN/../lib:/opt/missing/lib"));

        let (interpreters, run_paths) = needs(&server, None);
        assert_eq!(interpreters, std::slice::from_ref(&ld));
        assert_eq!(
            run_paths,
            [PathBuf::from(format!("{}/../lib", bin.display())), PathBuf::from("/opt/missing/lib")]
        );
        assert_eq!(
            program_paths(&server, None),
            canonical(&[&bin, &lib, &loader]),
            "the program's, its run path's and its loader's directories; a missing one is left out"
        );
        let mut truncated = elf("/x", "/y");
        truncated.truncate(100);
        executable(&server, &truncated);
        assert_eq!(needs(&server, None), (Vec::new(), Vec::new()), "a broken header opens nothing");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_sandbox_runs_the_file_a_link_points_to() {
        let dir = tempfile::tempdir().unwrap();
        let (links, real) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let target = real.path().join("mcp-notes");
        std::fs::write(&target, b"").unwrap();
        let link = links.path().join("mcp-notes");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let manifest = DriverManifest::from_toml(&format!(
            "id = \"notes\"\ncommand = \"{}\"\nsandbox = \"os\"\n",
            link.display()
        ))
        .unwrap();
        let empty_root: EnforcerSource =
            Arc::new(|| Ok(Arc::new(EmptyRoot) as Arc<dyn SandboxEnforcer>));
        let plan = prepare(&manifest, dir.path(), Some(&Vault), &acting(), true, &empty_root)
            .await
            .unwrap();
        let resolved = link.canonicalize().unwrap();
        let DriverIsolation::OsSandbox { program_paths, .. } = &plan.isolation else {
            panic!("sandboxed: {:?}", plan.isolation)
        };
        assert_eq!(program_paths, &[resolved.parent().unwrap().to_path_buf()]);
        let args: Vec<String> = plan
            .command(&manifest.server)
            .unwrap()
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let separator = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[separator + 1], resolved.display().to_string(), "{args:?}");
    }
}
