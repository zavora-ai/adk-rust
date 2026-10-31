//! Standing rules: "don't ask again" decisions bound to a subject and a tool declaration
//! (spec 3.1 §7.5).

use std::collections::BTreeMap;

use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{ArgPredicate, STATIC_TOOL_ORIGIN, ToolConfirmationRequest, ToolOrigin};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::matcher::{is_protected_path, plain_shell_words};

/// Who a [`StandingRule`] belongs to (spec 3.1 §7.5).
///
/// A rule never applies to another subject, another app, or a subject of another kind,
/// so a person's rule never applies to a `Service` run.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectKind;
/// use adk_harness::RuleScope;
///
/// let scope = RuleScope::User {
///     app_name: "adk-rust".into(),
///     user_id: "alice".into(),
///     subject_kind: SubjectKind::Human,
/// };
/// assert_eq!(scope.user_id(), "alice");
/// assert_eq!(scope.session_id(), None);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum RuleScope {
    /// Every session of the user in the app.
    #[serde(rename_all = "camelCase")]
    User {
        /// Application name.
        app_name: String,
        /// The subject the rule belongs to.
        user_id: String,
        /// The subject's kind.
        subject_kind: SubjectKind,
    },
    /// One session of the user.
    #[serde(rename_all = "camelCase")]
    Session {
        /// Application name.
        app_name: String,
        /// The subject the rule belongs to.
        user_id: String,
        /// The subject's kind.
        subject_kind: SubjectKind,
        /// The session.
        session_id: String,
    },
}

impl RuleScope {
    /// The application the rule belongs to.
    pub fn app_name(&self) -> &str {
        match self {
            Self::User { app_name, .. } | Self::Session { app_name, .. } => app_name,
        }
    }

    /// The subject the rule belongs to.
    pub fn user_id(&self) -> &str {
        match self {
            Self::User { user_id, .. } | Self::Session { user_id, .. } => user_id,
        }
    }

    /// The subject's kind.
    pub fn subject_kind(&self) -> SubjectKind {
        match self {
            Self::User { subject_kind, .. } | Self::Session { subject_kind, .. } => *subject_kind,
        }
    }

    /// The session, for a session-scoped rule.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::User { .. } => None,
            Self::Session { session_id, .. } => Some(session_id),
        }
    }
}

/// The tool declaration a [`StandingRule`] is bound to (spec 3.1 §7.5, §9.2 I8).
///
/// # Example
///
/// ```rust
/// use adk_core::ToolOrigin;
/// use adk_harness::OriginPin;
/// use serde_json::json;
///
/// let origin = ToolOrigin::new("static", &json!({ "name": "bash" }), None);
/// let pin = OriginPin::exact(&origin);
/// assert!(pin.admits(&origin));
/// assert!(!pin.admits(&ToolOrigin::new("static", &json!({ "name": "bash", "x": 1 }), None)));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum OriginPin {
    /// One publisher and one declaration digest; the rule dies when the declaration changes.
    #[serde(rename_all = "camelCase")]
    Exact {
        /// [`ToolOrigin::source`].
        source: String,
        /// [`ToolOrigin::declaration_digest`].
        declaration_digest: String,
    },
    /// Any declaration from one publisher. Operator-only: suggestions never produce it.
    #[serde(rename_all = "camelCase")]
    AnyDeclaration {
        /// [`ToolOrigin::source`].
        source: String,
    },
    /// One publisher and one exact declaration for each tool it names: the file-writing
    /// tools of a folder rule (spec 3.3 §8.2 "Always allow for this folder"). A call is
    /// covered only through its own tool's declaration, so the rule stops covering a tool
    /// whose declaration changes, as an [`Exact`](Self::Exact) pin does.
    #[serde(rename_all = "camelCase")]
    ExactEach {
        /// [`ToolOrigin::source`].
        source: String,
        /// Each tool's [`ToolOrigin::declaration_digest`], by tool name.
        declarations: BTreeMap<String, String>,
    },
}

impl OriginPin {
    /// A pin to exactly `origin`.
    pub fn exact(origin: &ToolOrigin) -> Self {
        Self::Exact {
            source: origin.source.clone(),
            declaration_digest: origin.declaration_digest.clone(),
        }
    }

