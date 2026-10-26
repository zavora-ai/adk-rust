//! Hooks: observers with a veto on tool events (spec 3.1 §7.22).
//!
//! A hook is a command the operator, the user or the workspace configures to run on a tool
//! event through the same launcher as `bash`. It can add a Deny, fail a call or annotate
//! a result, and it can never Allow, change the mode, create a rule or answer a call
//! (spec 3.1 §9.2 I53). The outcome types make the loosening outcomes unrepresentable:
//! [`PreToolVerdict`] has no `Allow`, and [`PreToolVerdict::apply`] can only tighten a
//! decision; [`PostToolVerdict`] reaches the model only through [`HookNote::render`], a
//! delimited untrusted block.
//!
//! | Item | What it is |
//! |---|---|
//! | [`HookSpec`], [`HookSet`] | A hook and the set of hooks the harness runs |
//! | [`HookEvent`], [`HookIsolation`], [`HookSource`] | When it runs, where it runs, who configured it |
//! | [`HookOutcome`] | What a hook's stdout says; anything else is `Continue` |
//! | [`PreToolVerdict`], [`PostToolVerdict`], [`HookNote`] | What each event may do with the outcome |
//! | [`HookRunner`] | Runs hooks through a `CommandLauncher` and records `harness.hook.ran` |
//! | [`HookScope`] | The tool context a hook runs under outside a tool call |

mod runner;
mod wrap;

use std::path::Path;
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, PolicyDecision, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use runner::{HookCall, HookEdit, HookResult, HookRunner, HookScope, TurnSummary};
pub(crate) use wrap::{HookedTool, class_label};

/// The workspace file hooks are read from, relative to the workspace root (spec 3.1 §7.22).
///
/// `.adk` is a protected directory, so the model cannot write it without a non-waivable
/// approval, and the file joins the workspace's instruction digest, so a changed hook
/// invalidates the trust decision (spec 3.1 §9.2 I54).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::hooks::WORKSPACE_HOOKS_FILE, ".adk/harness.toml");
/// ```
pub const WORKSPACE_HOOKS_FILE: &str = ".adk/harness.toml";

/// The timeout of a hook that sets none (spec 3.1 §7.22): 30 s, or
/// [`PRE_TOOL_HOOK_TIMEOUT`] for a `PreTool` hook.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// The timeout of a `PreTool` hook that sets none (spec 3.1 §7.22): 5 s, because it runs
/// inside the policy decision of every matching call.
pub const PRE_TOOL_HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// How many hooks of one event run (spec 3.1 §7.22 `max_per_event`).
pub const DEFAULT_MAX_HOOKS_PER_EVENT: u8 = 8;

/// How much time the hooks of one turn may take together (spec 3.1 §7.22
/// `max_time_per_turn`).
pub const DEFAULT_MAX_HOOK_TIME_PER_TURN: Duration = Duration::from_secs(60);

/// The environment variable naming the event a hook runs for (spec 3.1 §7.22).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::hooks::HOOK_EVENT_ENV, "ADK_HOOK_EVENT");
/// ```
pub const HOOK_EVENT_ENV: &str = "ADK_HOOK_EVENT";

/// The longest annotation a hook may add to a result, in bytes; longer text is cut.
pub const HOOK_NOTE_MAX_BYTES: usize = 4096;

/// Error code of a hook configuration the harness refuses (spec 3.1 §7.22).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::hooks::HOOK_CONFIG_CODE, "harness.hooks.config");
/// ```
pub const HOOK_CONFIG_CODE: &str = "harness.hooks.config";

/// The key under which a notice from an `OnHold` or `OnStop` hook travels in
/// `Event::provider_metadata`: text shown to the user, never to the model
/// (spec 3.1 §7.22).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::hooks::HOOK_NOTICE_KEY, "harness.hook.notice");
/// ```
pub const HOOK_NOTICE_KEY: &str = "harness.hook.notice";

