//! Long-term fact types (spec 3.1 §7.15.2).

use std::sync::Arc;
use std::time::Duration;

use adk_core::Llm;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::fsutil::sha256_hex;

/// Most characters of one fact's text (spec 3.1 §7.15.2, §9.2 I43).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::FACT_TEXT_MAX_CHARS, 500);
/// ```
pub const FACT_TEXT_MAX_CHARS: usize = 500;

/// Prefix of the `author` of a fact's `MemoryEntry` in a [`MemoryServiceIndex`]
/// (spec 3.1 §7.15.2); the fact id follows it.
///
/// [`MemoryServiceIndex`]: crate::MemoryServiceIndex
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::FACT_AUTHOR_PREFIX, "adk-harness:fact:");
/// ```
pub const FACT_AUTHOR_PREFIX: &str = "adk-harness:fact:";

/// Where a fact applies (spec 3.1 §7.15.2): one user of one app, and one project or every
/// project of the user.
///
/// `project` is [`project_slug`](crate::project_slug) of the workspace, or a spec 3.2
/// partition's `memory_scope`. `None` is user-wide: a preference that holds in every
/// project. Facts never cross apps or users (spec 3.1 §9.2 I42).
///
/// # Example
///
/// ```rust
/// use adk_harness::FactScope;
///
/// let scope = FactScope::new("adk-rust", "alice", Some("repo-0123456789ab".into()));
/// assert_eq!(scope.user_wide().project, None);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactScope {
    /// The application.
    pub app: String,
    /// The user the fact is about.
    pub user: String,
    /// The project, or `None` for a user-wide fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl FactScope {
    /// A scope of `user` of `app` in `project`.
    pub fn new(app: impl Into<String>, user: impl Into<String>, project: Option<String>) -> Self {
        Self { app: app.into(), user: user.into(), project }
    }

    /// The user-wide scope of the same user.
    pub fn user_wide(&self) -> Self {
        Self { app: self.app.clone(), user: self.user.clone(), project: None }
    }

    /// Whether a fact in `other` may be recalled in this scope: the same app and user, and
    /// either this project or user-wide (spec 3.1 §9.2 I42).
    pub fn admits(&self, other: &FactScope) -> bool {
        self.app == other.app
            && self.user == other.user
            && (other.project.is_none() || other.project == self.project)
    }
}

/// What a fact is about (spec 3.1 §7.15.2).
///
/// # Example
///
/// ```rust
/// use adk_harness::FactKind;
///
/// assert_eq!(FactKind::ProjectFact.as_str(), "project_fact");
/// assert_eq!("procedure".parse::<FactKind>().unwrap(), FactKind::Procedure);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    /// How the user likes things done. User-wide unless tied to a project.
    Preference,
    /// How the project is built, tested or laid out.
    ProjectFact,
    /// A way of doing something that worked.
    Procedure,
    /// What happened when something was tried.
    Outcome,
    /// A decision and why it was taken.
    Decision,
    /// A correction of something believed earlier.
    Correction,
}

impl FactKind {
    /// Every kind.
    pub const ALL: [FactKind; 6] = [
        FactKind::Preference,
        FactKind::ProjectFact,
        FactKind::Procedure,
        FactKind::Outcome,
        FactKind::Decision,
        FactKind::Correction,
    ];

    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::ProjectFact => "project_fact",
            Self::Procedure => "procedure",
            Self::Outcome => "outcome",
            Self::Decision => "decision",
            Self::Correction => "correction",
        }
    }
}

impl std::fmt::Display for FactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for FactKind {
    type Err = adk_core::AdkError;

    fn from_str(value: &str) -> adk_core::Result<Self> {
        let wanted = value.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        Self::ALL.into_iter().find(|kind| kind.as_str() == wanted).ok_or_else(|| {
            adk_core::AdkError::new(
                adk_core::ErrorComponent::Tool,
                adk_core::ErrorCategory::InvalidInput,
                "harness.memory.invalid_kind",
                format!(
                    "'{value}' is not a fact kind; use preference, project_fact, procedure, \
                     outcome, decision or correction"
                ),
            )
        })
    }
}

/// Whether a fact is believed, and on what grounds (spec 3.1 §7.15.2).
///
/// Only `Known` and `Assumed` facts are live: recalled, searched and counted. History is
/// kept: a superseded or forgotten fact stays in the store with `valid_to` set.
///
/// # Example
///
/// ```rust
/// use adk_harness::FactStatus;
///
/// assert!(FactStatus::Assumed.is_live());
/// assert!(!FactStatus::Superseded { by: "01J".into() }.is_live());
/// assert_eq!(FactStatus::Forgotten.label(), "forgotten");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    /// Stated by the user in a verified quote, or confirmed by a person (spec 3.1 §9.2 I41).
    Known,
    /// Inferred by the agent or observed in a tool's output; a hint, not an instruction.
    Assumed,
    /// Replaced by the fact `by`.
    Superseded {
        /// The id of the fact that replaced this one.
        by: String,
    },
    /// Forgotten at a person's request, or evicted when the scope was full.
    Forgotten,
}

