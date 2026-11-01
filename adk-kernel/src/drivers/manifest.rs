//! Driver manifests and effect classes (spec 3.2 §7.7).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use adk_tool::mcp::McpTaskConfig;
use adk_tool::mcp::manager::{McpLifecycleMode, McpServerConfig, RestartPolicy};
use serde::{Deserialize, Serialize};

use super::EffectClass;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;

/// The scheme of an environment value the credential broker resolves (spec 3.2 §7.7).
pub const SECRET_SCHEME: &str = "secret";

/// How a driver's process is isolated (spec 3.2 §7.7).
///
/// `os` runs the server under the platform's enforcer (Seatbelt, bubblewrap) through
/// `adk_sandbox::sandbox::sandboxed_command`; where no enforcer exists the kernel records
/// the driver as unsandboxed with the reason, or refuses to start it under
/// [`KernelConfig::fail_if_sandbox_unavailable`](crate::KernelConfig::fail_if_sandbox_unavailable).
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::DriverSandbox;
///
/// assert_eq!(DriverSandbox::default(), DriverSandbox::Os);
/// assert_eq!(serde_json::to_value(DriverSandbox::Unsandboxed).unwrap(), "unsandboxed");
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DriverSandbox {
    /// The platform's OS sandbox.
    #[default]
    Os,
    /// A container. Drivers have no container launcher yet, so this behaves as an
    /// unavailable enforcer.
    Container,
    /// No sandbox, by the manifest's choice; recorded as such.
    Unsandboxed,
}

impl DriverSandbox {
    /// The name a manifest writes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Os => "os",
            Self::Container => "container",
            Self::Unsandboxed => "unsandboxed",
        }
    }
}

/// When a driver's process starts (spec 3.2 §7.7, the manifest's `lifecycle`).
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::DriverLifecycle;
///
/// assert_eq!(DriverLifecycle::default(), DriverLifecycle::Lazy);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DriverLifecycle {
    /// When a partition's agent first lists the driver's tools.
    #[default]
    Lazy,
    /// When the kernel starts, and when the driver is mounted.
    Eager,
}

/// What a sandboxed driver may reach besides the system's read-only paths (spec 3.2 §7.7).
///
/// The server's program directory, a scratch home and the directory of its brokered
/// secrets are granted by the kernel; these add to them. Paths that do not exist are left
/// out, as the enforcers refuse a policy naming a missing path.
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::DriverAccess;
///
/// let access = DriverAccess::default();
/// assert!(access.allow_network);
/// assert!(access.allow_write.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DriverAccess {
    /// Paths the server may read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_read: Vec<PathBuf>,
    /// Paths the server may read and write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_write: Vec<PathBuf>,
    /// Whether the server may use the network; integrations usually must.
    #[serde(default = "default_true")]
    pub allow_network: bool,
    /// With `allow_network`, the only hosts the server may reach (`[network] hosts` in the
    /// manifest), through an egress proxy of its own: `Proxied` network access under the OS
    /// sandbox. Empty leaves the network open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_hosts: Vec<adk_core::egress::EgressRule>,
}

impl Default for DriverAccess {
    fn default() -> Self {
        Self {
            allow_read: Vec::new(),
            allow_write: Vec::new(),
            allow_network: true,
            network_hosts: Vec::new(),
        }
    }
}

impl DriverAccess {
    /// The egress policy the server's connections pass, when `network_hosts` names any.
    pub fn egress_policy(&self) -> Option<adk_core::egress::EgressPolicy> {
        (self.allow_network && !self.network_hosts.is_empty()).then(|| {
            self.network_hosts.iter().cloned().fold(
                adk_core::egress::EgressPolicy::deny_all(),
                adk_core::egress::EgressPolicy::allow,
            )
        })
    }
}

fn default_true() -> bool {
    true
}