/// When a hook runs (spec 3.1 §7.22).
///
/// # Example
///
/// ```rust
/// use adk_harness::hooks::HookEvent;
///
/// assert_eq!(HookEvent::PreTool.as_str(), "pre_tool");
/// assert_eq!(HookEvent::PreTool.default_timeout(), std::time::Duration::from_secs(5));
/// assert_eq!(HookEvent::OnStop.default_timeout(), std::time::Duration::from_secs(30));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Before a tool call is authorized: inside `decide()`, step 4a. May `Deny`.
    PreTool,
    /// After a tool executed. May `Fail` or `Annotate`.
    PostTool,
    /// After `write_file` or `edit_file` changed a file. May `Fail` or `Annotate`; never
    /// reverts.
    PostEdit,
    /// When a call is held for a decision. May `Annotate`, for the user.
    OnHold,
    /// When the harness turn ends. May `Annotate`, for the user.
    OnStop,
    /// Once per session, when its first run starts. May `Annotate` the `<environment>` block.
    OnSessionStart,
}

impl HookEvent {
    /// Every event.
    pub const ALL: [HookEvent; 6] = [
        Self::PreTool,
        Self::PostTool,
        Self::PostEdit,
        Self::OnHold,
        Self::OnStop,
        Self::OnSessionStart,
    ];

    /// The event's name, as `ADK_HOOK_EVENT` and the audit record carry it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreTool => "pre_tool",
            Self::PostTool => "post_tool",
            Self::PostEdit => "post_edit",
            Self::OnHold => "on_hold",
            Self::OnStop => "on_stop",
            Self::OnSessionStart => "on_session_start",
        }
    }

    /// The timeout of a hook on this event that sets none.
    pub fn default_timeout(self) -> Duration {
        match self {
            Self::PreTool => PRE_TOOL_HOOK_TIMEOUT,
            Self::PostTool
            | Self::PostEdit
            | Self::OnHold
            | Self::OnStop
            | Self::OnSessionStart => DEFAULT_HOOK_TIMEOUT,
        }
    }

    /// Whether `outcome` is one this event may produce (spec 3.1 §7.22 "Allowed
    /// outcomes"); `Continue` always is.
    pub fn allows(self, outcome: &HookOutcome) -> bool {
        matches!(
            (self, outcome),
            (_, HookOutcome::Continue)
                | (Self::PreTool, HookOutcome::Deny { .. })
                | (
                    Self::PostTool | Self::PostEdit,
                    HookOutcome::Fail { .. } | HookOutcome::Annotate { .. }
                )
                | (
                    Self::OnHold | Self::OnStop | Self::OnSessionStart,
                    HookOutcome::Annotate { .. }
                )
        )
    }

    /// `outcome`, or `Continue` when this event does not allow it.
    pub fn narrow(self, outcome: HookOutcome) -> HookOutcome {
        if self.allows(&outcome) { outcome } else { HookOutcome::Continue }
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a hook runs (spec 3.1 §7.22).
///
/// # Example
///
/// ```rust
/// use adk_harness::hooks::HookIsolation;
///
/// assert_eq!(HookIsolation::default(), HookIsolation::Inherit);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookIsolation {
    /// As the shell: inside the OS sandbox when `bash` is sandboxed, on the host otherwise.
    #[default]
    Inherit,
    /// On the host even when the shell is sandboxed. A workspace hook needs the workspace's
    /// trust; in Auto only an operator hook may ask for it.
    Host,
}

impl HookIsolation {
    /// The isolation's name, as the audit record carries it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Host => "host",
        }
    }
}

/// Who configured a hook (spec 3.1 §7.22).
///
/// # Example
///
/// ```rust
/// use adk_harness::hooks::HookSource;
///
/// assert_eq!(HookSource::Workspace.as_str(), "workspace");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookSource {
    /// `[[hooks]]` in the workspace's `.adk/harness.toml`; runs only under workspace trust.
    Workspace,
    /// `[[hooks]]` in the user's configuration file.
    User,
    /// `HarnessAgentBuilder::hooks`.
    Operator,
}

impl HookSource {
    /// The source's name, as the audit record and the `<hook-note>` block carry it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::User => "user",
            Self::Operator => "operator",
        }
    }
}