impl FactStatus {
    /// Whether the fact is `Known` or `Assumed`.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Known | Self::Assumed)
    }

    /// `known`, `assumed`, `superseded` or `forgotten`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Assumed => "assumed",
            Self::Superseded { .. } => "superseded",
            Self::Forgotten => "forgotten",
        }
    }
}

/// Where a fact came from (spec 3.1 §7.15.2).
///
/// # Example
///
/// ```rust
/// use adk_harness::FactSource;
///
/// assert_eq!(serde_json::to_value(FactSource::UserStated).unwrap(), "user_stated");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactSource {
    /// The user said it; the quote was found in one of the user's own messages.
    UserStated,
    /// A person promoted or corrected it.
    UserConfirmed,
    /// The agent inferred it.
    AgentInferred,
    /// The agent read it in a tool's output.
    ToolObserved,
    /// It came from an import.
    Imported,
}

impl FactSource {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserStated => "user_stated",
            Self::UserConfirmed => "user_confirmed",
            Self::AgentInferred => "agent_inferred",
            Self::ToolObserved => "tool_observed",
            Self::Imported => "imported",
        }
    }
}

/// How a fact came to be stored (spec 3.1 §7.15.2).
///
/// # Example
///
/// ```rust
/// use adk_harness::{FactSource, Provenance};
///
/// let provenance = Provenance::new(FactSource::AgentInferred, "s1", "inv-1");
/// assert!(provenance.quote.is_none());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    /// Where the fact came from.
    pub source: FactSource,
    /// The session it was learned in.
    pub session_id: String,
    /// The invocation it was learned in.
    pub invocation_id: String,
    /// The durable run it was learned in, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The user's own words, redacted, when the fact is a verified quote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    /// When it was learned.
    pub at: DateTime<Utc>,
    /// The facts a consolidation tick merged into this one (spec 3.2 §7.5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_ids: Vec<String>,
    /// The consolidation tick that wrote it, by distilling a session the per-session
    /// distiller never reached or by merging facts (spec 3.2 §7.5, §10 HK2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<String>,
}

impl Provenance {
    /// A provenance of `source` in `session_id` and `invocation_id`, now.
    pub fn new(
        source: FactSource,
        session_id: impl Into<String>,
        invocation_id: impl Into<String>,
    ) -> Self {
        Self {
            source,
            session_id: session_id.into(),
            invocation_id: invocation_id.into(),
            run_id: None,
            quote: None,
            at: Utc::now(),
            source_ids: Vec::new(),
            tick: None,
        }
    }
}

/// One long-term fact with its provenance (spec 3.1 §7.15.2).
///
/// # Example
///
/// ```rust
/// use adk_harness::{FactKind, FactScope, FactSource, FactStatus, MemoryFact, Provenance};
///
/// let scope = FactScope::new("app", "alice", Some("repo-0123456789ab".into()));
/// let fact = MemoryFact::new(
///     scope,
///     FactKind::ProjectFact,
///     "The tests need `--features sqlite`.",
///     FactStatus::Assumed,
///     Provenance::new(FactSource::AgentInferred, "s1", "inv-1"),
/// );
/// assert_eq!(fact.id.len(), 26);
/// assert_eq!(fact.digest, adk_harness::fact_digest("the tests need `--features sqlite`"));
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFact {
    /// A ULID.
    pub id: String,
    /// Where the fact applies.
    pub scope: FactScope,
    /// What it is about.
    pub kind: FactKind,
    /// One fact, at most [`FACT_TEXT_MAX_CHARS`] characters, redacted before storage.
    pub text: String,
    /// Whether it is believed.
    pub status: FactStatus,
    /// How it was learned.
    pub provenance: Provenance,
    /// The agent's confidence, for inferred facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// When it started to hold.
    pub valid_from: DateTime<Utc>,
    /// When it was superseded or forgotten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<DateTime<Utc>>,
    /// [`fact_digest`] of the text: the deduplication key within a scope.
    pub digest: String,
    /// How often it was stored again or recalled.
    #[serde(default)]
    pub uses: u32,
    /// When it was last recalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recalled: Option<DateTime<Utc>>,
}

