//! `HarnessArgs`: the flags every harness command shares, and their precedence over
//! `.adk/harness.toml` and the user's config (spec 3.1 §8.1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use adk_core::{AdkError, Effort, Result};
use adk_harness::hooks::{HookSet, HookSource};
use adk_harness::{EffortPolicy, HarnessMode, MemberSpec, Pack, Preset};
use serde::Deserialize;

/// The project file, relative to the workspace root.
pub const PROJECT_CONFIG_FILE: &str = ".adk/harness.toml";

/// The flags every harness command shares (spec 3.1 §8.1).
///
/// Flattened into the top level (`adk-rust` with no subcommand is the harness REPL) and
/// into `code`, `goal`, `ultracode` and `serve`. [`resolve`](Self::resolve) applies the
/// precedence flags → `.adk/harness.toml` → user config (`<config dir>/adk-rust/harness.toml`)
/// → defaults. The project file can only tighten: it may choose a stricter mode, turn the
/// sandbox on, or require it, and anything else it sets is ignored with a note
/// (spec 3.1 §9.2 I3).
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::HarnessArgs;
/// use adk_harness::HarnessMode;
/// use clap::Parser;
///
/// #[derive(Parser)]
/// struct Cli {
///     #[command(flatten)]
///     harness: HarnessArgs,
/// }
///
/// let cli = Cli::parse_from(["adk-rust", "--mode", "plan", "--dir", "/tmp"]);
/// assert_eq!(cli.harness.mode, Some(HarnessMode::Plan));
/// ```
#[derive(clap::Args, Debug, Clone, Default, PartialEq)]
pub struct HarnessArgs {
    /// Workspace directory; file tools are confined to it (default: the current
    /// directory, which may not be your home directory or /)
    #[arg(short = 'd', long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Permission mode: read-only, plan, ask (default) or auto
    #[arg(long, value_parser = parse_mode, value_name = "MODE")]
    pub mode: Option<HarnessMode>,

    /// Approve held calls without asking; never overrides a policy denial and never
    /// approves an unsandboxed retry without --allow-unsandboxed
    #[arg(long)]
    pub yes: bool,

    /// Continue the session with this id, creating it when it does not exist
    #[arg(long, conflicts_with_all = ["continue_session", "ephemeral"])]
    pub session: Option<String>,

    /// Continue the most recent session in this workspace
    #[arg(long = "continue", conflicts_with = "ephemeral")]
    pub continue_session: bool,

    /// Keep the conversation in memory only; nothing is written to disk
    #[arg(long)]
    pub ephemeral: bool,

    /// Run bash in the OS sandbox (Seatbelt on macOS, bubblewrap on Linux)
    #[arg(long)]
    pub sandbox: bool,

    /// Let --yes approve an unsandboxed retry of a command the sandbox refused
    #[arg(long)]
    pub allow_unsandboxed: bool,

    /// Refuse to start when the sandbox is unavailable instead of running bash on the
    /// host behind approvals (requires --sandbox)
    #[arg(long, requires = "sandbox")]
    pub fail_if_unavailable: bool,

    /// Trust the workspace's instruction files (AGENTS.md) without asking
    #[arg(long)]
    pub trust: bool,

    /// Add a pack: browser, computer-use, payments or voice
    #[arg(long = "pack", value_parser = parse_pack, value_name = "PACK")]
    pub packs: Vec<Pack>,

    /// Add a team member: NAME=explore, NAME=general, NAME=acp:COMMAND or NAME=a2a:URL
    #[arg(long = "member", value_parser = parse_member, value_name = "NAME=KIND")]
    pub members: Vec<MemberArg>,

    /// Turn long-term memory off: nothing is recalled or learned (spec 3.1 §7.15.2)
    #[arg(long)]
    pub no_memory: bool,

    /// Turn the kernel's consolidation tick off: `adk-rust up` neither distils missed
    /// sessions nor tidies assumed facts (spec 3.2 §7.5)
    #[arg(long)]
    pub no_consolidation: bool,

    /// Declare web_fetch (on by default in this binary; spec 3.1 §7.20)
    #[arg(long, conflicts_with = "no_web")]
    pub web: bool,

    /// Declare no web tools
    #[arg(long = "no-web")]
    pub no_web: bool,

    /// Host fetched without approval: exact, or *.example.com (repeatable)
    #[arg(long = "allow-domain", value_name = "HOST")]
    pub allow_domains: Vec<String>,

    /// Where web search runs: provider (the model provider's built-in search, the
    /// default), off, searxng (SEARXNG_URL or `[web] searxng_url`) or deepseek
    /// (DEEPSEEK_API_KEY); spec 3.1 §7.20
    #[arg(long, value_parser = parse_search, value_name = "SEARCH")]
    pub search: Option<SearchArg>,
    /// Reasoning effort of this session's model clients: low, medium or high
    /// (default: the `[effort]` policy, medium in ask; spec 3.1 §7.23)
    #[arg(long, value_parser = parse_effort, value_name = "LEVEL")]
    pub effort: Option<Effort>,

    /// Where the key sealing harness.db is kept: `keychain:<entry>`, `secret:<provider>/<name>`
    /// or `env:<VAR>` (default: `[store] key` in the user config; spec 3.1 §7.12)
    #[arg(long, value_name = "SOURCE")]
    pub store_key: Option<String>,