/// One hook (spec 3.1 §7.22).
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
///
/// use adk_harness::hooks::{HookEvent, HookSpec};
///
/// let hook = HookSpec::new(HookEvent::PreTool, "./scripts/check-call.sh").matching("bash");
/// assert!(hook.matches("bash"));
/// assert!(!hook.matches("write_file"));
/// assert_eq!(hook.timeout, Duration::from_secs(5));
/// assert!(!hook.fail_closed);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSpec {
    /// When the hook runs.
    pub event: HookEvent,
    /// A glob on the tool name (`bash`, `write_*`, `mcp:*`); `None` runs on every tool of
    /// the event. `*` matches any run of characters and `?` one character.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// The command, run as `sh -c` through the launcher.
    pub command: String,
    /// How long the hook may run: 30 s, 5 s for `PreTool`, never more than the workspace's
    /// bash timeout.
    pub timeout: Duration,
    /// Where it runs.
    #[serde(default)]
    pub isolation: HookIsolation,
    /// Who configured it.
    pub source: HookSource,
    /// `false`: a crashed or timed-out `PreTool` hook is `Continue`; `true`: it is `Deny`.
    #[serde(default)]
    pub fail_closed: bool,
}

impl HookSpec {
    /// An operator hook on `event` running `command` with the event's default timeout,
    /// inheriting the shell's isolation, matching every tool.
    pub fn new(event: HookEvent, command: impl Into<String>) -> Self {
        Self {
            event,
            matcher: None,
            command: command.into(),
            timeout: event.default_timeout(),
            isolation: HookIsolation::Inherit,
            source: HookSource::Operator,
            fail_closed: false,
        }
    }

    /// Runs only for tools whose name matches `glob`.
    #[must_use]
    pub fn matching(mut self, glob: impl Into<String>) -> Self {
        self.matcher = Some(glob.into());
        self
    }

    /// Sets the timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets where the hook runs.
    #[must_use]
    pub fn isolation(mut self, isolation: HookIsolation) -> Self {
        self.isolation = isolation;
        self
    }

    /// Sets who configured the hook.
    #[must_use]
    pub fn source(mut self, source: HookSource) -> Self {
        self.source = source;
        self
    }

    /// Makes a crashed or timed-out `PreTool` hook a `Deny`.
    #[must_use]
    pub fn fail_closed(mut self, yes: bool) -> Self {
        self.fail_closed = yes;
        self
    }

    /// Whether the hook runs for a call to `tool_name`.
    pub fn matches(&self, tool_name: &str) -> bool {
        self.matcher.as_deref().is_none_or(|glob| glob_matches(glob, tool_name))
    }

    /// `sha256:` and the hex SHA-256 of the command, as the audit record carries it.
    pub fn command_digest(&self) -> String {
        format!("sha256:{}", crate::fsutil::hex_digest(self.command.as_bytes()))
    }

    /// One line naming the hook for a trust prompt: the event, the matcher and the command.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::hooks::{HookEvent, HookSpec};
    ///
    /// let hook = HookSpec::new(HookEvent::PostEdit, "cargo fmt").matching("write_*");
    /// assert_eq!(hook.describe(), "post_edit [write_*]: cargo fmt");
    /// ```
    pub fn describe(&self) -> String {
        match &self.matcher {
            Some(matcher) => format!("{} [{matcher}]: {}", self.event, self.command),
            None => format!("{}: {}", self.event, self.command),
        }
    }
}

/// Whether `glob` matches `name`: `*` matches any run of characters, `?` one character,
/// anything else itself.
///
/// # Example
///
/// ```rust
/// use adk_harness::hooks::glob_matches;
///
/// assert!(glob_matches("write_*", "write_file"));
/// assert!(glob_matches("mcp:*", "mcp:github"));
/// assert!(glob_matches("*", "anything"));
/// assert!(!glob_matches("bash", "bash_unsafe"));
/// ```
pub fn glob_matches(glob: &str, name: &str) -> bool {
    let pattern: Vec<char> = glob.chars().collect();
    let text: Vec<char> = name.chars().collect();
    // Iterative wildcard matching with one backtrack point, linear in the inputs.
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some(c) if *c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// The hooks a harness runs, with the bounds of one turn (spec 3.1 §7.22).
///
/// # Example
///
/// ```rust
/// use adk_harness::hooks::{HookEvent, HookSet, HookSource};
///
/// let set = HookSet::from_toml(
///     r#"
///     [[hooks]]
///     event = "pre_tool"
///     matcher = "bash"
///     command = "./deny-rm.sh"
///     fail_closed = true
///
///     [[hooks]]
///     event = "post_edit"
///     command = "cargo fmt"
///     timeout_secs = 90
///     "#,
///     HookSource::Workspace,
/// )
/// .unwrap();
/// assert_eq!(set.hooks.len(), 2);
/// assert_eq!(set.for_event(HookEvent::PreTool).count(), 1);
/// assert_eq!(set.hooks[1].timeout, std::time::Duration::from_secs(90));
/// assert_eq!(set.max_per_event, 8);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSet {
    /// The hooks, in the order they run within an event.
    pub hooks: Vec<HookSpec>,
    /// How many hooks of one event may run; the rest are refused at build.
    pub max_per_event: u8,
    /// How much time all hooks of one turn may take together; a turn over it ends with a
    /// reason.
    pub max_time_per_turn: Duration,
}