    /// Whether a call to a tool with `origin` is covered by the pin.
    pub fn admits(&self, origin: &ToolOrigin) -> bool {
        match self {
            Self::Exact { source, declaration_digest } => {
                *source == origin.source && *declaration_digest == origin.declaration_digest
            }
            Self::AnyDeclaration { source } => *source == origin.source,
            // The digest covers the declaration's name, so it names the tool as well.
            Self::ExactEach { source, declarations } => {
                *source == origin.source
                    && declarations.values().any(|digest| *digest == origin.declaration_digest)
            }
        }
    }

    /// The pinned publisher.
    pub fn source(&self) -> &str {
        match self {
            Self::Exact { source, .. }
            | Self::AnyDeclaration { source }
            | Self::ExactEach { source, .. } => source,
        }
    }
}

/// A serializable mirror of [`ArgPredicate`] for stored rules (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_harness::ArgSpec;
/// use serde_json::json;
///
/// let spec = ArgSpec::Equals { pointer: "/unsandboxed".into(), value: json!(true) };
/// assert!(spec.holds(&json!({ "unsandboxed": true })));
/// assert_eq!(serde_json::to_value(&spec).unwrap()["equals"]["pointer"], "/unsandboxed");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum ArgSpec {
    /// See [`ArgPredicate::Equals`].
    #[serde(rename_all = "camelCase")]
    Equals {
        /// JSON pointer to the argument.
        pointer: String,
        /// The required value.
        value: Value,
    },
    /// See [`ArgPredicate::InSet`].
    #[serde(rename_all = "camelCase")]
    InSet {
        /// JSON pointer to the argument.
        pointer: String,
        /// The permitted values.
        values: Vec<Value>,
    },
    /// See [`ArgPredicate::AtMost`].
    #[serde(rename_all = "camelCase")]
    AtMost {
        /// JSON pointer to the argument.
        pointer: String,
        /// The largest permitted value.
        max: f64,
    },
    /// See [`ArgPredicate::StartsWith`].
    #[serde(rename_all = "camelCase")]
    StartsWith {
        /// JSON pointer to the argument.
        pointer: String,
        /// The required prefix.
        prefix: String,
    },
    /// See [`ArgPredicate::DomainIn`].
    #[serde(rename_all = "camelCase")]
    DomainIn {
        /// JSON pointer to the argument.
        pointer: String,
        /// The permitted hosts.
        domains: Vec<String>,
    },
}

impl ArgSpec {
    /// The core predicate this mirrors.
    pub fn to_predicate(&self) -> ArgPredicate {
        match self {
            Self::Equals { pointer, value } => ArgPredicate::equals(pointer.clone(), value.clone()),
            Self::InSet { pointer, values } => {
                ArgPredicate::in_set(pointer.clone(), values.iter().cloned())
            }
            Self::AtMost { pointer, max } => ArgPredicate::at_most(pointer.clone(), *max),
            Self::StartsWith { pointer, prefix } => {
                ArgPredicate::starts_with(pointer.clone(), prefix.clone())
            }
            Self::DomainIn { pointer, domains } => {
                ArgPredicate::domain_in(pointer.clone(), domains.iter().cloned())
            }
        }
    }

    /// Whether the predicate holds for `args`.
    pub fn holds(&self, args: &Value) -> bool {
        self.to_predicate().holds(args)
    }
}

impl From<ArgSpec> for ArgPredicate {
    fn from(spec: ArgSpec) -> Self {
        spec.to_predicate()
    }
}

/// One condition of a [`StandingRule`] (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_harness::RulePredicate;
/// use serde_json::json;
///
/// let prefix = RulePredicate::ShellPrefix {
///     pointer: "/command".into(),
///     tokens: vec!["cargo".into(), "test".into()],
/// };
/// assert_eq!(serde_json::to_value(&prefix).unwrap()["shellPrefix"]["tokens"][1], "test");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum RulePredicate {
    /// An argument predicate.
    Arg(ArgSpec),
    /// The command at `pointer` tokenizes to plain words whose first tokens are `tokens`.
    #[serde(rename_all = "camelCase")]
    ShellPrefix {
        /// JSON pointer to the command.
        pointer: String,
        /// The required leading tokens.
        tokens: Vec<String>,
    },
    /// The path at `pointer`, resolved against the workspace root, lies within `dir` and is
    /// not a protected path.
    #[serde(rename_all = "camelCase")]
    PathWithin {
        /// JSON pointer to the path.
        pointer: String,
        /// Directory relative to the workspace root; `.` is the root.
        dir: String,
    },
    /// The call only writes files — `write_file`, `edit_file`, or an `apply_patch` that adds
    /// files or updates them in place — and every path it writes lies within `dir` and is
    /// not protected, each resolved as `PathWithin` resolves its path. A delete or a move
    /// never holds (spec 3.3 §8.2 "Always allow for this folder").
    #[serde(rename_all = "camelCase")]
    WritesWithin {
        /// Directory relative to the workspace root; `.` is the root.
        dir: String,
    },
}