    /// Run no hooks, not even the workspace's (spec 3.1 §7.22)
    #[arg(long = "no-hooks")]
    pub no_hooks: bool,

    /// Where plans and goal graphs are checkpointed: sqlite://, postgres://, redis://,
    /// mongodb://, neo4j:// or firestore:// (default: `[plan] checkpointer` in the user
    /// config, else the store the sessions are in; spec 3.1 §7.11)
    #[arg(long, value_name = "URL", conflicts_with = "ephemeral")]
    pub checkpointer: Option<String>,

    /// Preset: coding, or personal — a workspace of notes and files under the kernel home
    /// instead of a repository, no host shell, and quiet time and a daily spending limit
    /// asked for at its first run (spec 3.3 §10 H1)
    #[arg(long, value_parser = parse_preset, value_name = "PRESET")]
    pub preset: Option<Preset>,
}

/// A `--member` flag value (spec 3.1 §8.1, §7.10).
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::{MemberArg, MemberKindArg};
///
/// let member: MemberArg = "reviewer=acp:codex".parse().unwrap();
/// assert_eq!(member.kind, MemberKindArg::Acp("codex".into()));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberArg {
    /// The member's name, as the coordinator delegates to it.
    pub name: String,
    /// What the member is.
    pub kind: MemberKindArg,
}

/// What a `--member` names (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_cli::harness::MemberKindArg::Explore, adk_cli::harness::MemberKindArg::Explore);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberKindArg {
    /// [`MemberSpec::explore`]: read-only investigation.
    Explore,
    /// [`MemberSpec::general`]: a self-contained subtask with the full tool set.
    General,
    /// An ACP-compatible coding agent started with this command.
    Acp(String),
    /// A remote agent over A2A at this URL.
    A2a(String),
}

impl std::str::FromStr for MemberArg {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let (name, kind) =
            value.split_once('=').ok_or_else(|| format!("expected NAME=KIND, got '{value}'"))?;
        let name = name.trim();
        if name.is_empty()
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("member name '{name}' must be letters, digits, '_' or '-'"));
        }
        let kind = match kind.trim() {
            "explore" => MemberKindArg::Explore,
            "general" => MemberKindArg::General,
            other => match other.split_once(':') {
                Some(("acp", command)) if !command.trim().is_empty() => {
                    MemberKindArg::Acp(command.trim().to_string())
                }
                Some(("a2a", url)) if !url.trim().is_empty() => {
                    MemberKindArg::A2a(url.trim().to_string())
                }
                _ => {
                    return Err(format!(
                        "unknown member kind '{other}'; use explore, general, acp:COMMAND or a2a:URL"
                    ));
                }
            },
        };
        Ok(Self { name: name.to_string(), kind })
    }
}

impl MemberArg {
    /// The member as a [`MemberSpec`] for `HarnessAgentBuilder::member`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for ACP and A2A members, which this build cannot
    /// create yet (spec 3.1 §7.10).
    pub fn to_spec(&self) -> Result<MemberSpec> {
        let mut spec = match &self.kind {
            MemberKindArg::Explore => MemberSpec::explore(),
            MemberKindArg::General => MemberSpec::general(),
            // wave B: team-graph — MemberSpec::reviewer_acp(command) and the A2A member
            // kind arrive with spec 3.1 §7.10 (`team-acp`, `team-a2a`).
            MemberKindArg::Acp(_) | MemberKindArg::A2a(_) => {
                return Err(AdkError::config(format!(
                    "member '{}': ACP and A2A members are not available in this build \
                     (spec 3.1 §7.10); use explore or general",
                    self.name
                )));
            }
        };
        spec.name = self.name.clone();
        Ok(spec)
    }
}

/// Where web search runs: `--search` and `[web] search` (spec 3.1 §7.20, §8.1).
///
/// A configured search provider replaces the model provider's built-in search with the
/// governed `web_search` tool: policy, screening and a per-call record.
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::SearchArg;
///
/// assert_eq!("provider".parse::<SearchArg>(), Ok(SearchArg::Provider));
/// assert_eq!("searxng".parse::<SearchArg>(), Ok(SearchArg::Searxng));
/// assert_eq!(SearchArg::default(), SearchArg::Provider);
/// assert!("bing".parse::<SearchArg>().is_err());
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchArg {
    /// The built-in search of the provider the model's client serves (Gemini, Anthropic,
    /// OpenAI Responses, DeepSeek Responses); none for a provider without one.
    #[default]
    Provider,
    /// No web search.
    Off,
    /// The governed `web_search` through a SearXNG instance: `[web] searxng_url` in the
    /// user config, else `SEARXNG_URL`.
    Searxng,
    /// The governed `web_search` through DeepSeek's Search API, keyed from
    /// `DEEPSEEK_API_KEY`.
    DeepSeek,
}

impl std::str::FromStr for SearchArg {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "provider" => Ok(Self::Provider),
            "off" | "none" => Ok(Self::Off),
            "searxng" => Ok(Self::Searxng),
            "deepseek" => Ok(Self::DeepSeek),
            other => {
                Err(format!("unknown search '{other}'; use provider, off, searxng or deepseek"))
            }
        }
    }
}