impl Default for HookSet {
    fn default() -> Self {
        Self {
            hooks: Vec::new(),
            max_per_event: DEFAULT_MAX_HOOKS_PER_EVENT,
            max_time_per_turn: DEFAULT_MAX_HOOK_TIME_PER_TURN,
        }
    }
}

/// A `[[hooks]]` table as `harness.toml` writes it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookTable {
    event: HookEvent,
    #[serde(default)]
    matcher: Option<String>,
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    isolation: Option<HookIsolation>,
    #[serde(default)]
    fail_closed: Option<bool>,
}

/// The tables of `harness.toml` the hook loader reads; everything else belongs to the CLI.
#[derive(Debug, Default, Deserialize)]
struct HooksFile {
    #[serde(default)]
    hooks: Vec<toml::Value>,
}

fn config_error(message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Agent, ErrorCategory::InvalidInput, HOOK_CONFIG_CODE, message)
}

impl HookSet {
    /// An empty set with the default bounds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `hook`.
    #[must_use]
    pub fn with(mut self, hook: HookSpec) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Whether the set has no hooks.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// The hooks of `event`, in order.
    pub fn for_event(&self, event: HookEvent) -> impl Iterator<Item = &HookSpec> {
        self.hooks.iter().filter(move |hook| hook.event == event)
    }

    /// The `[[hooks]]` tables of `text`, a `harness.toml`, as hooks from `source`; other
    /// tables are ignored. A table with an unknown field, an unknown event or an empty
    /// command is refused.
    ///
    /// # Errors
    ///
    /// Returns `harness.hooks.config` when the text is not TOML or a table is invalid.
    pub fn from_toml(text: &str, source: HookSource) -> Result<Self> {
        let file: HooksFile = toml::from_str(text)
            .map_err(|error| config_error(format!("harness.toml is not valid TOML: {error}")))?;
        let mut set = Self::default();
        for (index, table) in file.hooks.into_iter().enumerate() {
            let table: HookTable = table.try_into().map_err(|error| {
                config_error(format!(
                    "[[hooks]] entry {} is not a valid hook: {error}; a hook has event, \
                     command and optionally matcher, timeout_secs, isolation and fail_closed",
                    index + 1
                ))
            })?;
            if table.command.trim().is_empty() {
                return Err(config_error(format!(
                    "[[hooks]] entry {} has an empty command",
                    index + 1
                )));
            }
            let mut hook = HookSpec::new(table.event, table.command).source(source);
            hook.matcher = table.matcher;
            if let Some(secs) = table.timeout_secs {
                hook.timeout = Duration::from_secs(secs);
            }
            if let Some(isolation) = table.isolation {
                hook.isolation = isolation;
            }
            if let Some(fail_closed) = table.fail_closed {
                hook.fail_closed = fail_closed;
            }
            set.hooks.push(hook);
        }
        Ok(set)
    }