/// What a [`StandingRule`] decides (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_harness::StandingDecision;
///
/// assert_eq!(serde_json::to_value(StandingDecision::Allow).unwrap()["kind"], "allow");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", try_from = "RawDecision")]
pub enum StandingDecision {
    /// Calls the rule covers run without approval where the mode would ask.
    Allow,
    /// Calls the rule covers are refused.
    Deny {
        /// Reported to the model.
        reason: String,
    },
}

/// The wire form of [`StandingDecision`], parsed strictly: serde ignores unknown fields
/// on unit variants of internally tagged enums, and a rule with unknown fields must not
/// parse (spec 3.1 §9.2 I16).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDecision {
    kind: String,
    #[serde(default)]
    reason: Option<String>,
}

impl TryFrom<RawDecision> for StandingDecision {
    type Error = String;

    fn try_from(raw: RawDecision) -> std::result::Result<Self, Self::Error> {
        match (raw.kind.as_str(), raw.reason) {
            ("allow", None) => Ok(Self::Allow),
            ("deny", Some(reason)) => Ok(Self::Deny { reason }),
            (kind, _) => Err(format!("unknown standing decision '{kind}'")),
        }
    }
}

/// A standing approval or denial (spec 3.1 §7.5).
///
/// Created only by an authenticated human action (spec 3.1 §9.2 I2). A rule with fields
/// this release does not know fails to parse and is ignored (I16).
///
/// # Example
///
/// ```rust
/// use adk_core::principal::{SubjectKind, SubjectRef};
/// use adk_core::ToolOrigin;
/// use adk_harness::{OriginPin, RulePredicate, RuleScope, StandingDecision, StandingRule};
/// use serde_json::json;
///
/// let rule = StandingRule {
///     id: "r1".into(),
///     scope: RuleScope::User {
///         app_name: "adk-rust".into(),
///         user_id: "alice".into(),
///         subject_kind: SubjectKind::Human,
///     },
///     tool: "bash".into(),
///     origin: OriginPin::exact(&ToolOrigin::new("static", &json!({ "name": "bash" }), None)),
///     agent: None,
///     predicates: vec![RulePredicate::ShellPrefix {
///         pointer: "/command".into(),
///         tokens: vec!["cargo".into(), "test".into()],
///     }],
///     decision: StandingDecision::Allow,
///     created_by: SubjectRef::human("alice"),
///     created_at: chrono::Utc::now(),
///     expires_at: None,
///     label: "cargo test".into(),
///     source_fingerprint: None,
/// };
/// let json = serde_json::to_value(&rule).unwrap();
/// assert_eq!(serde_json::from_value::<StandingRule>(json).unwrap(), rule);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StandingRule {
    /// Unique id.
    pub id: String,
    /// Who the rule belongs to.
    pub scope: RuleScope,
    /// Tool-name glob (`*` any run, `?` one character).
    pub tool: String,
    /// The declaration the rule is bound to.
    pub origin: OriginPin,
    /// Acting-agent id glob; `None` matches any agent.
    #[serde(default)]
    pub agent: Option<String>,
    /// Every predicate must hold.
    #[serde(default)]
    pub predicates: Vec<RulePredicate>,
    /// What the rule decides.
    pub decision: StandingDecision,
    /// The human who created it.
    pub created_by: SubjectRef,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it stops applying.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Shown in rule listings.
    pub label: String,
    /// Fingerprint of the call the rule was created from, if any.
    #[serde(default)]
    pub source_fingerprint: Option<String>,
}

/// Commands that a shell-prefix suggestion never starts with: each runs an arbitrary
/// command of its own (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::SHELL_PREFIX_SUGGESTION_DENYLIST.contains(&"sudo"));
/// ```
pub const SHELL_PREFIX_SUGGESTION_DENYLIST: &[&str] =
    &["sh", "bash", "env", "xargs", "eval", "exec", "sudo", "nohup"];