/// An MCP server run as a supervised child process, as the user approved it (spec 3.2
/// §7.7).
///
/// Written as TOML in the user's drivers directory, or in a workspace's `.adk/drivers/`,
/// where it may only restrict ([`restrict_with`](Self::restrict_with)):
///
/// ```toml
/// id          = "email"
/// command     = "mcp-email"
/// args        = ["--imap"]
/// env         = { EMAIL_TOKEN = "secret://email/token" }
/// partitions  = ["work"]
/// lifecycle   = "lazy"          # lazy | eager
/// # with allow_network, the only hosts the server reaches, through an egress proxy
/// # network     = { hosts = ["imap.example.com:993", "smtp.example.com:465"] }
/// sandbox     = "os"            # os | container | unsandboxed
/// restart     = { max_attempts = 5, initial_delay_ms = 500, max_delay_ms = 30000, backoff_multiplier = 2.0 }
///
/// [tools]                       # unknown tools are `other`
/// list_inbox   = "read"
/// send_draft   = "send_external"
/// ```
///
/// Further keys: `protocol` (`initialize`, `auto` or `discover`, the MCP handshake),
/// `allow_read`, `allow_write`, `allow_network` (the [`DriverAccess`] of a sandboxed
/// server), `sampling` (whether the server may sample the harness model) and
/// `data_classes`. An `env` value `secret://...` is resolved through the credential broker
/// when the driver starts and reaches the process as a file named by `<NAME>_FILE`, never
/// as a variable's value; a plain value under a name that reads as a credential
/// (`*_TOKEN`, `*_SECRET`, `*_PASSWORD`, `*_API_KEY`, ...) is refused.
///
/// Every driver declares MCP tasks ([`McpTaskConfig::enable_tasks`]), so a server that
/// needs a long wait can hand back a durable task handle (§13 Q9). Without a `restart`
/// table a driver restarts under [`RestartPolicy::default`].
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::{DriverManifest, EffectClass};
///
/// let manifest = DriverManifest::from_toml(r#"
///     id = "email"
///     command = "mcp-email"
///     partitions = ["work"]
///     [tools]
///     list_inbox = "read"
///     send_draft = "send_external"
/// "#).unwrap();
/// assert_eq!(manifest.class_of("send_draft"), EffectClass::SendExternal);
/// assert_eq!(manifest.class_of("delete_everything"), EffectClass::Other);
/// assert!(manifest.server.task_config.enable_tasks);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DriverManifest {
    /// The driver's id, also the MCP server id: ASCII letters, digits, `-` and `_`.
    pub id: String,
    /// The server process: command, arguments, environment, restart policy, handshake and
    /// task policy.
    pub server: McpServerConfig,
    /// The partitions whose agents may use the driver.
    pub partitions: Vec<PartitionId>,
    /// How the process is isolated.
    pub sandbox: DriverSandbox,
    /// The effect class of each tool; a tool not listed is [`EffectClass::Other`].
    #[serde(default)]
    pub tools: BTreeMap<String, EffectClass>,
    /// Labels of the data the driver handles, for the person approving it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_classes: Vec<String>,
    /// Each tool's declaration digest (`ToolOrigin::declaration_digest`), pinned when the
    /// driver first lists its tools after approval, and again at re-approval.
    #[serde(default)]
    pub declaration_digests: BTreeMap<String, String>,
    /// When the process starts.
    #[serde(default)]
    pub lifecycle: DriverLifecycle,
    /// What a sandboxed process may reach.
    #[serde(default)]
    pub access: DriverAccess,
    /// Whether the server may sample the harness model.
    #[serde(default)]
    pub sampling: bool,
}

/// The TOML form of a manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    id: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    partitions: Option<Vec<String>>,
    #[serde(default)]
    lifecycle: DriverLifecycle,
    #[serde(default)]
    protocol: McpLifecycleMode,
    #[serde(default)]
    sandbox: DriverSandbox,
    #[serde(default)]
    allow_read: Vec<PathBuf>,
    #[serde(default)]
    allow_write: Vec<PathBuf>,
    #[serde(default = "default_true")]
    allow_network: bool,
    #[serde(default)]
    network: Option<NetworkFile>,
    #[serde(default)]
    restart: Option<RestartFile>,
    #[serde(default)]
    sampling: bool,
    #[serde(default)]
    data_classes: Vec<String>,
    #[serde(default)]
    tools: BTreeMap<String, String>,
}

/// The manifest's `network` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkFile {
    #[serde(default)]
    hosts: Vec<adk_core::egress::EgressRule>,
}

/// The manifest's `restart` table, in the manifest's own key names.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestartFile {
    #[serde(default)]
    max_attempts: Option<u32>,
    #[serde(default)]
    initial_delay_ms: Option<u64>,
    #[serde(default)]
    max_delay_ms: Option<u64>,
    #[serde(default)]
    backoff_multiplier: Option<f64>,
}

/// Name suffixes that mark an environment variable as a credential.
const CREDENTIAL_NAMES: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "PRIVATE_KEY",
    "CREDENTIAL",
    "CREDENTIALS",
];