impl std::fmt::Display for SearchArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Provider => "provider",
            Self::Off => "off",
            Self::Searxng => "searxng",
            Self::DeepSeek => "deepseek",
        })
    }
}

fn parse_search(value: &str) -> std::result::Result<SearchArg, String> {
    value.parse()
}

fn parse_mode(value: &str) -> std::result::Result<HarnessMode, String> {
    match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "read-only" | "readonly" => Ok(HarnessMode::ReadOnly),
        "plan" => Ok(HarnessMode::Plan),
        "ask" => Ok(HarnessMode::Ask),
        "auto" => Ok(HarnessMode::Auto),
        other => Err(format!("unknown mode '{other}'; use read-only, plan, ask or auto")),
    }
}

fn parse_effort(value: &str) -> std::result::Result<Effort, String> {
    value.parse()
}

fn parse_pack(value: &str) -> std::result::Result<Pack, String> {
    match value.trim() {
        "browser" => Ok(Pack::Browser),
        "computer-use" => Ok(Pack::ComputerUse),
        "payments" => Ok(Pack::Payments),
        "voice" => Ok(Pack::Voice),
        other => {
            Err(format!("unknown pack '{other}'; use browser, computer-use, payments or voice"))
        }
    }
}

fn parse_member(value: &str) -> std::result::Result<MemberArg, String> {
    value.parse()
}

fn parse_preset(value: &str) -> std::result::Result<Preset, String> {
    value.parse()
}

/// `harness.toml`, in the project (`.adk/harness.toml`) or the user's config directory.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    mode: Option<String>,
    yes: Option<bool>,
    sandbox: Option<bool>,
    allow_unsandboxed: Option<bool>,
    fail_if_unavailable: Option<bool>,
    trust: Option<bool>,
    packs: Option<Vec<String>>,
    members: Option<Vec<String>>,
    /// `web = true|false`, or a `[web]` table (spec 3.1 §7.20).
    web: Option<WebKey>,
    allow_domains: Option<Vec<String>>,
    /// `[effort]`: the reasoning effort per mode and role (spec 3.1 §7.23).
    effort: Option<EffortPolicy>,
    /// `[store]`: where the store key is kept (spec 3.1 §7.12); the user config only.
    store: Option<StoreTable>,
    /// `[plan]`: where plans are checkpointed (spec 3.1 §7.11); the user config only.
    plan: Option<PlanTable>,
    /// `[shells]`: background shell limits (spec 3.1 §7.29); a project file may only lower
    /// them.
    shells: Option<ShellsTable>,
    /// `[[hooks]]` tables (spec 3.1 §7.22): the user config's are read by
    /// `HookSet::load_file`, the workspace's by the harness at build, so they are neither
    /// applied here nor reported as unknown.
    #[serde(default)]
    #[allow(dead_code)]
    hooks: Vec<toml::Value>,
    /// `[ultracode]` (spec 3.1 §7.10): read by `adk-rust ultracode` through
    /// `UltracodeLimits::resolve`, so it is neither applied here nor reported as unknown.
    #[serde(default)]
    #[allow(dead_code)]
    ultracode: Option<toml::Value>,
    /// `[kernel]` (spec 3.2 §8.2): read by `adk-rust up` through `KernelSettings::resolve`,
    /// so it is neither applied here nor reported as unknown.
    #[serde(default)]
    #[allow(dead_code)]
    kernel: Option<toml::Value>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

/// `web` in `harness.toml`: whether web tools are declared, or the `[web]` table.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WebKey {
    Enabled(bool),
    Table(WebTable),
}

impl WebKey {
    fn enabled(&self) -> Option<bool> {
        match self {
            Self::Enabled(enabled) => Some(*enabled),
            Self::Table(_) => None,
        }
    }

    fn table(&self) -> Option<&WebTable> {
        match self {
            Self::Enabled(_) => None,
            Self::Table(table) => Some(table),
        }
    }
}

/// The `[web]` table: where web search runs (spec 3.1 §7.20).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WebTable {
    /// `provider`, `off`, `searxng` or `deepseek`.
    search: Option<String>,
    /// The SearXNG instance `search = "searxng"` queries.
    searxng_url: Option<String>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

/// The `[shells]` table: background shell limits (spec 3.1 §7.29).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ShellsTable {
    /// The most background shells one session runs at once.
    max_running: Option<usize>,
    /// How long a background shell of a goal, served or kernel run lives, in minutes
    /// (at most 120); the REPL sets no lifetime.
    lifetime_minutes: Option<u64>,
}

/// Background shell limits from `[shells]` (spec 3.1 §7.29); `None` keeps the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShellLimits {
    /// The most background shells one session runs at once.
    pub max_running: Option<usize>,
    /// How long a background shell of an unattended run lives.
    pub lifetime_minutes: Option<u64>,
}

impl ShellLimits {
    /// Lowers each limit to `table`'s where it sets a lower one: what a project file may do.
    fn narrow(&mut self, table: &ShellsTable) {
        let lower = |current: Option<u64>, wanted: Option<u64>, default: u64| match wanted {
            Some(wanted) => Some(wanted.min(current.unwrap_or(default))),
            None => current,
        };
        self.max_running =
            lower(self.max_running.map(|n| n as u64), table.max_running.map(|n| n as u64), 8)
                .map(|n| n as usize);
        self.lifetime_minutes = lower(self.lifetime_minutes, table.lifetime_minutes, 30);
    }