/// The scope a [`RuleSuggestion`] proposes (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// assert_eq!(serde_json::to_value(adk_harness::SuggestedScope::User).unwrap(), "user");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SuggestedScope {
    /// Every session of the user.
    User,
    /// This session only.
    Session,
}

/// A rule a prompt offers to remember (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_core::{ToolConfirmationRequest, ToolOrigin};
/// use adk_harness::StandingRule;
/// use serde_json::json;
///
/// let request = ToolConfirmationRequest::new("bash", json!({ "command": "cargo test --quiet" }))
///     .with_origin(ToolOrigin::new("static", &json!({ "name": "bash" }), None));
/// let labels: Vec<_> =
///     StandingRule::suggestions(&request).into_iter().map(|s| s.label).collect();
/// assert_eq!(labels, ["bash: cargo test …", "bash: cargo …"]);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleSuggestion {
    /// What the rule matches, never its scope: surfaces render the scope from
    /// [`scope`](Self::scope) (spec 3.1 §7.33 fix 1).
    pub label: String,
    /// Proposed scope.
    pub scope: SuggestedScope,
    /// Tool-name glob.
    pub tool: String,
    /// The declaration the rule binds to: [`OriginPin::Exact`], or for a folder rule
    /// [`OriginPin::ExactEach`] over its file-writing tools.
    pub origin: OriginPin,
    /// Conditions.
    pub predicates: Vec<RulePredicate>,
    /// The matched commands run project scripts (`cargo test`, `npm test`, `make`), so the
    /// rule allows whatever those scripts do.
    pub runs_scripts: bool,
}

/// The owner and creator a remembered rule is recorded for (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_core::principal::{SubjectKind, SubjectRef};
/// use adk_harness::RuleOwner;
///
/// let owner = RuleOwner {
///     app_name: "adk-rust".into(),
///     user_id: "alice".into(),
///     subject_kind: SubjectKind::Human,
///     session_id: "s1".into(),
///     created_by: SubjectRef::human("alice"),
/// };
/// # let _ = owner;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleOwner {
    /// Application name.
    pub app_name: String,
    /// The subject the rule belongs to.
    pub user_id: String,
    /// The subject's kind.
    pub subject_kind: SubjectKind,
    /// The session, used for session-scoped rules.
    pub session_id: String,
    /// The human creating the rule.
    pub created_by: SubjectRef,
}

impl RuleSuggestion {
    /// The rule this suggestion becomes when a person accepts it.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::principal::{SubjectKind, SubjectRef};
    /// use adk_core::{ToolConfirmationRequest, ToolOrigin};
    /// use adk_harness::{RuleOwner, StandingDecision, StandingRule};
    /// use serde_json::json;
    ///
    /// let request = ToolConfirmationRequest::new("bash", json!({ "command": "make" }))
    ///     .with_origin(ToolOrigin::new("static", &json!({ "name": "bash" }), None));
    /// let suggestion = StandingRule::suggestions(&request).remove(0);
    /// let owner = RuleOwner {
    ///     app_name: "app".into(),
    ///     user_id: "alice".into(),
    ///     subject_kind: SubjectKind::Human,
    ///     session_id: "s1".into(),
    ///     created_by: SubjectRef::human("alice"),
    /// };
    /// let rule = suggestion.into_rule(&owner, StandingDecision::Allow, None);
    /// assert_eq!(rule.scope.user_id(), "alice");
    /// ```
    pub fn into_rule(
        self,
        owner: &RuleOwner,
        decision: StandingDecision,
        source_fingerprint: Option<String>,
    ) -> StandingRule {
        let scope = match self.scope {
            SuggestedScope::User => RuleScope::User {
                app_name: owner.app_name.clone(),
                user_id: owner.user_id.clone(),
                subject_kind: owner.subject_kind,
            },
            SuggestedScope::Session => RuleScope::Session {
                app_name: owner.app_name.clone(),
                user_id: owner.user_id.clone(),
                subject_kind: owner.subject_kind,
                session_id: owner.session_id.clone(),
            },
        };
        StandingRule {
            id: uuid::Uuid::new_v4().to_string(),
            scope,
            tool: self.tool,
            origin: self.origin,
            agent: None,
            predicates: self.predicates,
            decision,
            created_by: owner.created_by.clone(),
            created_at: Utc::now(),
            expires_at: None,
            label: self.label,
            source_fingerprint,
        }
    }
}