/// Whether `name` reads as the name of a credential.
fn names_a_credential(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    CREDENTIAL_NAMES.iter().any(|suffix| {
        upper == *suffix || upper.strip_suffix(suffix).is_some_and(|rest| rest.ends_with('_'))
    })
}

fn invalid(id: &str, reason: impl fmt::Display) -> KernelError {
    KernelError::Config { reason: format!("driver manifest `{id}`: {reason}") }
}

impl DriverManifest {
    /// A manifest for server `server` under `id`, in the default partition, sandboxed,
    /// with every tool `other` until [`with_tool`](Self::with_tool) classes it.
    ///
    /// Tasks are enabled on the server's task configuration, and a server without a restart
    /// policy gets [`RestartPolicy::default`].
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when `id` or the server's command is not valid, or
    /// an environment value is a plain credential or a malformed `secret://` reference.
    pub fn new(id: impl Into<String>, mut server: McpServerConfig) -> Result<Self> {
        server.task_config.enable_tasks = true;
        // A supervised driver restarts with backoff unless its manifest says otherwise.
        server.restart_policy.get_or_insert_with(RestartPolicy::default);
        let manifest = Self {
            id: id.into(),
            server,
            partitions: vec![PartitionId::default()],
            sandbox: DriverSandbox::default(),
            tools: BTreeMap::new(),
            data_classes: Vec::new(),
            declaration_digests: BTreeMap::new(),
            lifecycle: DriverLifecycle::default(),
            access: DriverAccess::default(),
            sampling: false,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Parses a manifest from its TOML form.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when the text is not a manifest, names an unknown
    /// key or effect class, or fails [`new`](Self::new)'s checks.
    pub fn from_toml(text: &str) -> Result<Self> {
        let file: ManifestFile = toml::from_str(text).map_err(|error| KernelError::Config {
            reason: format!("not a driver manifest: {error}"),
        })?;
        let mut restart = None;
        if let Some(table) = file.restart {
            let defaults = RestartPolicy::default();
            restart = Some(RestartPolicy {
                initial_delay_ms: table.initial_delay_ms.unwrap_or(defaults.initial_delay_ms),
                max_delay_ms: table.max_delay_ms.unwrap_or(defaults.max_delay_ms),
                backoff_multiplier: table.backoff_multiplier.unwrap_or(defaults.backoff_multiplier),
                max_restart_attempts: table.max_attempts.unwrap_or(defaults.max_restart_attempts),
            });
        }
        let server = McpServerConfig {
            command: file.command,
            args: file.args,
            env: file.env,
            restart_policy: restart,
            lifecycle: file.protocol,
            task_config: McpTaskConfig::enabled(),
            ..McpServerConfig::default()
        };
        let mut tools = BTreeMap::new();
        for (tool, class) in file.tools {
            let class = class
                .parse::<EffectClass>()
                .map_err(|error| invalid(&file.id, format!("tool `{tool}`: {error}")))?;
            tools.insert(tool, class);
        }
        let mut manifest = Self::new(file.id, server)?;
        if let Some(partitions) = file.partitions {
            manifest.partitions = partitions.into_iter().map(PartitionId::new).collect();
        }
        manifest.sandbox = file.sandbox;
        manifest.tools = tools;
        manifest.data_classes = file.data_classes;
        manifest.lifecycle = file.lifecycle;
        manifest.access = DriverAccess {
            allow_read: file.allow_read,
            allow_write: file.allow_write,
            allow_network: file.allow_network,
            network_hosts: file.network.map(|network| network.hosts).unwrap_or_default(),
        };
        manifest.sampling = file.sampling;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reads and parses the manifest at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Io`] when the file cannot be read, and the errors of
    /// [`from_toml`](Self::from_toml) naming the file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        Self::from_toml(&text)
            .map_err(|error| KernelError::Config { reason: format!("{}: {error}", path.display()) })
    }

    /// Checks the manifest's invariants.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] naming the first violation.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || !self.id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err(invalid(
                &self.id,
                "the id must be ASCII letters, digits, `-` or `_`, as MCP server ids are",
            ));
        }
        if self.server.command.trim().is_empty() {
            return Err(invalid(&self.id, "the command is empty"));
        }
        if !self.access.network_hosts.is_empty()
            && (!self.access.allow_network || self.sandbox != DriverSandbox::Os)
        {
            return Err(invalid(
                &self.id,
                "network hosts are enforced only by the OS sandbox's egress proxy: set \
                 sandbox = \"os\" and leave allow_network on, or drop the hosts",
            ));
        }
        if self.partitions.is_empty() {
            return Err(invalid(&self.id, "it serves no partition; list at least one"));
        }
        for (name, value) in &self.server.env {
            match secret_reference(value) {
                Some(Err(reason)) => {
                    return Err(invalid(&self.id, format!("env `{name}`: {reason}")));
                }
                Some(Ok(_)) => {}
                None if names_a_credential(name) && !value.is_empty() => {
                    return Err(invalid(
                        &self.id,
                        format!(
                            "env `{name}` holds a plain value under a credential's name; write \
                             `secret://<path>` and bind the secret in the credential broker"
                        ),
                    ));
                }
                None => {}
            }
        }
        Ok(())
    }

    /// The effect class of `tool`: the manifest's, or [`EffectClass::Other`].
    pub fn class_of(&self, tool: &str) -> EffectClass {
        self.tools.get(tool).copied().unwrap_or(EffectClass::Other)
    }

    /// Sets the class of `tool`.
    #[must_use]
    pub fn with_tool(mut self, tool: impl Into<String>, class: EffectClass) -> Self {
        self.tools.insert(tool.into(), class);
        self
    }

    /// Sets the partitions the driver serves.
    #[must_use]
    pub fn with_partitions(mut self, partitions: impl IntoIterator<Item = PartitionId>) -> Self {
        self.partitions = partitions.into_iter().collect();
        self
    }

    /// Sets how the process is isolated.
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: DriverSandbox) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Sets what a sandboxed process may reach.
    #[must_use]
    pub fn with_access(mut self, access: DriverAccess) -> Self {
        self.access = access;
        self
    }

    /// Sets when the process starts.
    #[must_use]
    pub fn with_lifecycle(mut self, lifecycle: DriverLifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    /// Sets whether the server may sample the harness model.
    #[must_use]
    pub fn with_sampling(mut self, sampling: bool) -> Self {
        self.sampling = sampling;
        self
    }

    /// The command line the person approving the driver sees: the command and its
    /// arguments, each quoted where the shell would need it.
    pub fn command_line(&self) -> String {
        std::iter::once(&self.server.command)
            .chain(&self.server.args)
            .map(|word| shell_word(word))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The digest of what runs: command, arguments, environment (references, never
    /// secrets), handshake, sandbox and access (spec 3.2 §9.3 `commandDigest`).
    pub fn command_digest(&self) -> String {
        let env: BTreeMap<&String, &String> = self.server.env.iter().collect();
        adk_core::json_digest(&serde_json::json!({
            "command": self.server.command,
            "args": self.server.args,
            "env": env,
            "protocol": self.server.lifecycle,
            "sandbox": self.sandbox,
            "access": self.access,
        }))
    }

    /// The `secret://` references of the environment, by variable name.
    pub fn secret_references(&self) -> BTreeMap<String, url::Url> {
        self.server
            .env
            .iter()
            .filter_map(|(name, value)| match secret_reference(value) {
                Some(Ok(reference)) => Some((name.clone(), reference)),
                _ => None,
            })
            .collect()
    }

    /// This user-scope manifest restricted by `project`, a workspace's manifest for the
    /// same driver (spec 3.2 §7.7, 3.1 I3).
    ///
    /// A project manifest may narrow the partitions, the sandbox access and sampling, move
    /// the driver from eager to lazy start, require a sandbox the user left off, and raise
    /// a tool's class to a stricter one. It may not change what runs: the command, the
    /// arguments, the environment, the handshake and the restart policy.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::drivers::{DriverManifest, EffectClass};
    ///
    /// let user = DriverManifest::from_toml(r#"
    ///     id = "email"
    ///     command = "mcp-email"
    ///     partitions = ["work", "home"]
    ///     [tools]
    ///     send_draft = "write_local"
    /// "#).unwrap();
    /// let project = DriverManifest::from_toml(r#"
    ///     id = "email"
    ///     command = "mcp-email"
    ///     partitions = ["work"]
    ///     [tools]
    ///     send_draft = "publish_public"
    /// "#).unwrap();
    /// let effective = user.restrict_with(&project).unwrap();
    /// assert_eq!(effective.partitions.len(), 1);
    /// assert_eq!(effective.class_of("send_draft"), EffectClass::PublishPublic);
    ///
    /// // Loosening is refused.
    /// assert!(project.restrict_with(&user).is_err());
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] naming what the project manifest tried to loosen
    /// or change.
    pub fn restrict_with(&self, project: &DriverManifest) -> Result<DriverManifest> {
        let refuse = |what: &str| {
            Err(invalid(
                &self.id,
                format!("a project manifest may only restrict a driver; it {what}"),
            ))
        };
        if project.id != self.id {
            return refuse(&format!("names driver `{}` instead", project.id));
        }
        let (mine, theirs) = (&self.server, &project.server);
        if mine.command != theirs.command
            || mine.args != theirs.args
            || mine.env != theirs.env
            || mine.lifecycle != theirs.lifecycle
            || mine.restart_policy != theirs.restart_policy
        {
            return refuse(
                "changes what runs: the command, arguments, environment, protocol or restart",
            );
        }
        let allowed: BTreeSet<&PartitionId> = self.partitions.iter().collect();
        if let Some(extra) = project.partitions.iter().find(|p| !allowed.contains(p)) {
            return refuse(&format!("adds partition `{extra}`"));
        }
        if project.sandbox != self.sandbox && self.sandbox != DriverSandbox::Unsandboxed {
            return refuse(&format!(
                "changes the sandbox from `{}` to `{}`",
                self.sandbox.as_str(),
                project.sandbox.as_str()
            ));
        }
        if let Some(path) =
            project.access.allow_read.iter().find(|p| !self.access.allow_read.contains(p))
        {
            return refuse(&format!("grants read access to {}", path.display()));
        }
        if let Some(path) =
            project.access.allow_write.iter().find(|p| !self.access.allow_write.contains(p))
        {
            return refuse(&format!("grants write access to {}", path.display()));
        }
        if project.access.allow_network && !self.access.allow_network {
            return refuse("grants network access");
        }
        // A project allowlist narrows an open network; under the user's allowlist each of its
        // hosts must be one the user's admits.
        if project.access.allow_network
            && let Some(mine) = self.access.egress_policy()
        {
            if project.access.network_hosts.is_empty() {
                return refuse("opens the network the user's manifest limits to its hosts");
            }
            if let Some(host) = project.access.network_hosts.iter().find(|host| !mine.covers(host))
            {
                return refuse(&format!("grants network access to {host}"));
            }
        }
        if project.sampling && !self.sampling {
            return refuse("lets the server sample");
        }
        if project.lifecycle == DriverLifecycle::Eager && self.lifecycle == DriverLifecycle::Lazy {
            return refuse("starts the driver eagerly");
        }
        let mut tools = self.tools.clone();
        for (tool, class) in &project.tools {
            let current = self.class_of(tool);
            if class.strictness() < current.strictness() {
                return refuse(&format!("loosens tool `{tool}` from `{current}` to `{class}`"));
            }
            tools.insert(tool.clone(), *class);
        }
        let mut effective = self.clone();
        effective.partitions = project.partitions.clone();
        effective.sandbox = project.sandbox;
        effective.access = project.access.clone();
        effective.sampling = project.sampling;
        effective.lifecycle = project.lifecycle;
        effective.tools = tools;
        Ok(effective)
    }
}

/// `Some(Ok(url))` for a well-formed `secret://` value, `Some(Err(reason))` for a
/// malformed one, `None` for a plain value.
fn secret_reference(value: &str) -> Option<std::result::Result<url::Url, String>> {
    let rest = value.strip_prefix(SECRET_SCHEME)?.strip_prefix("://")?;
    Some(match url::Url::parse(value) {
        Ok(url) if !rest.is_empty() && url.host_str().is_some_and(|host| !host.is_empty()) => {
            Ok(url)
        }
        Ok(_) => Err(format!("`{value}` names no secret; write secret://<scope>/<name>")),
        Err(error) => Err(format!("`{value}` is not a secret reference: {error}")),
    })
}

/// `word` as the shell would need it written.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
    if plain { word.to_string() } else { format!("'{}'", word.replace('\'', "'\\''")) }
}