impl MemoryFact {
    /// A new fact with a fresh id, valid from the provenance's time.
    pub fn new(
        scope: FactScope,
        kind: FactKind,
        text: impl Into<String>,
        status: FactStatus,
        provenance: Provenance,
    ) -> Self {
        let text = text.into();
        Self {
            id: new_fact_id(),
            scope,
            kind,
            digest: fact_digest(&text),
            text,
            status,
            valid_from: provenance.at,
            provenance,
            confidence: None,
            valid_to: None,
            uses: 0,
            last_recalled: None,
        }
    }

    /// This fact with `confidence`.
    #[must_use]
    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = Some(confidence.clamp(0.0, 1.0));
        self
    }
}

/// Which projects a [`FactFilter`] matches.
///
/// # Example
///
/// ```rust
/// let projects = adk_harness::FactProjects::AndUserWide("repo-0123456789ab".into());
/// # let _ = projects;
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FactProjects {
    /// Facts of exactly this project, or the user-wide facts for `None`.
    Only(Option<String>),
    /// Facts of this project and the user-wide facts: what recall sees.
    AndUserWide(String),
    /// Every fact of the user.
    #[default]
    All,
}

impl FactProjects {
    fn matches(&self, project: Option<&str>) -> bool {
        match self {
            Self::Only(wanted) => wanted.as_deref() == project,
            Self::AndUserWide(wanted) => project.is_none_or(|project| project == wanted),
            Self::All => true,
        }
    }
}

/// What [`FactStore::list`](crate::FactStore::list) returns: one user's facts by project,
/// kind, status and age (spec 3.1 §7.15.2).
///
/// # Example
///
/// ```rust
/// use adk_harness::{FactFilter, FactScope};
///
/// let scope = FactScope::new("app", "alice", Some("repo-0123456789ab".into()));
/// let filter = FactFilter::recall(&scope);
/// assert!(filter.live_only);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FactFilter {
    /// The application.
    pub app: String,
    /// The user.
    pub user: String,
    /// Which projects.
    pub projects: FactProjects,
    /// Only these kinds; empty for every kind.
    pub kinds: Vec<FactKind>,
    /// Only `Known` and `Assumed` facts.
    pub live_only: bool,
    /// Only facts valid from this time on.
    pub since: Option<DateTime<Utc>>,
}

impl FactFilter {
    /// Every fact of `user` of `app`, in every project and status.
    pub fn user(app: impl Into<String>, user: impl Into<String>) -> Self {
        Self { app: app.into(), user: user.into(), ..Self::default() }
    }

    /// The live facts recall sees from `scope`: its project's and the user-wide ones.
    pub fn recall(scope: &FactScope) -> Self {
        Self {
            app: scope.app.clone(),
            user: scope.user.clone(),
            projects: match &scope.project {
                Some(project) => FactProjects::AndUserWide(project.clone()),
                None => FactProjects::Only(None),
            },
            live_only: true,
            ..Self::default()
        }
    }

    /// Every fact of exactly `scope`, in every status.
    pub fn exactly(scope: &FactScope) -> Self {
        Self {
            app: scope.app.clone(),
            user: scope.user.clone(),
            projects: FactProjects::Only(scope.project.clone()),
            ..Self::default()
        }
    }

    /// Whether `fact` passes the filter.
    pub fn matches(&self, fact: &MemoryFact) -> bool {
        fact.scope.app == self.app
            && fact.scope.user == self.user
            && self.projects.matches(fact.scope.project.as_deref())
            && (self.kinds.is_empty() || self.kinds.contains(&fact.kind))
            && (!self.live_only || fact.status.is_live())
            && self.since.is_none_or(|since| fact.valid_from >= since)
    }
}

/// Long-term memory configuration (spec 3.1 §7.15.2).
///
/// Off in the library until [`HarnessAgentBuilder::long_term_memory`] is called.
///
/// [`HarnessAgentBuilder::long_term_memory`]: crate::HarnessAgentBuilder::long_term_memory
///
/// # Example
///
/// ```rust
/// use adk_harness::LongTermMemoryConfig;
///
/// let config = LongTermMemoryConfig::default();
/// assert_eq!((config.recall_limit, config.recall_tokens, config.max_facts), (8, 1_500, 2_000));
/// assert_eq!(config.distill_idle, std::time::Duration::from_secs(600));
/// ```
#[derive(Clone)]
pub struct LongTermMemoryConfig {
    /// Most facts recalled into one request.
    pub recall_limit: usize,
    /// Most tokens (about four characters each) of recalled fact lines.
    pub recall_tokens: usize,
    /// Least index score a recalled fact needs.
    pub min_score: f32,
    /// Distil sessions into facts.
    pub distill: bool,
    /// The distiller model; the member model, else the harness model, when `None`.
    pub distill_model: Option<Arc<dyn Llm>>,
    /// Distil a session after it has been idle this long.
    pub distill_idle: Duration,
    /// Distil a session every this many user turns.
    pub distill_every: u32,
    /// Least similarity at which a new fact of the same kind replaces an existing one.
    pub supersede_score: f32,
    /// Most live facts in one scope.
    pub max_facts: usize,
    /// Most characters of one fact's text.
    pub max_fact_chars: usize,
}