/// Names of the harness edit tools.
pub(crate) const EDIT_TOOLS: &[&str] = &["write_file", "edit_file", "apply_patch"];

/// Tools no standing rule ever covers (spec 3.1 §7.5).
pub(crate) const NEVER_ELIGIBLE_TOOLS: &[&str] =
    &["exit_plan_mode", "memory_write", "memory_delete"];

/// Commands whose first tokens run project scripts.
fn runs_scripts(tokens: &[&str]) -> bool {
    let first = tokens.first().copied().unwrap_or_default();
    let second = tokens.get(1).copied().unwrap_or_default();
    match first {
        "make" | "just" | "gradle" | "./gradlew" | "mvn" | "rake" | "tox" | "nox" => true,
        "cargo" => !matches!(second, "fmt" | "metadata" | "tree" | "--version" | "search"),
        "npm" | "pnpm" | "yarn" | "bun" => {
            matches!(second, "test" | "run" | "start" | "install" | "ci" | "exec" | "build")
        }
        "npx" | "bunx" | "node" | "deno" | "python" | "python3" | "ruby" | "perl" | "go" => true,
        other => other.contains('/'),
    }
}

impl StandingRule {
    /// Rules a prompt for `request` offers to remember (spec 3.1 §7.5).
    ///
    /// For `bash`, a `ShellPrefix` on the first two tokens and on the first token (user
    /// scope), offered only for a command of plain words that does not start with a
    /// command runner; for edits, `PathWithin` the workspace root or the file's directory
    /// (session scope), never for a protected path; for any other tool, any arguments for
    /// this tool pinned to its exact origin (user scope). Every suggestion pins the exact
    /// declaration, so a request without an origin gets none. `exit_plan_mode`, memory
    /// writes, payments and unsandboxed retries get none: they are never covered by rules.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ToolConfirmationRequest, ToolOrigin};
    /// use adk_harness::StandingRule;
    /// use serde_json::json;
    ///
    /// let origin = ToolOrigin::new("static", &json!({ "name": "bash" }), None);
    /// let chained = ToolConfirmationRequest::new("bash", json!({ "command": "ls; rm -rf ~" }))
    ///     .with_origin(origin.clone());
    /// assert!(StandingRule::suggestions(&chained).is_empty());
    /// let wrapped = ToolConfirmationRequest::new("bash", json!({ "command": "sudo ls" }))
    ///     .with_origin(origin);
    /// assert!(StandingRule::suggestions(&wrapped).is_empty());
    /// ```
    pub fn suggestions(request: &ToolConfirmationRequest) -> Vec<RuleSuggestion> {
        let Some(origin) = request.origin.as_ref() else {
            return Vec::new();
        };
        let name = request.tool_name.as_str();
        if NEVER_ELIGIBLE_TOOLS.contains(&name) {
            return Vec::new();
        }
        let pin = OriginPin::exact(origin);
        let static_tool = origin.source == STATIC_TOOL_ORIGIN;
        if name == "bash" && static_tool {
            if request.args.get("unsandboxed").and_then(Value::as_bool) == Some(true) {
                return Vec::new();
            }
            let Some(command) = request.args.get("command").and_then(Value::as_str) else {
                return Vec::new();
            };
            let Some(words) = plain_shell_words(command) else {
                return Vec::new();
            };
            let Some(first) = words.first() else {
                return Vec::new();
            };
            if SHELL_PREFIX_SUGGESTION_DENYLIST.contains(first) {
                return Vec::new();
            }
            let scripts = runs_scripts(&words);
            let mut out = Vec::new();
            for width in [2usize, 1] {
                if words.len() < width {
                    continue;
                }
                let tokens: Vec<String> = words[..width].iter().map(|w| (*w).to_string()).collect();
                out.push(RuleSuggestion {
                    label: format!("bash: {} …", tokens.join(" ")),
                    scope: SuggestedScope::User,
                    tool: "bash".into(),
                    origin: pin.clone(),
                    predicates: vec![RulePredicate::ShellPrefix {
                        pointer: "/command".into(),
                        tokens,
                    }],
                    runs_scripts: scripts,
                });
            }
            return out;
        }
        // Web: the exact host the call named, never a wildcard the human did not type
        // (spec 3.1 §7.5, §7.20); the browser pack's navigation is a web call too (§7.24).
        if matches!(
            name,
            "web_fetch" | "browser_navigate" | "browser_new_tab" | "browser_new_window"
        ) && static_tool
        {
            let Some(host) =
                request.args.get("url").and_then(Value::as_str).and_then(crate::web::url_host)
            else {
                return Vec::new();
            };
            if crate::web::is_private_host(&host) {
                return Vec::new();
            }
            return vec![RuleSuggestion {
                label: format!("{name}: any page on {host}"),
                scope: SuggestedScope::User,
                tool: name.into(),
                origin: pin,
                predicates: vec![RulePredicate::Arg(ArgSpec::DomainIn {
                    pointer: "/url".into(),
                    domains: vec![host],
                })],
                runs_scripts: false,
            }];
        }
        if name == "web_search" && static_tool {
            let Some(site) = request.args.get("site").and_then(Value::as_str) else {
                return Vec::new();
            };
            let Some(host) = crate::web::url_host(&format!("https://{site}/")) else {
                return Vec::new();
            };
            return vec![RuleSuggestion {
                label: format!("web_search: searches restricted to {host}"),
                scope: SuggestedScope::User,
                tool: "web_search".into(),
                origin: pin,
                predicates: vec![RulePredicate::Arg(ArgSpec::Equals {
                    pointer: "/site".into(),
                    value: Value::String(host),
                })],
                runs_scripts: false,
            }];
        }
        if EDIT_TOOLS.contains(&name) && static_tool {
            let Some(paths) = crate::edits::edit_paths(name, &request.args) else {
                return Vec::new();
            };
            if paths.is_empty()
                || paths.iter().any(|path| {
                    is_protected_path(path) || path.split(['/', '\\']).any(|part| part == "..")
                })
            {
                return Vec::new();
            }
            // `apply_patch` reads its paths from the patch; the others from `/path`.
            let pointer = if name == crate::edits::APPLY_PATCH { "/patch" } else { "/path" };
            let mut out = vec![RuleSuggestion {
                label: format!("{name}: anywhere in the workspace"),
                scope: SuggestedScope::Session,
                tool: name.to_string(),
                origin: pin.clone(),
                predicates: vec![RulePredicate::PathWithin {
                    pointer: pointer.into(),
                    dir: ".".into(),
                }],
                runs_scripts: false,
            }];
            if let Some(dir) = shared_dir(&paths) {
                out.push(RuleSuggestion {
                    label: format!("{name}: within {dir}/"),
                    scope: SuggestedScope::Session,
                    tool: name.to_string(),
                    origin: pin,
                    predicates: vec![RulePredicate::PathWithin { pointer: pointer.into(), dir }],
                    runs_scripts: false,
                });
            }
            return out;
        }
        if static_tool && crate::policy::HARNESS_TOOL_NAMES.contains(&name) {
            // Reads and todos never ask; nothing to remember.
            return Vec::new();
        }
        vec![RuleSuggestion {
            label: format!("{name}: any arguments from {}", origin.source),
            scope: SuggestedScope::User,
            tool: name.to_string(),
            origin: pin,
            predicates: Vec::new(),
            runs_scripts: false,
        }]
    }