/// Reads every `*.toml` manifest in `dir`, in file-name order; an absent directory has none.
///
/// # Errors
///
/// Returns the first manifest's error, naming its file, or [`KernelError::Config`] when
/// two files declare the same driver id.
pub fn load_dir(dir: impl AsRef<Path>) -> Result<Vec<DriverManifest>> {
    let dir = dir.as_ref();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
        .collect();
    paths.sort();
    let mut manifests: Vec<DriverManifest> = Vec::with_capacity(paths.len());
    for path in paths {
        let manifest = DriverManifest::from_file(&path)?;
        if manifests.iter().any(|known| known.id == manifest.id) {
            return Err(KernelError::Config {
                reason: format!(
                    "{}: driver `{}` is declared twice in {}",
                    path.display(),
                    manifest.id,
                    dir.display()
                ),
            });
        }
        manifests.push(manifest);
    }
    Ok(manifests)
}

/// The manifests of the user's drivers directory, each restricted by the workspace's
/// manifest of the same id (spec 3.2 §7.7: a project scope may only restrict).
///
/// # Example
///
/// ```rust
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("email.toml"), "id = \"email\"\ncommand = \"mcp-email\"\n").unwrap();
/// let manifests = adk_kernel::drivers::load_manifests(dir.path(), None).unwrap();
/// assert_eq!(manifests[0].id, "email");
/// ```
///
/// # Errors
///
/// Returns the errors of [`load_dir`], and [`KernelError::Config`] when a workspace
/// manifest names a driver the user did not declare or loosens one.
pub fn load_manifests(
    user_dir: impl AsRef<Path>,
    project_dir: Option<&Path>,
) -> Result<Vec<DriverManifest>> {
    let mut manifests = load_dir(user_dir)?;
    let Some(project_dir) = project_dir else {
        return Ok(manifests);
    };
    for project in load_dir(project_dir)? {
        let Some(user) = manifests.iter_mut().find(|user| user.id == project.id) else {
            return Err(KernelError::Config {
                reason: format!(
                    "{}: driver `{}` is not in the user's drivers; a workspace may only \
                     restrict drivers the user declared",
                    project_dir.display(),
                    project.id
                ),
            });
        };
        *user = user.restrict_with(&project)?;
    }
    Ok(manifests)
}