    /// `config` with these limits applied.
    pub fn apply(
        self,
        config: adk_harness::BackgroundShellConfig,
    ) -> adk_harness::BackgroundShellConfig {
        let config = match self.max_running {
            Some(count) => config.with_max_running(count),
            None => config,
        };
        match (self.lifetime_minutes, config.lifetime()) {
            // An attended REPL sets no lifetime; the limit is for unattended runs.
            (Some(minutes), Some(_)) => {
                config.with_lifetime(std::time::Duration::from_secs(minutes * 60))
            }
            _ => config,
        }
    }
}

/// The `[store]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StoreTable {
    key: Option<String>,
}

/// The `[plan]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PlanTable {
    checkpointer: Option<String>,
}

/// The `[deploy]` table is read by deployment tooling (spec 3.1 §8.5), not by the CLI.
const DEPLOY_TABLE: &str = "deploy";

impl ConfigFile {
    fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AdkError::config(format!(
                    "failed to read {}: {error}",
                    path.display()
                )));
            }
        };
        toml::from_str(&text).map(Some).map_err(|error| {
            AdkError::config(format!(
                "{} is not valid harness configuration: {error}",
                path.display()
            ))
        })
    }
}

/// [`HarnessArgs`] with `.adk/harness.toml`, the user's config and the defaults applied
/// (spec 3.1 §8.1).
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::HarnessArgs;
/// use adk_harness::HarnessMode;
///
/// let dir = tempfile::tempdir().unwrap();
/// let args = HarnessArgs { dir: Some(dir.path().to_path_buf()), ..HarnessArgs::default() };
/// let resolved = args.resolve(HarnessMode::Ask, None).unwrap();
/// assert_eq!(resolved.mode, HarnessMode::Ask);
/// assert!(!resolved.mode_explicit);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedHarnessArgs {
    /// The workspace root, canonical.
    pub workspace: PathBuf,
    /// The mode new sessions start in.
    pub mode: HarnessMode,
    /// Whether a flag or a config file chose the mode, so a resumed session switches to it.
    pub mode_explicit: bool,
    /// `--yes`.
    pub yes: bool,
    /// `--sandbox`.
    pub sandbox: bool,
    /// `--allow-unsandboxed`.
    pub allow_unsandboxed: bool,
    /// `--fail-if-unavailable`.
    pub fail_if_unavailable: bool,
    /// `--trust`.
    pub trust: bool,
    /// `--pack`.
    pub packs: Vec<Pack>,
    /// `--member`.
    pub members: Vec<MemberArg>,
    /// `--web` / `--no-web`: whether `web_fetch` is declared (spec 3.1 §7.20).
    pub web: bool,
    /// `--allow-domain`: hosts fetched without approval.
    pub allow_domains: Vec<String>,
    /// `--search` / `[web] search`: where web search runs (spec 3.1 §7.20).
    pub search: SearchArg,
    /// `[web] searxng_url` of the user config.
    pub searxng_url: Option<String>,
    /// Long-term memory: on unless `--no-memory` or `--ephemeral`.
    pub memory: bool,
    /// The kernel's consolidation tick: allowed unless `--no-consolidation`, and only with
    /// long-term memory (spec 3.2 §7.5).
    pub consolidation: bool,
    /// The reasoning effort per mode and role (spec 3.1 §7.23): `--effort` sets every
    /// level, else the `[effort]` tables, else the defaults.
    pub effort: EffortPolicy,
    /// Whether `--effort` or an `[effort]` table chose the policy.
    pub effort_explicit: bool,
    /// Where the store key is kept, from `--store-key` or the user config's `[store] key`
    /// (spec 3.1 §7.12).
    pub store_key: Option<String>,
    /// Whether hooks run: off with `--no-hooks` (spec 3.1 §7.22, §8.1).
    pub hooks: bool,
    /// Where plans and goal graphs are checkpointed, from `--checkpointer` or the user
    /// config's `[plan] checkpointer`; `None` checkpoints them beside the sessions
    /// (spec 3.1 §7.11, §13 Q5).
    pub checkpointer: Option<String>,
    /// The `[[hooks]]` of the user config, source `User` (spec 3.1 §7.22); the workspace's
    /// are read at build and run under its trust.
    pub user_hooks: HookSet,
    /// `[shells]`: background shell limits; the project file may only lower the user's
    /// (spec 3.1 §7.29).
    pub shells: ShellLimits,
    /// Settings a config file made that were ignored, and why.
    pub notes: Vec<String>,
}