    /// The hooks of the file at `path` as hooks from `source`, or `None` when there is no
    /// file.
    ///
    /// # Errors
    ///
    /// Returns `harness.hooks.config` when the file cannot be read or parsed.
    pub fn load_file(path: &Path, source: HookSource) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(config_error(format!("read {}: {error}", path.display())));
            }
        };
        Self::from_toml(&text, source)
            .map(Some)
            .map_err(|error| config_error(format!("{}: {}", path.display(), error.message)))
    }

    /// The workspace hooks of `root`, from [`WORKSPACE_HOOKS_FILE`], or `None` when the
    /// workspace has none.
    ///
    /// # Errors
    ///
    /// Returns `harness.hooks.config` when the file cannot be read or parsed.
    pub fn load_workspace(root: &Path) -> Result<Option<Self>> {
        Self::load_file(&root.join(WORKSPACE_HOOKS_FILE), HookSource::Workspace)
    }

    /// This set followed by `other`'s hooks; the bounds stay this set's. The harness merges
    /// operator, user and workspace hooks in that order (spec 3.1 §7.22).
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.hooks.extend(other.hooks);
        self
    }

    /// Refuses more than `max_per_event` hooks on one event and an empty command.
    ///
    /// # Errors
    ///
    /// Returns `harness.hooks.config` naming the event or the hook.
    pub fn validate(&self) -> Result<()> {
        for event in HookEvent::ALL {
            let count = self.for_event(event).count();
            if count > usize::from(self.max_per_event) {
                return Err(config_error(format!(
                    "{count} hooks on {event} exceed max_per_event {}; remove some or raise \
                     the bound",
                    self.max_per_event
                )));
            }
        }
        if let Some(hook) = self.hooks.iter().find(|hook| hook.command.trim().is_empty()) {
            return Err(config_error(format!("the {} hook has an empty command", hook.event)));
        }
        Ok(())
    }

    /// The hooks from `source`, each described for a trust prompt (spec 3.1 §7.22: the
    /// prompt lists each workspace hook with its command).
    pub fn describe(&self, source: HookSource) -> Vec<String> {
        self.hooks.iter().filter(|hook| hook.source == source).map(HookSpec::describe).collect()
    }
}

/// What a hook's standard output says (spec 3.1 §7.22), parsed from JSON.
///
/// Anything that is not one of the three decisions — including `{"decision":"allow"}`,
/// text, or nothing — is `Continue`.
///
/// # Example
///
/// ```rust
/// use adk_harness::hooks::HookOutcome;
///
/// let parsed = HookOutcome::parse(br#"{"decision":"deny","reason":"no writes under migrations/"}"#);
/// assert_eq!(parsed.outcome, HookOutcome::Deny { reason: "no writes under migrations/".into() });
/// let grant = HookOutcome::parse(br#"{"decision":"allow"}"#);
/// assert_eq!(grant.outcome, HookOutcome::Continue);
/// assert!(grant.ignored_grant);
/// assert_eq!(HookOutcome::parse(b"formatted 3 files").outcome, HookOutcome::Continue);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum HookOutcome {
    /// Nothing changes.
    Continue,
    /// A `PreTool` hook denies the call.
    Deny {
        /// Why, shown to the model and recorded.
        reason: String,
    },
    /// A `PostTool` or `PostEdit` hook replaces the result with `{"error": message}`.
    Fail {
        /// The error the model sees.
        message: String,
    },
    /// The hook adds `text` to the result, delimited as untrusted.
    Annotate {
        /// The note.
        text: String,
    },
}

/// A parsed outcome and whether the hook tried to grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedOutcome {
    /// The outcome; `Continue` for anything that is not a decision.
    pub outcome: HookOutcome,
    /// The output carried an `allow`-like decision, which is ignored and recorded as
    /// `ignored_grant` (spec 3.1 §9.3).
    pub ignored_grant: bool,
}

/// The `decision`-like keys a grant may hide under.
const DECISION_KEYS: &[&str] = &["decision", "permissionDecision", "permission_decision"];
/// Decision words that mean a grant.
const GRANT_WORDS: &[&str] = &["allow", "approve", "approved", "permit", "accept", "yes"];