#[cfg(test)]
mod tests {
    use adk_core::ToolEffect;

    use super::*;

    #[test]
    fn network_hosts_limit_the_driver_and_a_project_can_only_narrow_them() {
        let base = "id = \"mail\"\ncommand = \"mcp-mail\"\nsandbox = \"os\"\n";
        let user = DriverManifest::from_toml(&format!(
            "{base}network = {{ hosts = [\"imap.example.com:993\", \"*.example.com:443\"] }}\n"
        ))
        .unwrap();
        let policy = user.access.egress_policy().expect("hosts make an egress policy");
        assert!(policy.check_url("https://api.example.com/").is_ok());
        assert!(policy.check_url("https://evil.test/").is_err());

        let narrower = DriverManifest::from_toml(&format!(
            "{base}network = {{ hosts = [\"api.example.com:443\"] }}\n"
        ))
        .unwrap();
        let effective = user.restrict_with(&narrower).unwrap();
        assert_eq!(effective.access.network_hosts, narrower.access.network_hosts);
        for wider in
            [format!("{base}network = {{ hosts = [\"evil.test:443\"] }}\n"), base.to_string()]
        {
            let project = DriverManifest::from_toml(&wider).unwrap();
            assert!(user.restrict_with(&project).is_err(), "{wider}");
        }
        let denied = DriverManifest::from_toml(&format!("{base}allow_network = false\n")).unwrap();
        assert!(user.restrict_with(&denied).is_ok());
        assert_eq!(denied.access.egress_policy(), None);
        for refused in [
            format!("{base}network = {{ hosts = [\"https://x\"] }}\n"),
            format!("{base}allow_network = false\nnetwork = {{ hosts = [\"x.example.com\"] }}\n"),
            "id = \"mail\"\ncommand = \"mcp-mail\"\nsandbox = \"unsandboxed\"\n\
             network = { hosts = [\"x.example.com\"] }\n"
                .to_string(),
        ] {
            assert!(DriverManifest::from_toml(&refused).is_err(), "{refused}");
        }
    }