    /// The rule "Always allow for this folder" makes for `request` (spec 3.3 §8.2): every file
    /// write within the folder the call writes in — the workspace root for a file there or an
    /// absolute path — by any of the harness's file-writing tools, `write_file`, `edit_file`
    /// and `apply_patch`, each pinned to the declaration `origin_of` gives for it; scoped to
    /// the session, as the edit suggestions are.
    ///
    /// `None` when the call is not one of those tools with that declaration, when it deletes
    /// or moves a file, or when a path it writes is protected or climbs with `..`.
    pub(crate) fn folder_suggestion(
        request: &ToolConfirmationRequest,
        origin_of: impl Fn(&str) -> Option<ToolOrigin>,
    ) -> Option<RuleSuggestion> {
        let origin = request.origin.as_ref()?;
        let name = request.tool_name.as_str();
        if origin.source != STATIC_TOOL_ORIGIN
            || !EDIT_TOOLS.contains(&name)
            || origin_of(name).as_ref() != Some(origin)
        {
            return None;
        }
        let paths = crate::edits::written_paths(name, &request.args)?;
        if paths.is_empty()
            || paths.iter().any(|path| {
                is_protected_path(path) || path.split(['/', '\\']).any(|part| part == "..")
            })
        {
            return None;
        }
        let declarations = EDIT_TOOLS
            .iter()
            .filter_map(|tool| Some((tool.to_string(), origin_of(tool)?)))
            .filter(|(_, origin)| origin.source == STATIC_TOOL_ORIGIN)
            .map(|(tool, origin)| (tool, origin.declaration_digest))
            .collect();
        let (dir, label) = match shared_dir(&paths) {
            Some(dir) => (dir.clone(), format!("file writes: within {dir}/")),
            None => (".".to_string(), "file writes: anywhere in the workspace".to_string()),
        };
        Some(RuleSuggestion {
            label,
            scope: SuggestedScope::Session,
            tool: "*".into(),
            origin: OriginPin::ExactEach { source: STATIC_TOOL_ORIGIN.into(), declarations },
            predicates: vec![RulePredicate::WritesWithin { dir }],
            runs_scripts: false,
        })
    }