impl HarnessArgs {
    /// Resolves the flags against `.adk/harness.toml` in the workspace, the user's config
    /// file `user_config` (normally `<config dir>/adk-rust/harness.toml`), and
    /// `default_mode` (spec 3.1 §8.1).
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the workspace does not exist, when no `--dir`
    /// was given and the current directory is the home directory or `/`, or when a config
    /// file does not parse.
    pub fn resolve(
        &self,
        default_mode: HarnessMode,
        user_config: Option<&Path>,
    ) -> Result<ResolvedHarnessArgs> {
        let workspace = match &self.dir {
            Some(dir) => canonical(dir)?,
            None => {
                let cwd = std::env::current_dir()
                    .map_err(|error| AdkError::config(format!("no current directory: {error}")))?;
                let cwd = canonical(&cwd)?;
                let home = dirs::home_dir().and_then(|home| home.canonicalize().ok());
                if cwd.parent().is_none() || home.as_deref() == Some(cwd.as_path()) {
                    return Err(AdkError::config(format!(
                        "refusing to use {} as the workspace: the agent could read and change \
                         everything under it. cd into a project, or pass --dir {} to mean it",
                        cwd.display(),
                        cwd.display()
                    )));
                }
                cwd
            }
        };

        let user = match user_config {
            Some(path) => ConfigFile::load(path)?,
            None => None,
        };
        let user_hooks = match user_config {
            Some(path) if user.is_some() => {
                HookSet::load_file(path, HookSource::User)?.unwrap_or_default()
            }
            _ => HookSet::default(),
        };
        let project = ConfigFile::load(&workspace.join(PROJECT_CONFIG_FILE))?;
        let mut notes = Vec::new();

        // User config: trusted for every key.
        let mut mode = default_mode;
        let mut mode_explicit = false;
        let mut yes = false;
        let mut sandbox = false;
        let mut allow_unsandboxed = false;
        let mut fail_if_unavailable = false;
        let mut trust = false;
        let mut packs = Vec::new();
        let mut members = Vec::new();
        let mut web = true;
        let mut allow_domains: Vec<String> = Vec::new();
        let mut search = SearchArg::default();
        let mut searxng_url = None;
        let mut effort = EffortPolicy::default();
        let mut effort_explicit = false;
        let mut store_key = None;
        let mut checkpointer = None;
        let mut shells = ShellLimits::default();
        if let Some(user) = &user {
            let origin = user_config.map_or_else(String::new, |path| path.display().to_string());
            if let Some(value) = &user.mode {
                mode = parse_mode(value).map_err(|error| config_error(&origin, &error))?;
                mode_explicit = true;
            }
            yes = user.yes.unwrap_or(false);
            sandbox = user.sandbox.unwrap_or(false);
            allow_unsandboxed = user.allow_unsandboxed.unwrap_or(false);
            fail_if_unavailable = user.fail_if_unavailable.unwrap_or(false);
            trust = user.trust.unwrap_or(false);
            for pack in user.packs.iter().flatten() {
                packs.push(parse_pack(pack).map_err(|error| config_error(&origin, &error))?);
            }
            for member in user.members.iter().flatten() {
                members.push(parse_member(member).map_err(|error| config_error(&origin, &error))?);
            }
            if let Some(value) = user.web.as_ref().and_then(WebKey::enabled) {
                web = value;
            }
            if let Some(table) = user.web.as_ref().and_then(WebKey::table) {
                if let Some(value) = &table.search {
                    search =
                        value.parse().map_err(|error: String| config_error(&origin, &error))?;
                }
                searxng_url = table.searxng_url.clone();
                for key in table.unknown.keys() {
                    notes.push(format!("{origin}: unknown key `web.{key}` ignored"));
                }
            }
            allow_domains.extend(user.allow_domains.iter().flatten().cloned());
            if let Some(policy) = user.effort {
                effort = policy;
                effort_explicit = true;
            }
            store_key = user.store.as_ref().and_then(|store| store.key.clone());
            checkpointer = user.plan.as_ref().and_then(|plan| plan.checkpointer.clone());
            if let Some(table) = &user.shells {
                shells = ShellLimits {
                    max_running: table.max_running,
                    lifetime_minutes: table.lifetime_minutes,
                };
            }
            for key in user.unknown.keys().filter(|key| *key != DEPLOY_TABLE) {
                notes.push(format!("{origin}: unknown key `{key}` ignored"));
            }
        }

        // Project config: workspace files may only tighten (spec 3.1 §9.2 I3).
        if let Some(project) = &project {
            let origin = PROJECT_CONFIG_FILE;
            if let Some(value) = &project.mode {
                let requested = parse_mode(value).map_err(|error| config_error(origin, &error))?;
                if requested.is_stricter_than(mode) {
                    mode = requested;
                    mode_explicit = true;
                } else if requested != mode {
                    notes.push(loosening_note(
                        "mode",
                        &format!("{requested} is looser than {mode}"),
                    ));
                }
            }
            if project.sandbox == Some(true) {
                sandbox = true;
            }
            if project.fail_if_unavailable == Some(true) {
                fail_if_unavailable = true;
            }
            if project.web.as_ref().and_then(WebKey::enabled) == Some(false) {
                web = false;
            }
            // A workspace may turn search off; it may not choose where queries go.
            if let Some(table) = project.web.as_ref().and_then(WebKey::table) {
                if let Some(value) = &table.search {
                    let requested: SearchArg =
                        value.parse().map_err(|error: String| config_error(origin, &error))?;
                    if requested == SearchArg::Off {
                        search = SearchArg::Off;
                    } else if requested != search {
                        notes.push(loosening_note(
                            "web.search",
                            "it would choose where the agent's queries go",
                        ));
                    }
                }
                if table.searxng_url.is_some() {
                    notes.push(loosening_note(
                        "web.searxng_url",
                        "it would choose where the agent's queries go",
                    ));
                }
                for key in table.unknown.keys() {
                    notes.push(format!("{origin}: unknown key `web.{key}` ignored"));
                }
            }
            // A project may lower the background shell limits, never raise them.
            if let Some(table) = &project.shells {
                shells.narrow(table);
            }
            // Effort costs money, not permissions: a project may set it; flags win.
            if let Some(policy) = project.effort {
                effort = policy;
                effort_explicit = true;
            }
            // A checkpointer location is not a permission, but a workspace file naming one
            // would send plan state to a server the repository chose.
            if project.plan.as_ref().is_some_and(|plan| plan.checkpointer.is_some()) {
                notes.push(format!(
                    "{origin}: `plan.checkpointer` ignored — where plans are written is yours to \
                     choose; set [plan] checkpointer in your user config or pass --checkpointer"
                ));
            }
            if project.store.is_some() {
                notes.push(format!(
                    "{origin}: `store` ignored — where your store key lives is yours to choose; \
                     set [store] key in your user config or pass --store-key"
                ));
            }
            for (key, set) in [
                ("yes", project.yes == Some(true)),
                ("allow_unsandboxed", project.allow_unsandboxed == Some(true)),
                ("trust", project.trust == Some(true)),
                ("packs", project.packs.as_ref().is_some_and(|packs| !packs.is_empty())),
                ("members", project.members.as_ref().is_some_and(|members| !members.is_empty())),
                (
                    "allow_domains",
                    project.allow_domains.as_ref().is_some_and(|domains| !domains.is_empty()),
                ),
            ] {
                if set {
                    notes.push(loosening_note(key, "it would loosen what the agent may do"));
                }
            }
            for key in project.unknown.keys().filter(|key| *key != DEPLOY_TABLE) {
                notes.push(format!("{origin}: unknown key `{key}` ignored"));
            }
        }

        // Flags win.
        if let Some(flag) = self.mode {
            mode = flag;
            mode_explicit = true;
        }
        yes |= self.yes;
        sandbox |= self.sandbox;
        allow_unsandboxed |= self.allow_unsandboxed;
        fail_if_unavailable |= self.fail_if_unavailable;
        trust |= self.trust;
        packs.extend(self.packs.iter().copied());
        members.extend(self.members.iter().cloned());
        if self.no_web {
            web = false;
        } else if self.web {
            web = true;
        }
        allow_domains.extend(self.allow_domains.iter().cloned());
        if let Some(flag) = self.search {
            search = flag;
        }
        if let Some(level) = self.effort {
            effort = EffortPolicy::uniform(level);
            effort_explicit = true;
        }
        if let Some(source) = &self.store_key {
            store_key = Some(source.clone());
        }
        if let Some(url) = &self.checkpointer {
            checkpointer = Some(url.clone());
        }
        if fail_if_unavailable && !sandbox {
            return Err(AdkError::config(
                "fail_if_unavailable needs the sandbox: pass --sandbox or set sandbox = true",
            ));
        }

        Ok(ResolvedHarnessArgs {
            workspace,
            mode,
            mode_explicit,
            yes,
            sandbox,
            allow_unsandboxed,
            fail_if_unavailable,
            trust,
            packs,
            members,
            memory: !self.no_memory && !self.ephemeral,
            consolidation: !self.no_memory && !self.ephemeral && !self.no_consolidation,
            web,
            allow_domains,
            search,
            searxng_url,
            effort,
            effort_explicit,
            store_key,
            hooks: !self.no_hooks,
            checkpointer,
            user_hooks,
            shells,
            notes,
        })
    }
}