    const EMAIL: &str = r#"
        id          = "email"
        command     = "mcp-email"
        args        = ["--imap"]
        env         = { EMAIL_TOKEN = "secret://email/token", LOG_LEVEL = "info" }
        partitions  = ["work"]
        lifecycle   = "lazy"
        sandbox     = "os"
        restart     = { max_attempts = 5, initial_delay_ms = 500, max_delay_ms = 30000, backoff_multiplier = 2.0 }

        [tools]
        list_inbox   = "read"
        get_email    = "read"
        create_draft = "write_local"
        send_draft   = "send_external"
    "#;

    #[test]
    fn the_spec_manifest_parses() {
        let manifest = DriverManifest::from_toml(EMAIL).unwrap();
        assert_eq!(manifest.id, "email");
        assert_eq!(manifest.server.command, "mcp-email");
        assert_eq!(manifest.server.args, ["--imap"]);
        assert_eq!(manifest.partitions, [PartitionId::new("work")]);
        assert_eq!(manifest.lifecycle, DriverLifecycle::Lazy);
        assert_eq!(manifest.sandbox, DriverSandbox::Os);
        assert_eq!(
            manifest.server.restart_policy,
            Some(RestartPolicy {
                initial_delay_ms: 500,
                max_delay_ms: 30_000,
                backoff_multiplier: 2.0,
                max_restart_attempts: 5,
            })
        );
        assert!(manifest.server.task_config.enable_tasks);
        assert_eq!(manifest.class_of("list_inbox"), EffectClass::Read);
        assert_eq!(manifest.class_of("create_draft"), EffectClass::WriteLocal);
        assert_eq!(manifest.class_of("send_draft"), EffectClass::SendExternal);
        assert_eq!(manifest.class_of("unknown_tool"), EffectClass::Other);
        assert!(manifest.declaration_digests.is_empty());
        let secrets = manifest.secret_references();
        assert_eq!(secrets["EMAIL_TOKEN"].as_str(), "secret://email/token");
        assert!(!secrets.contains_key("LOG_LEVEL"));
        assert_eq!(manifest.command_line(), "mcp-email --imap");
    }