impl HookOutcome {
    /// The outcome in `stdout`.
    pub fn parse(stdout: &[u8]) -> ParsedOutcome {
        let text = String::from_utf8_lossy(stdout);
        let Ok(value) = serde_json::from_str::<Value>(text.trim()) else {
            return ParsedOutcome { outcome: Self::Continue, ignored_grant: false };
        };
        let Some(object) = value.as_object() else {
            return ParsedOutcome { outcome: Self::Continue, ignored_grant: false };
        };
        let decision = DECISION_KEYS
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
            .map(|decision| decision.trim().to_ascii_lowercase());
        let text_field = |keys: &[&str], fallback: &str| {
            keys.iter()
                .find_map(|key| object.get(*key).and_then(Value::as_str))
                .map_or_else(|| fallback.to_string(), |text| text.trim().to_string())
        };
        let outcome = match decision.as_deref() {
            Some("deny") => Self::Deny {
                reason: text_field(&["reason", "message", "text"], "denied by a hook"),
            },
            Some("fail") => Self::Fail {
                message: text_field(&["message", "reason", "text"], "failed by a hook"),
            },
            Some("annotate") => {
                let text = text_field(&["text", "message", "reason"], "");
                if text.is_empty() { Self::Continue } else { Self::Annotate { text } }
            }
            _ => Self::Continue,
        };
        let ignored_grant = decision.as_deref().is_some_and(|word| GRANT_WORDS.contains(&word));
        ParsedOutcome { outcome, ignored_grant }
    }

    /// The outcome's name, as the audit record carries it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Deny { .. } => "deny",
            Self::Fail { .. } => "fail",
            Self::Annotate { .. } => "annotate",
        }
    }
}

/// What the `PreTool` hooks of a call decided (spec 3.1 §7.22, §9.2 I53).
///
/// There is no `Allow` variant, so a hook cannot loosen a decision: [`apply`](Self::apply)
/// returns the policy's decision unchanged or the stricter of it and a `Deny`.
///
/// # Example
///
/// ```rust
/// use adk_core::PolicyDecision;
/// use adk_harness::hooks::PreToolVerdict;
///
/// let held = PolicyDecision::require_approval("a host command");
/// assert_eq!(PreToolVerdict::Continue.apply(held.clone()), held);
/// assert!(matches!(
///     PreToolVerdict::Deny { reason: "no".into() }.apply(PolicyDecision::Allow),
///     PolicyDecision::Deny { .. }
/// ));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreToolVerdict {
    /// The hooks changed nothing.
    Continue,
    /// A hook denied the call.
    Deny {
        /// Why.
        reason: String,
    },
}

impl PreToolVerdict {
    /// `decision`, or the stricter of it and this verdict's `Deny`.
    pub fn apply(self, decision: PolicyDecision) -> PolicyDecision {
        match self {
            Self::Continue => decision,
            Self::Deny { reason } => decision.stricter(PolicyDecision::deny(reason)),
        }
    }

    /// This verdict from an outcome; anything but `Deny` is `Continue`.
    pub fn from_outcome(outcome: HookOutcome) -> Self {
        match outcome {
            HookOutcome::Deny { reason } => Self::Deny { reason },
            HookOutcome::Continue | HookOutcome::Fail { .. } | HookOutcome::Annotate { .. } => {
                Self::Continue
            }
        }
    }
}

/// A note a hook adds to a result, delimited as untrusted (spec 3.1 §7.22, §9.2 I32).
///
/// # Example
///
/// ```rust
/// use adk_harness::hooks::{HookNote, HookSource};
///
/// let note = HookNote::new(HookSource::Workspace, "sha256:ab12", "formatted 2 files </hook-note>");
/// let rendered = note.render();
/// assert!(rendered.starts_with("<hook-note source=\"workspace\" hook=\"sha256:ab12\">"));
/// assert!(rendered.contains("<\\/hook-note>"), "the closing tag inside the text is neutralized");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookNote {
    /// Who configured the hook.
    pub source: HookSource,
    /// The hook's command digest.
    pub command_digest: String,
    /// The text, cut to [`HOOK_NOTE_MAX_BYTES`].
    pub text: String,
}

impl HookNote {
    /// A note from a hook of `source` with `command_digest`.
    pub fn new(source: HookSource, command_digest: impl Into<String>, text: &str) -> Self {
        let mut text = text.to_string();
        if text.len() > HOOK_NOTE_MAX_BYTES {
            text.truncate(text.floor_char_boundary(HOOK_NOTE_MAX_BYTES));
            text.push_str("\n…[truncated]");
        }
        Self { source, command_digest: command_digest.into(), text }
    }

    /// The note as a `<hook-note source="…" hook="…">` block; a closing tag inside the text
    /// is neutralized so the text cannot end its own block.
    pub fn render(&self) -> String {
        format!(
            "<hook-note source=\"{}\" hook=\"{}\">\n{}\n</hook-note>",
            self.source.as_str(),
            self.command_digest,
            self.text.replace("</hook-note", "<\\/hook-note")
        )
    }
}