impl Default for LongTermMemoryConfig {
    fn default() -> Self {
        Self {
            recall_limit: 8,
            recall_tokens: 1_500,
            min_score: 0.35,
            distill: true,
            distill_model: None,
            distill_idle: Duration::from_secs(600),
            distill_every: 20,
            supersede_score: 0.85,
            max_facts: 2_000,
            max_fact_chars: FACT_TEXT_MAX_CHARS,
        }
    }
}

impl std::fmt::Debug for LongTermMemoryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LongTermMemoryConfig")
            .field("recall_limit", &self.recall_limit)
            .field("recall_tokens", &self.recall_tokens)
            .field("min_score", &self.min_score)
            .field("distill", &self.distill)
            .field("distill_model", &self.distill_model.as_ref().map(|model| model.name()))
            .field("distill_idle", &self.distill_idle)
            .field("distill_every", &self.distill_every)
            .field("supersede_score", &self.supersede_score)
            .field("max_facts", &self.max_facts)
            .field("max_fact_chars", &self.max_fact_chars)
            .finish()
    }
}

/// `text` normalised for deduplication: lowercase, whitespace collapsed, without
/// surrounding whitespace or trailing sentence punctuation.
pub(crate) fn normalise_fact_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    collapsed.trim_end_matches(['.', '!', ';', ',']).trim_end().to_string()
}

/// The SHA-256 of `text` normalised: lowercase, whitespace collapsed, without trailing
/// sentence punctuation. Two facts with the same digest in a scope are one fact
/// (spec 3.1 §7.15.2).
///
/// # Example
///
/// ```rust
/// use adk_harness::fact_digest;
///
/// assert_eq!(fact_digest("Use  pnpm, not npm."), fact_digest("use pnpm, not npm"));
/// ```
pub fn fact_digest(text: &str) -> String {
    sha256_hex(&normalise_fact_text(text))
}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A ULID: 48 bits of milliseconds since the epoch and 80 random bits, in Crockford
/// base32, so ids sort by creation time.
pub(crate) fn new_fact_id() -> String {
    let millis = u128::try_from(Utc::now().timestamp_millis().max(0)).unwrap_or_default();
    let random = u128::from_be_bytes(*uuid::Uuid::new_v4().as_bytes()) & ((1u128 << 80) - 1);
    let value = (millis << 80) | random;
    (0..26)
        .rev()
        .map(|i| CROCKFORD[((value >> (i * 5)) & 0x1f) as usize] as char)
        .collect::<String>()
}

/// About four characters per token, rounded up.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_ulids_that_sort_by_time() {
        let first = new_fact_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = new_fact_id();
        assert_eq!(first.len(), 26);
        assert!(first.chars().all(|c| CROCKFORD.contains(&(c as u8))));
        assert!(first < second, "{first} < {second}");
    }

    #[test]
    fn digests_ignore_case_spacing_and_trailing_punctuation() {
        assert_eq!(fact_digest("Use pnpm, not npm."), fact_digest("  use   PNPM, not npm "));
        assert_ne!(fact_digest("use pnpm"), fact_digest("use npm"));
    }

    #[test]
    fn filters_scope_projects() {
        let fact = |project: Option<&str>| {
            MemoryFact::new(
                FactScope::new("app", "alice", project.map(str::to_string)),
                FactKind::Preference,
                "x",
                FactStatus::Known,
                Provenance::new(FactSource::UserStated, "s", "i"),
            )
        };
        let scope = FactScope::new("app", "alice", Some("p1".into()));
        let recall = FactFilter::recall(&scope);
        assert!(recall.matches(&fact(Some("p1"))));
        assert!(recall.matches(&fact(None)));
        assert!(!recall.matches(&fact(Some("p2"))));
        let mut other_user = fact(Some("p1"));
        other_user.scope.user = "bob".into();
        assert!(!recall.matches(&other_user));
        assert!(!FactFilter::exactly(&scope).matches(&fact(None)));
        assert!(scope.admits(&fact(None).scope) && !scope.admits(&fact(Some("p2")).scope));
    }
}