    #[test]
    fn every_effect_class_parses_and_prints_as_itself() {
        for class in EffectClass::ALL {
            assert_eq!(class.as_str().parse::<EffectClass>().unwrap(), class);
            assert_eq!(serde_json::to_value(class).unwrap(), class.as_str());
        }
        assert_eq!("Send-External".parse::<EffectClass>().unwrap(), EffectClass::SendExternal);
        assert!("teleport".parse::<EffectClass>().is_err());
        let manifest =
            DriverManifest::from_toml("id = \"x\"\ncommand = \"x\"\n[tools]\na = \"teleport\"\n");
        assert!(manifest.unwrap_err().to_string().contains("teleport"));
    }

    #[test]
    fn only_publish_public_and_financial_are_never_auto_allowed() {
        let never: Vec<_> =
            EffectClass::ALL.into_iter().filter(|class| class.is_never_auto_allowed()).collect();
        assert_eq!(never, [EffectClass::PublishPublic, EffectClass::Financial]);
        for class in EffectClass::ALL {
            let effect = class.tool_effect();
            assert_eq!(effect == ToolEffect::ReadOnly, class == EffectClass::Read, "{class}");
        }
    }

    #[test]
    fn defaults_are_sandboxed_lazy_and_in_the_default_partition() {
        let manifest = DriverManifest::from_toml("id = \"fs\"\ncommand = \"mcp-fs\"\n").unwrap();
        assert_eq!(manifest.partitions, [PartitionId::default()]);
        assert_eq!(manifest.sandbox, DriverSandbox::Os);
        assert_eq!(manifest.lifecycle, DriverLifecycle::Lazy);
        assert!(manifest.access.allow_network);
        assert!(!manifest.sampling);
        assert_eq!(manifest.server.lifecycle, McpLifecycleMode::Initialize);
        assert_eq!(manifest.server.restart_policy, Some(RestartPolicy::default()));
    }

    #[test]
    fn bad_manifests_are_refused_with_the_reason() {
        let cases = [
            ("id = \"a b\"\ncommand = \"x\"\n", "the id must be"),
            ("id = \"a\"\ncommand = \" \"\n", "command is empty"),
            ("id = \"a\"\ncommand = \"x\"\npartitions = []\n", "serves no partition"),
            ("id = \"a\"\ncommand = \"x\"\nenv = { API_TOKEN = \"abc\" }\n", "credential"),
            ("id = \"a\"\ncommand = \"x\"\nenv = { GH_PASSWORD = \"abc\" }\n", "credential"),
            ("id = \"a\"\ncommand = \"x\"\nenv = { TOKEN = \"secret://\" }\n", "secret"),
            ("id = \"a\"\ncommand = \"x\"\nsurprise = 1\n", "unknown field"),
            ("id = \"a\"\ncommand = \"x\"\nsandbox = \"vm\"\n", "unknown variant"),
        ];
        for (text, expected) in cases {
            let error = DriverManifest::from_toml(text).unwrap_err().to_string();
            assert!(error.contains(expected), "{text}: {error}");
        }
        // A credential's name with a reference, or a non-credential name with a value, is fine.
        DriverManifest::from_toml(
            "id = \"a\"\ncommand = \"x\"\nenv = { API_TOKEN = \"secret://a/t\", TOKENIZER = \"bpe\" }\n",
        )
        .unwrap();
    }