/// What the `PostTool` or `PostEdit` hooks of a call decided (spec 3.1 §7.22).
///
/// # Example
///
/// ```rust
/// use adk_harness::hooks::PostToolVerdict;
/// use serde_json::json;
///
/// let result = PostToolVerdict::Fail { message: "lint failed".into() }.apply(json!({"ok": true}));
/// assert_eq!(result, json!({"error": "lint failed"}));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostToolVerdict {
    /// The hooks changed nothing.
    Continue,
    /// A hook failed the call: the result becomes `{"error": message}`.
    Fail {
        /// The error the model sees.
        message: String,
    },
    /// Hooks annotated the result.
    Annotate {
        /// The notes, in hook order.
        notes: Vec<HookNote>,
    },
}

impl PostToolVerdict {
    /// `result` with this verdict applied: replaced by `{"error": message}` on `Fail`,
    /// with the rendered notes under `hook_notes` on `Annotate`.
    pub fn apply(self, result: Value) -> Value {
        match self {
            Self::Continue => result,
            Self::Fail { message } => serde_json::json!({ "error": message }),
            Self::Annotate { notes } => {
                let rendered: Vec<Value> =
                    notes.iter().map(|note| Value::String(note.render())).collect();
                match result {
                    // Notes from earlier verdicts on the same call — another file of a
                    // patch — are kept.
                    Value::Object(mut object) => {
                        match object.get_mut("hook_notes") {
                            Some(Value::Array(existing)) => existing.extend(rendered),
                            _ => {
                                object.insert("hook_notes".to_string(), Value::Array(rendered));
                            }
                        }
                        Value::Object(object)
                    }
                    other => serde_json::json!({ "result": other, "hook_notes": rendered }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_tool_names() {
        for (glob, name, expected) in [
            ("bash", "bash", true),
            ("bash", "bash2", false),
            ("write_*", "write_file", true),
            ("write_*", "write", false),
            ("write*", "write", true),
            ("write_*", "edit_file", false),
            ("*_file", "edit_file", true),
            ("mcp:*", "mcp:github", true),
            ("mcp:*", "bash", false),
            ("*", "", true),
            ("?ash", "bash", true),
            ("?ash", "ash", false),
            ("a*b*c", "aXXbYYc", true),
            ("a*b*c", "aXXbYY", false),
            ("", "", true),
            ("", "x", false),
        ] {
            assert_eq!(glob_matches(glob, name), expected, "{glob} vs {name}");
        }
    }

    #[test]
    fn outcomes_parse_and_grants_are_ignored() {
        let deny = HookOutcome::parse(br#"{"decision": "DENY", "reason": "nope"}"#);
        assert_eq!(deny.outcome, HookOutcome::Deny { reason: "nope".into() });
        assert!(!deny.ignored_grant);
        let fail = HookOutcome::parse(br#"{"decision":"fail"}"#);
        assert_eq!(fail.outcome, HookOutcome::Fail { message: "failed by a hook".into() });
        let note = HookOutcome::parse(br#"{"decision":"annotate","text":"hi"}"#);
        assert_eq!(note.outcome, HookOutcome::Annotate { text: "hi".into() });
        let empty_note = HookOutcome::parse(br#"{"decision":"annotate"}"#);
        assert_eq!(empty_note.outcome, HookOutcome::Continue);
        for grant in [
            r#"{"decision":"allow"}"#,
            r#"{"permissionDecision":"allow","reason":"trusted"}"#,
            r#"{"decision":"approve"}"#,
        ] {
            let parsed = HookOutcome::parse(grant.as_bytes());
            assert_eq!(parsed.outcome, HookOutcome::Continue, "{grant}");
            assert!(parsed.ignored_grant, "{grant}");
        }
        for other in ["", "ok", "[1,2]", r#"{"decision":"continue"}"#, r#"{"mode":"auto"}"#] {
            let parsed = HookOutcome::parse(other.as_bytes());
            assert_eq!(
                parsed,
                ParsedOutcome { outcome: HookOutcome::Continue, ignored_grant: false }
            );
        }
    }

    #[test]
    fn events_narrow_outcomes_to_what_they_allow() {
        let deny = HookOutcome::Deny { reason: "x".into() };
        let fail = HookOutcome::Fail { message: "x".into() };
        let note = HookOutcome::Annotate { text: "x".into() };
        assert_eq!(HookEvent::PreTool.narrow(deny.clone()), deny);
        assert_eq!(HookEvent::PreTool.narrow(fail.clone()), HookOutcome::Continue);
        assert_eq!(HookEvent::PreTool.narrow(note.clone()), HookOutcome::Continue);
        assert_eq!(HookEvent::PostTool.narrow(deny.clone()), HookOutcome::Continue);
        assert_eq!(HookEvent::PostTool.narrow(fail.clone()), fail);
        assert_eq!(HookEvent::PostEdit.narrow(note.clone()), note);
        assert_eq!(HookEvent::OnHold.narrow(fail), HookOutcome::Continue);
        assert_eq!(HookEvent::OnStop.narrow(note.clone()), note);
        assert_eq!(HookEvent::OnSessionStart.narrow(deny), HookOutcome::Continue);
    }

    #[test]
    fn toml_hooks_load_with_defaults_and_refuse_unknown_fields() {
        let set = HookSet::from_toml(
            r#"
            mode = "ask"
            [[hooks]]
            event = "pre_tool"
            command = "echo"
            [[hooks]]
            event = "on_stop"
            command = "notify"
            isolation = "host"
            "#,
            HookSource::User,
        )
        .unwrap();
        assert_eq!(set.hooks[0].timeout, PRE_TOOL_HOOK_TIMEOUT);
        assert_eq!(set.hooks[1].timeout, DEFAULT_HOOK_TIMEOUT);
        assert_eq!(set.hooks[1].isolation, HookIsolation::Host);
        assert!(set.hooks.iter().all(|hook| hook.source == HookSource::User));

        let unknown = HookSet::from_toml(
            "[[hooks]]\nevent = \"pre_tool\"\ncommand = \"x\"\nallow = true\n",
            HookSource::Workspace,
        )
        .unwrap_err();
        assert_eq!(unknown.code, HOOK_CONFIG_CODE);
        let bad_event = HookSet::from_toml(
            "[[hooks]]\nevent = \"allow_tool\"\ncommand = \"x\"\n",
            HookSource::Workspace,
        );
        assert!(bad_event.is_err());
        let empty = HookSet::from_toml(
            "[[hooks]]\nevent = \"pre_tool\"\ncommand = \" \"\n",
            HookSource::Workspace,
        );
        assert!(empty.is_err());
        assert!(HookSet::from_toml("mode = \"plan\"", HookSource::Workspace).unwrap().is_empty());
    }

    #[test]
    fn merge_keeps_order_and_validate_bounds_each_event() {
        let operator = HookSet::new().with(HookSpec::new(HookEvent::PreTool, "a"));
        let user =
            HookSet::new().with(HookSpec::new(HookEvent::PreTool, "b").source(HookSource::User));
        let merged = operator.merge(user);
        let commands: Vec<&str> = merged.hooks.iter().map(|hook| hook.command.as_str()).collect();
        assert_eq!(commands, ["a", "b"]);
        assert!(merged.validate().is_ok());
        let mut crowded = HookSet::new();
        for i in 0..9 {
            crowded = crowded.with(HookSpec::new(HookEvent::PostTool, format!("hook {i}")));
        }
        assert_eq!(crowded.validate().unwrap_err().code, HOOK_CONFIG_CODE);
    }

    #[test]
    fn notes_are_capped_and_delimited() {
        let long = "x".repeat(HOOK_NOTE_MAX_BYTES + 10);
        let note = HookNote::new(HookSource::Operator, "sha256:1", &long);
        assert!(note.text.len() < HOOK_NOTE_MAX_BYTES + 20);
        assert!(note.text.ends_with("[truncated]"));
        let annotated =
            PostToolVerdict::Annotate { notes: vec![note] }.apply(Value::String("out".into()));
        assert!(
            annotated["hook_notes"][0]
                .as_str()
                .unwrap()
                .starts_with("<hook-note source=\"operator\"")
        );
        assert_eq!(annotated["result"], "out");
    }
}