fn canonical(dir: &Path) -> Result<PathBuf> {
    dir.canonicalize().map_err(|error| {
        AdkError::config(format!("workspace {} is not usable: {error}", dir.display()))
    })
}

fn config_error(origin: &str, error: &str) -> AdkError {
    AdkError::config(format!("{origin}: {error}"))
}

pub(crate) fn loosening_note(key: &str, why: &str) -> String {
    format!(
        "{PROJECT_CONFIG_FILE}: `{key}` ignored — {why}, and workspace files may only tighten. \
         Set it in your user config or pass the flag"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_in(dir: &Path) -> HarnessArgs {
        HarnessArgs { dir: Some(dir.to_path_buf()), ..HarnessArgs::default() }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn defaults_apply_without_flags_or_files() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = args_in(dir.path()).resolve(HarnessMode::Plan, None).unwrap();
        assert_eq!(
            resolved,
            ResolvedHarnessArgs {
                workspace: dir.path().canonicalize().unwrap(),
                mode: HarnessMode::Plan,
                mode_explicit: false,
                yes: false,
                sandbox: false,
                allow_unsandboxed: false,
                fail_if_unavailable: false,
                trust: false,
                packs: vec![],
                members: vec![],
                memory: true,
                consolidation: true,
                web: true,
                allow_domains: vec![],
                search: SearchArg::Provider,
                searxng_url: None,
                effort: EffortPolicy::default(),
                effort_explicit: false,
                store_key: None,
                hooks: true,
                checkpointer: None,
                user_hooks: HookSet::default(),
                shells: ShellLimits::default(),
                notes: vec![],
            }
        );
    }

    /// `[shells]` in the user config sets the limits; the project file may only lower them
    /// (spec 3.1 §7.29, §8.1).
    #[test]
    fn a_project_file_only_lowers_the_shell_limits() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        write(&user, "[shells]\nmax_running = 4\nlifetime_minutes = 60\n");
        write(
            &dir.path().join(PROJECT_CONFIG_FILE),
            "[shells]\nmax_running = 16\nlifetime_minutes = 10\n",
        );
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap();
        assert_eq!(
            resolved.shells,
            ShellLimits { max_running: Some(4), lifetime_minutes: Some(10) }
        );
        let unattended = resolved.shells.apply(adk_harness::BackgroundShellConfig::unattended());
        assert_eq!(unattended.max_running, 4);
        assert_eq!(unattended.lifetime(), Some(std::time::Duration::from_secs(600)));
        let attended = resolved.shells.apply(adk_harness::BackgroundShellConfig::attended());
        assert_eq!(attended.lifetime(), None, "the REPL sets no lifetime");
    }

    #[test]
    fn hooks_tables_are_recognised_and_the_user_hooks_load() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        write(
            &user,
            "mode = \"ask\"\n\n[[hooks]]\nevent = \"post_edit\"\ncommand = \"cargo fmt\"\n",
        );
        write(
            &dir.path().join(PROJECT_CONFIG_FILE),
            "sandbox = true\n\n[[hooks]]\nevent = \"pre_tool\"\nmatcher = \"bash\"\n\
             command = \"./check.sh\"\n",
        );
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap();
        assert!(resolved.hooks);
        assert_eq!(resolved.user_hooks.hooks.len(), 1);
        assert_eq!(resolved.user_hooks.hooks[0].source, HookSource::User);
        assert_eq!(resolved.user_hooks.hooks[0].describe(), "post_edit: cargo fmt");
        assert!(
            !resolved.notes.iter().any(|note| note.contains("hooks")),
            "[[hooks]] is a known table: {:?}",
            resolved.notes
        );

        let off = HarnessArgs { no_hooks: true, ..args_in(dir.path()) }
            .resolve(HarnessMode::Ask, Some(&user))
            .unwrap();
        assert!(!off.hooks);

        write(&user, "[[hooks]]\nevent = \"post_edit\"\n");
        let error = args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap_err();
        assert!(error.to_string().contains("[[hooks]] entry 1"), "{error}");
    }

    #[test]
    fn effort_and_store_come_from_flags_then_files() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        write(&user, "[effort]\nplan = \"low\"\n\n[store]\nkey = \"keychain:adk-rust\"\n");
        write(
            &dir.path().join(PROJECT_CONFIG_FILE),
            "[effort]\nask = \"high\"\n\n[store]\nkey = \"env:EVIL\"\n",
        );
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap();
        assert_eq!(resolved.effort, EffortPolicy { ask: Effort::High, ..EffortPolicy::default() });
        assert!(resolved.effort_explicit);
        assert_eq!(resolved.store_key.as_deref(), Some("keychain:adk-rust"));
        assert!(resolved.notes.iter().any(|note| note.contains("`store`")), "{:?}", resolved.notes);

        let flagged = HarnessArgs {
            effort: Some(Effort::Low),
            store_key: Some("env:ADK_STORE_KEY".into()),
            ..args_in(dir.path())
        }
        .resolve(HarnessMode::Ask, Some(&user))
        .unwrap();
        assert_eq!(flagged.effort, EffortPolicy::uniform(Effort::Low));
        assert_eq!(flagged.store_key.as_deref(), Some("env:ADK_STORE_KEY"));
    }

    #[test]
    fn the_checkpointer_comes_from_the_flag_then_the_user_config_never_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        write(&user, "[plan]\ncheckpointer = \"postgres://plans@db/harness\"\n");
        write(
            &dir.path().join(PROJECT_CONFIG_FILE),
            "[plan]\ncheckpointer = \"redis://attacker.example:6379\"\n",
        );
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap();
        assert_eq!(resolved.checkpointer.as_deref(), Some("postgres://plans@db/harness"));
        assert!(
            resolved.notes.iter().any(|note| note.contains("`plan.checkpointer` ignored")),
            "{:?}",
            resolved.notes
        );
        let flagged =
            HarnessArgs { checkpointer: Some("sqlite:///plans.db".into()), ..args_in(dir.path()) }
                .resolve(HarnessMode::Ask, Some(&user))
                .unwrap();
        assert_eq!(flagged.checkpointer.as_deref(), Some("sqlite:///plans.db"));
        let workspace_only = args_in(dir.path()).resolve(HarnessMode::Ask, None).unwrap();
        assert_eq!(workspace_only.checkpointer, None);
    }

    #[test]
    fn flags_beat_the_project_file_which_beats_the_user_file() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        write(&user, "mode = \"auto\"\nyes = true\n");
        write(&dir.path().join(PROJECT_CONFIG_FILE), "mode = \"plan\"\nsandbox = true\n");

        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap();
        assert_eq!(
            (resolved.mode, resolved.yes, resolved.sandbox),
            (HarnessMode::Plan, true, true)
        );

        let flagged = HarnessArgs { mode: Some(HarnessMode::Ask), ..args_in(dir.path()) }
            .resolve(HarnessMode::Ask, Some(&user))
            .unwrap();
        assert_eq!(flagged.mode, HarnessMode::Ask);
        assert!(flagged.mode_explicit);
    }

    #[test]
    fn the_project_file_can_only_tighten() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join(PROJECT_CONFIG_FILE),
            "mode = \"auto\"\nyes = true\ntrust = true\nallow_unsandboxed = true\n\
             members = [\"x=acp:evil\"]\npacks = [\"payments\"]\nsurprise = 1\n",
        );
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, None).unwrap();
        assert_eq!(resolved.mode, HarnessMode::Ask);
        assert!(!resolved.mode_explicit);
        assert!(!resolved.yes && !resolved.trust && !resolved.allow_unsandboxed);
        assert!(resolved.members.is_empty() && resolved.packs.is_empty());
        for key in ["mode", "yes", "trust", "allow_unsandboxed", "members", "packs", "surprise"] {
            assert!(
                resolved.notes.iter().any(|note| note.contains(&format!("`{key}`"))),
                "{key}: {:?}",
                resolved.notes
            );
        }
    }

    #[test]
    fn ultracode_table_is_not_an_unknown_key() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        write(&user, "[ultracode]\nmax_rounds = 3\n");
        write(
            &dir.path().join(PROJECT_CONFIG_FILE),
            "[ultracode]\narchitecture = \"graph\"\nmax_model_calls = 200\n",
        );
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap();
        assert!(
            !resolved.notes.iter().any(|note| note.contains("ultracode")),
            "[ultracode] is read by `adk-rust ultracode`: {:?}",
            resolved.notes
        );
    }

    #[test]
    fn home_and_root_need_an_explicit_dir() {
        let root = HarnessArgs::default();
        let cwd = std::env::current_dir().unwrap();
        // The test runs in the crate directory, which is neither.
        assert!(root.resolve(HarnessMode::Ask, None).is_ok());
        std::env::set_current_dir("/").unwrap();
        let refused = root.resolve(HarnessMode::Ask, None).unwrap_err();
        assert!(refused.to_string().contains("--dir"), "{refused}");
        assert!(
            HarnessArgs { dir: Some("/".into()), ..HarnessArgs::default() }
                .resolve(HarnessMode::Ask, None)
                .is_ok()
        );
        std::env::set_current_dir(cwd).unwrap();
    }

    #[test]
    fn fail_if_unavailable_needs_the_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let error = HarnessArgs { fail_if_unavailable: true, ..args_in(dir.path()) }
            .resolve(HarnessMode::Ask, None)
            .unwrap_err();
        assert!(error.to_string().contains("--sandbox"), "{error}");
    }

    #[test]
    fn search_defaults_to_the_provider_and_the_flag_wins() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        write(&user, "[web]\nsearch = \"searxng\"\nsearxng_url = \"http://localhost:8888\"\n");
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, None).unwrap();
        assert_eq!((resolved.search, resolved.searxng_url), (SearchArg::Provider, None));
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap();
        assert_eq!(resolved.search, SearchArg::Searxng);
        assert_eq!(resolved.searxng_url.as_deref(), Some("http://localhost:8888"));
        assert!(resolved.web, "a [web] table leaves web on");
        assert!(resolved.notes.is_empty(), "{:?}", resolved.notes);
        let resolved = HarnessArgs { search: Some(SearchArg::Off), ..args_in(dir.path()) }
            .resolve(HarnessMode::Ask, Some(&user))
            .unwrap();
        assert_eq!(resolved.search, SearchArg::Off);
        // `web = false` keeps its meaning beside the table form.
        write(&user, "web = false\n");
        assert!(!args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap().web);
        write(&user, "[web]\nsearch = \"bing\"\n");
        let error = args_in(dir.path()).resolve(HarnessMode::Ask, Some(&user)).unwrap_err();
        assert!(error.to_string().contains("unknown search 'bing'"), "{error}");
    }

    #[test]
    fn a_project_may_turn_search_off_but_never_choose_where_queries_go() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join(PROJECT_CONFIG_FILE),
            "[web]\nsearch = \"deepseek\"\nsearxng_url = \"http://collector.example\"\n",
        );
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, None).unwrap();
        assert_eq!((resolved.search, resolved.searxng_url), (SearchArg::Provider, None));
        assert_eq!(resolved.notes.len(), 2, "{:?}", resolved.notes);
        assert!(resolved.notes.iter().all(|note| note.contains("where the agent's queries go")));
        write(&dir.path().join(PROJECT_CONFIG_FILE), "[web]\nsearch = \"off\"\n");
        let resolved = args_in(dir.path()).resolve(HarnessMode::Ask, None).unwrap();
        assert_eq!(resolved.search, SearchArg::Off);
        assert!(resolved.notes.is_empty(), "{:?}", resolved.notes);
    }

    #[test]
    fn members_parse_and_remote_kinds_are_refused_for_now() {
        let explore: MemberArg = "scout=explore".parse().unwrap();
        assert_eq!(explore.to_spec().unwrap().name, "scout");
        assert!("x=acp:codex".parse::<MemberArg>().unwrap().to_spec().is_err());
        assert!("x=nonsense".parse::<MemberArg>().is_err());
        assert!("bad name=explore".parse::<MemberArg>().is_err());
    }
}