    /// Whether the rule has expired at `now`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// assert!(!rule.is_expired(chrono::Utc::now()));
    /// ```
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|at| now >= at)
    }
}

/// The directory every one of `paths` lies within, relative to the workspace root; `None`
/// when that is the root itself or a path is absolute.
fn shared_dir(paths: &[String]) -> Option<String> {
    paths
        .iter()
        .map(|path| {
            std::path::Path::new(path.trim_start_matches("./"))
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .reduce(|shared, next| common_dir(&shared, &next))
        .filter(|parent| !parent.is_empty() && !parent.starts_with('/'))
}

/// The longest directory both `a` and `b` lie within, by whole components.
fn common_dir(a: &str, b: &str) -> String {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(left, right)| left == right)
        .map(|(part, _)| part)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_fields_make_a_rule_unparseable() {
        let mut value = json!({
            "id": "r", "scope": { "kind": "user", "appName": "a", "userId": "u", "subjectKind": "human" },
            "tool": "bash", "origin": { "kind": "exact", "source": "static", "declarationDigest": "sha256:x" },
            "predicates": [], "decision": { "kind": "allow" },
            "createdBy": { "kind": "human", "id": "u" }, "createdAt": "2026-10-20T00:00:00Z", "label": "l"
        });
        assert!(serde_json::from_value::<StandingRule>(value.clone()).is_ok());
        value["everything"] = json!(true);
        assert!(serde_json::from_value::<StandingRule>(value.clone()).is_err());
        value.as_object_mut().unwrap().remove("everything");
        value["scope"]["extra"] = json!(1);
        assert!(serde_json::from_value::<StandingRule>(value.clone()).is_err());
        value["scope"].as_object_mut().unwrap().remove("extra");
        value["decision"] = json!({ "kind": "allow", "always": true });
        assert!(serde_json::from_value::<StandingRule>(value.clone()).is_err());
        value["decision"] = json!({ "kind": "allow" });
        value["predicates"] =
            json!([{ "shellPrefix": { "pointer": "/command", "tokens": [], "x": 1 } }]);
        assert!(serde_json::from_value::<StandingRule>(value).is_err());
    }

    #[test]
    fn edit_suggestions_are_session_scoped_and_skip_protected_paths() {
        let origin = ToolOrigin::new("static", &json!({ "name": "write_file" }), None);
        let request = ToolConfirmationRequest::new("write_file", json!({ "path": "src/lib.rs" }))
            .with_origin(origin.clone());
        let suggestions = StandingRule::suggestions(&request);
        assert_eq!(suggestions.len(), 2);
        assert!(suggestions.iter().all(|s| s.scope == SuggestedScope::Session));
        let protected = ToolConfirmationRequest::new("write_file", json!({ "path": "AGENTS.md" }))
            .with_origin(origin);
        assert!(StandingRule::suggestions(&protected).is_empty());
    }

    /// The harness's three file-writing tools, as `folder_suggestion` looks them up.
    fn writer(name: &str) -> Option<ToolOrigin> {
        EDIT_TOOLS
            .contains(&name)
            .then(|| ToolOrigin::new("static", &json!({ "name": name }), None))
    }

    #[test]
    fn the_folder_rule_pins_every_file_writing_tool_and_names_the_folder() {
        let request = ToolConfirmationRequest::new("write_file", json!({ "path": "notes/a.md" }))
            .with_origin(writer("write_file").unwrap());
        let folder = StandingRule::folder_suggestion(&request, writer).unwrap();
        let declarations: BTreeMap<String, String> = EDIT_TOOLS
            .iter()
            .map(|tool| (tool.to_string(), writer(tool).unwrap().declaration_digest))
            .collect();
        assert_eq!(
            folder,
            RuleSuggestion {
                label: "file writes: within notes/".into(),
                scope: SuggestedScope::Session,
                tool: "*".into(),
                origin: OriginPin::ExactEach { source: "static".into(), declarations },
                predicates: vec![RulePredicate::WritesWithin { dir: "notes".into() }],
                runs_scripts: false,
            }
        );
        // Every tool it pins is admitted, and only at its declaration.
        for tool in EDIT_TOOLS {
            assert!(folder.origin.admits(&writer(tool).unwrap()), "{tool}");
        }
        let changed = ToolOrigin::new("static", &json!({ "name": "edit_file", "v": 2 }), None);
        assert!(!folder.origin.admits(&changed));
        let other = ToolOrigin::new("static", &json!({ "name": "read_file" }), None);
        assert!(!folder.origin.admits(&other));
        let mcp = ToolOrigin { source: "mcp:fs".into(), ..writer("write_file").unwrap() };
        assert!(!folder.origin.admits(&mcp));

        // A file at the root is the workspace's folder.
        let root = ToolConfirmationRequest::new("edit_file", json!({ "path": "a.md" }))
            .with_origin(writer("edit_file").unwrap());
        let workspace = StandingRule::folder_suggestion(&root, writer).unwrap();
        assert_eq!(workspace.label, "file writes: anywhere in the workspace");
        assert_eq!(workspace.predicates, [RulePredicate::WritesWithin { dir: ".".into() }]);
    }

    #[test]
    fn no_folder_rule_for_a_delete_a_move_an_escape_or_another_declaration() {
        let patch = |text: &str| {
            ToolConfirmationRequest::new("apply_patch", json!({ "patch": text }))
                .with_origin(writer("apply_patch").unwrap())
        };
        let add = patch("*** Begin Patch\n*** Add File: notes/a.md\n+x\n*** End Patch");
        assert!(StandingRule::folder_suggestion(&add, writer).is_some());
        for refused in [
            patch("*** Begin Patch\n*** Delete File: notes/a.md\n*** End Patch"),
            patch(
                "*** Begin Patch\n*** Update File: notes/a.md\n*** Move to: notes/b.md\n@@\n-x\n+y\n*** End Patch",
            ),
            ToolConfirmationRequest::new("write_file", json!({ "path": "notes/../a.md" }))
                .with_origin(writer("write_file").unwrap()),
            ToolConfirmationRequest::new("write_file", json!({ "path": "notes/AGENTS.md" }))
                .with_origin(writer("write_file").unwrap()),
            ToolConfirmationRequest::new("write_file", json!({ "path": "notes/a.md" }))
                .with_origin(ToolOrigin::new(
                    "static",
                    &json!({ "name": "write_file", "v": 2 }),
                    None,
                )),
            ToolConfirmationRequest::new("write_file", json!({ "path": "notes/a.md" })),
            ToolConfirmationRequest::new("bash", json!({ "command": "touch notes/a.md" }))
                .with_origin(ToolOrigin::new("static", &json!({ "name": "bash" }), None)),
        ] {
            assert_eq!(StandingRule::folder_suggestion(&refused, writer), None, "{refused:?}");
        }
    }

    #[test]
    fn non_waivable_tools_get_no_suggestions() {
        let origin = ToolOrigin::new("static", &json!({ "name": "exit_plan_mode" }), None);
        let request = ToolConfirmationRequest::new("exit_plan_mode", json!({ "plan": "x" }))
            .with_origin(origin);
        assert!(StandingRule::suggestions(&request).is_empty());
        let bash = ToolOrigin::new("static", &json!({ "name": "bash" }), None);
        let retry = ToolConfirmationRequest::new(
            "bash",
            json!({ "command": "cargo test", "unsandboxed": true }),
        )
        .with_origin(bash);
        assert!(StandingRule::suggestions(&retry).is_empty());
    }

    #[test]
    fn script_runners_are_flagged() {
        let origin = ToolOrigin::new("static", &json!({ "name": "bash" }), None);
        for (command, scripts) in [("cargo test", true), ("ls -la", false), ("make", true)] {
            let request = ToolConfirmationRequest::new("bash", json!({ "command": command }))
                .with_origin(origin.clone());
            assert!(StandingRule::suggestions(&request).iter().all(|s| s.runs_scripts == scripts));
        }
    }
}