    #[test]
    fn a_project_manifest_may_only_restrict() {
        let user = DriverManifest::from_toml(
            r#"
            id = "email"
            command = "mcp-email"
            partitions = ["work", "home"]
            sandbox = "unsandboxed"
            allow_read = ["/srv/mail"]
            lifecycle = "eager"
            sampling = true
            [tools]
            send_draft = "send_external"
            list_inbox = "read"
        "#,
        )
        .unwrap();
        let narrow = |extra: &str| {
            DriverManifest::from_toml(&format!(
                "id = \"email\"\ncommand = \"mcp-email\"\npartitions = [\"work\"]\n{extra}"
            ))
            .unwrap()
        };

        let effective = user
            .restrict_with(&narrow(
                "sandbox = \"os\"\nallow_network = false\n[tools]\nlist_inbox = \"delete\"\n",
            ))
            .unwrap();
        assert_eq!(effective.partitions, [PartitionId::new("work")]);
        assert_eq!(effective.sandbox, DriverSandbox::Os);
        assert!(!effective.access.allow_network);
        assert!(!effective.sampling);
        assert_eq!(effective.lifecycle, DriverLifecycle::Lazy);
        assert_eq!(effective.class_of("list_inbox"), EffectClass::Delete);
        assert_eq!(effective.class_of("send_draft"), EffectClass::SendExternal);

        let error = user
            .restrict_with(
                &DriverManifest::from_toml(
                    "id = \"email\"\ncommand = \"mcp-email\"\npartitions = [\"work\", \"play\"]\n",
                )
                .unwrap(),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("adds partition"), "{error}");
        for (project, loosens) in [
            ("[tools]\nsend_draft = \"read\"\n", "loosens tool"),
            ("allow_write = [\"/\"]\n", "write access"),
            ("allow_read = [\"/etc\"]\n", "read access"),
        ] {
            let error = user.restrict_with(&narrow(project)).unwrap_err().to_string();
            assert!(error.contains(loosens), "{error}");
        }
        let other_command = DriverManifest::from_toml(
            "id = \"email\"\ncommand = \"evil\"\npartitions = [\"work\"]\n",
        )
        .unwrap();
        assert!(user.restrict_with(&other_command).unwrap_err().to_string().contains("what runs"));

        // A sandboxed user manifest keeps its sandbox.
        let sandboxed = narrow("");
        let unsandboxed = narrow("sandbox = \"unsandboxed\"\n");
        assert!(sandboxed.restrict_with(&unsandboxed).is_err());
    }

    #[test]
    fn load_manifests_restricts_by_the_workspace_and_refuses_new_drivers() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("email.toml"), EMAIL).unwrap();
        std::fs::write(user.path().join("notes.txt"), "not a manifest").unwrap();
        std::fs::write(
            project.path().join("email.toml"),
            EMAIL.replace("send_external", "publish_public"),
        )
        .unwrap();
        let manifests = load_manifests(user.path(), Some(project.path())).unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].class_of("send_draft"), EffectClass::PublishPublic);

        std::fs::write(
            project.path().join("calendar.toml"),
            "id = \"calendar\"\ncommand = \"c\"\n",
        )
        .unwrap();
        let error = load_manifests(user.path(), Some(project.path())).unwrap_err().to_string();
        assert!(error.contains("may only restrict drivers the user declared"), "{error}");
        assert!(load_dir(user.path().join("missing")).unwrap().is_empty());
    }

    #[test]
    fn the_command_digest_changes_with_what_runs() {
        let manifest = DriverManifest::from_toml(EMAIL).unwrap();
        let mut other = manifest.clone();
        other.server.args.push("--debug".into());
        assert_ne!(manifest.command_digest(), other.command_digest());
        let mut reclassed = manifest.clone();
        reclassed.tools.clear();
        assert_eq!(manifest.command_digest(), reclassed.command_digest());
        let quoted = DriverManifest::new(
            "q",
            McpServerConfig {
                command: "/opt/my tools/srv".into(),
                args: vec!["it's".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(quoted.command_line(), "'/opt/my tools/srv' 'it'\\''s'");
    }

    #[test]
    fn a_manifest_round_trips_through_json() {
        let mut manifest = DriverManifest::from_toml(EMAIL).unwrap();
        manifest.declaration_digests.insert("list_inbox".into(), "sha256:ab".into());
        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["sandbox"], "os");
        assert_eq!(json["tools"]["send_draft"], "send_external");
        let back: DriverManifest = serde_json::from_value(json).unwrap();
        assert_eq!(back, manifest);
    }
}
