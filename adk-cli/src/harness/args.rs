//! `HarnessArgs`: the flags every harness command shares, and their precedence over
//! `.adk/harness.toml` and the user's config (spec 3.1 §8.1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use adk_core::{AdkError, Effort, Result};
use adk_harness::{EffortPolicy, HarnessMode, MemberSpec, Pack};
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

    /// Add a pack: browser, computer-use or payments
    #[arg(long = "pack", value_parser = parse_pack, value_name = "PACK")]
    pub packs: Vec<Pack>,

    /// Add a team member: NAME=explore, NAME=general, NAME=acp:COMMAND or NAME=a2a:URL
    #[arg(long = "member", value_parser = parse_member, value_name = "NAME=KIND")]
    pub members: Vec<MemberArg>,

    /// Turn long-term memory off: nothing is recalled or learned (spec 3.1 §7.15.2)
    #[arg(long)]
    pub no_memory: bool,

    /// Declare web_fetch (on by default in this binary; spec 3.1 §7.20)
    #[arg(long, conflicts_with = "no_web")]
    pub web: bool,

    /// Declare no web tools
    #[arg(long = "no-web")]
    pub no_web: bool,

    /// Host fetched without approval: exact, or *.example.com (repeatable)
    #[arg(long = "allow-domain", value_name = "HOST")]
    pub allow_domains: Vec<String>,
    /// Reasoning effort of this session's model clients: low, medium or high
    /// (default: the `[effort]` policy, medium in ask; spec 3.1 §7.23)
    #[arg(long, value_parser = parse_effort, value_name = "LEVEL")]
    pub effort: Option<Effort>,

    /// Where the key sealing harness.db is kept: `keychain:<entry>`, `secret:<provider>/<name>`
    /// or `env:<VAR>` (default: `[store] key` in the user config; spec 3.1 §7.12)
    #[arg(long, value_name = "SOURCE")]
    pub store_key: Option<String>,
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
        other => Err(format!("unknown pack '{other}'; use browser, computer-use or payments")),
    }
}

fn parse_member(value: &str) -> std::result::Result<MemberArg, String> {
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
    web: Option<bool>,
    allow_domains: Option<Vec<String>>,
    /// `[effort]`: the reasoning effort per mode and role (spec 3.1 §7.23).
    effort: Option<EffortPolicy>,
    /// `[store]`: where the store key is kept (spec 3.1 §7.12); the user config only.
    store: Option<StoreTable>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

/// The `[store]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StoreTable {
    key: Option<String>,
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
    /// Long-term memory: on unless `--no-memory` or `--ephemeral`.
    pub memory: bool,
    /// The reasoning effort per mode and role (spec 3.1 §7.23): `--effort` sets every
    /// level, else the `[effort]` tables, else the defaults.
    pub effort: EffortPolicy,
    /// Whether `--effort` or an `[effort]` table chose the policy.
    pub effort_explicit: bool,
    /// Where the store key is kept, from `--store-key` or the user config's `[store] key`
    /// (spec 3.1 §7.12).
    pub store_key: Option<String>,
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
        let mut effort = EffortPolicy::default();
        let mut effort_explicit = false;
        let mut store_key = None;
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
            if let Some(value) = user.web {
                web = value;
            }
            allow_domains.extend(user.allow_domains.iter().flatten().cloned());
            if let Some(policy) = user.effort {
                effort = policy;
                effort_explicit = true;
            }
            store_key = user.store.as_ref().and_then(|store| store.key.clone());
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
            if project.web == Some(false) {
                web = false;
            }
            // Effort costs money, not permissions: a project may set it; flags win.
            if let Some(policy) = project.effort {
                effort = policy;
                effort_explicit = true;
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
        if let Some(level) = self.effort {
            effort = EffortPolicy::uniform(level);
            effort_explicit = true;
        }
        if let Some(source) = &self.store_key {
            store_key = Some(source.clone());
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
            web,
            allow_domains,
            effort,
            effort_explicit,
            store_key,
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

fn loosening_note(key: &str, why: &str) -> String {
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
                web: true,
                allow_domains: vec![],
                effort: EffortPolicy::default(),
                effort_explicit: false,
                store_key: None,
                notes: vec![],
            }
        );
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
    fn members_parse_and_remote_kinds_are_refused_for_now() {
        let explore: MemberArg = "scout=explore".parse().unwrap();
        assert_eq!(explore.to_spec().unwrap().name, "scout");
        assert!("x=acp:codex".parse::<MemberArg>().unwrap().to_spec().is_err());
        assert!("x=nonsense".parse::<MemberArg>().is_err());
        assert!("bad name=explore".parse::<MemberArg>().is_err());
    }
}
