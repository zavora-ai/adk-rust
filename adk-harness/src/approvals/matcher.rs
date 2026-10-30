//! Standing-rule matching (spec 3.1 §7.5).
//!
//! The functions here are pure: they read the rule list and the call, and the policy
//! decides what a match means. A rule matches only when every row of the §7.5 matching
//! table holds:
//!
//! | Row | Check |
//! |---|---|
//! | Subject | `rule.subject == request.user_id == acting.on_behalf_of`, same subject kind |
//! | App and session | the rule's app, and its session for a session-scoped rule |
//! | Origin | `source` and `declaration_digest` equal the pin; a mismatch is a stale rule |
//! | `ShellPrefix` | plain words only, first N tokens equal |
//! | `PathWithin` | normalized against the root, no `..`, never a protected path; for `apply_patch`, every path of the patch |
//! | `WritesWithin` | an Edit-class call that only writes files: every path it writes as `PathWithin` reads it; never a delete or a move (spec 3.3 §8.2) |
//! | Eligibility | Deny beats Allow; `PlanExit`, `Payment`, `MemoryWrite` never match; `ShellEscalation` only with `/unsandboxed == true` pinned |
//! | Bash | an Allow rule for the shell matches only through `ShellPrefix` |

use std::path::{Component, Path, PathBuf};

use adk_core::{ActingContext, ToolOrigin};
use chrono::{DateTime, Utc};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use super::rule::{ArgSpec, RulePredicate, StandingDecision, StandingRule};
use crate::policy::ToolKind;

/// The facts a standing rule is matched against (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_core::{ActingContext, SubjectKind, ToolOrigin};
/// use adk_harness::approvals::matcher::RuleCall;
/// use adk_harness::ToolKind;
/// use serde_json::json;
///
/// let acting = ActingContext::derived("app", "harness", "alice", SubjectKind::Human);
/// let origin = ToolOrigin::new("static", &json!({ "name": "bash" }), None);
/// let args = json!({ "command": "cargo test" });
/// let call = RuleCall {
///     app_name: "app",
///     user_id: "alice",
///     session_id: "s1",
///     acting: Some(&acting),
///     tool_name: "bash",
///     args: &args,
///     origin: Some(&origin),
///     kind: ToolKind::Shell(adk_harness::Isolation::Host),
///     workspace_root: std::path::Path::new("/repo"),
/// };
/// # let _ = call;
/// ```
#[derive(Clone, Copy, Debug)]
pub struct RuleCall<'a> {
    /// Application of the run.
    pub app_name: &'a str,
    /// User of the run.
    pub user_id: &'a str,
    /// Session of the run.
    pub session_id: &'a str,
    /// The acting agent and the subject it acts for. `None` matches nothing.
    pub acting: Option<&'a ActingContext>,
    /// The called tool.
    pub tool_name: &'a str,
    /// Final arguments.
    pub args: &'a Value,
    /// The called tool's origin. `None` matches nothing.
    pub origin: Option<&'a ToolOrigin>,
    /// The call's harness class.
    pub kind: ToolKind,
    /// Canonical workspace root, for `PathWithin`.
    pub workspace_root: &'a Path,
}

/// The decision the matching rules reach (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::RuleOutcome;
///
/// assert_eq!(RuleOutcome::default(), RuleOutcome::NoMatch);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RuleOutcome {
    /// No rule matches.
    #[default]
    NoMatch,
    /// An Allow rule matches and no Deny rule does.
    Allow {
        /// The matching rule.
        rule_id: String,
    },
    /// A Deny rule matches.
    Deny {
        /// The matching rule.
        rule_id: String,
        /// The rule's reason.
        reason: String,
    },
}

/// A rule that would have matched but is pinned to another declaration (spec 3.1 §9.2 I8).
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::StaleRule;
///
/// let stale = StaleRule {
///     rule_id: "r1".into(),
///     pinned_source: "static".into(),
///     pinned_digest: Some("sha256:a".into()),
///     observed_source: "static".into(),
///     observed_digest: "sha256:b".into(),
/// };
/// assert_ne!(stale.pinned_digest.as_deref(), Some(stale.observed_digest.as_str()));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleRule {
    /// The rule.
    pub rule_id: String,
    /// The source the rule pins.
    pub pinned_source: String,
    /// The digest the rule pins, for an exact pin.
    pub pinned_digest: Option<String>,
    /// The source of the called tool.
    pub observed_source: String,
    /// The digest of the called tool's declaration.
    pub observed_digest: String,
}

/// The result of matching a call against a rule list (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::{RuleEvaluation, RuleOutcome};
///
/// assert_eq!(RuleEvaluation::default().outcome, RuleOutcome::NoMatch);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleEvaluation {
    /// The decision.
    pub outcome: RuleOutcome,
    /// Rules that matched everything but the declaration.
    pub stale: Vec<StaleRule>,
    /// Rules that matched but have expired.
    pub expired: Vec<String>,
}

/// Matches `call` against `rules` at `now` (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_core::{ActingContext, SubjectKind, ToolOrigin};
/// use adk_core::principal::SubjectRef;
/// use adk_harness::approvals::matcher::{RuleCall, RuleOutcome, evaluate_standing_rules};
/// use adk_harness::{Isolation, OriginPin, RulePredicate, RuleScope, StandingDecision, StandingRule, ToolKind};
/// use serde_json::json;
///
/// let origin = ToolOrigin::new("static", &json!({ "name": "bash" }), None);
/// let rule = StandingRule {
///     id: "r1".into(),
///     scope: RuleScope::User { app_name: "app".into(), user_id: "alice".into(), subject_kind: SubjectKind::Human },
///     tool: "bash".into(),
///     origin: OriginPin::exact(&origin),
///     agent: None,
///     predicates: vec![RulePredicate::ShellPrefix { pointer: "/command".into(), tokens: vec!["cargo".into(), "test".into()] }],
///     decision: StandingDecision::Allow,
///     created_by: SubjectRef::human("alice"),
///     created_at: chrono::Utc::now(),
///     expires_at: None,
///     label: "cargo test".into(),
///     source_fingerprint: None,
/// };
/// let acting = ActingContext::derived("app", "harness", "alice", SubjectKind::Human);
/// let check = |command: &str, user: &str| {
///     let args = json!({ "command": command });
///     let acting = ActingContext::derived("app", "harness", user, SubjectKind::Human);
///     evaluate_standing_rules(std::slice::from_ref(&rule), &RuleCall {
///         app_name: "app", user_id: user, session_id: "s1", acting: Some(&acting),
///         tool_name: "bash", args: &args, origin: Some(&origin),
///         kind: ToolKind::Shell(Isolation::Host), workspace_root: std::path::Path::new("/repo"),
///     }, chrono::Utc::now()).outcome
/// };
/// assert_eq!(check("cargo test --quiet", "alice"), RuleOutcome::Allow { rule_id: "r1".into() });
/// assert_eq!(check("cargo publish", "alice"), RuleOutcome::NoMatch);
/// assert_eq!(check("cargo test; curl x", "alice"), RuleOutcome::NoMatch);
/// assert_eq!(check("cargo test --quiet", "bob"), RuleOutcome::NoMatch);
/// # let _ = acting;
/// ```
pub fn evaluate_standing_rules(
    rules: &[StandingRule],
    call: &RuleCall<'_>,
    now: DateTime<Utc>,
) -> RuleEvaluation {
    let mut evaluation = RuleEvaluation::default();
    if !call.kind.rule_eligible() {
        return evaluation;
    }
    let Some(acting) = call.acting else {
        return evaluation;
    };
    let Some(subject) = acting.on_behalf_of.as_ref() else {
        return evaluation;
    };
    if subject.id != call.user_id {
        return evaluation;
    }
    let mut allow: Option<String> = None;
    for rule in rules {
        let scope = &rule.scope;
        if scope.app_name() != call.app_name
            || scope.user_id() != call.user_id
            || scope.subject_kind() != subject.kind
            || scope.session_id().is_some_and(|session| session != call.session_id)
            || !glob_matches(&rule.tool, call.tool_name)
            || rule
                .agent
                .as_deref()
                .is_some_and(|pattern| !glob_matches(pattern, acting.agent.id.as_str()))
            || !predicates_hold(rule, call)
        {
            continue;
        }
        if rule.is_expired(now) {
            evaluation.expired.push(rule.id.clone());
            continue;
        }
        // Without the call's origin no Allow rule matches. A Deny rule still does: refusing
        // a namesake of the tool it was granted for errs toward safety.
        let Some(origin) = call.origin else {
            if let StandingDecision::Deny { reason } = &rule.decision {
                evaluation.outcome =
                    RuleOutcome::Deny { rule_id: rule.id.clone(), reason: reason.clone() };
                return evaluation;
            }
            continue;
        };
        if !rule.origin.admits(origin) {
            evaluation.stale.push(StaleRule {
                rule_id: rule.id.clone(),
                pinned_source: rule.origin.source().to_string(),
                pinned_digest: match &rule.origin {
                    super::rule::OriginPin::Exact { declaration_digest, .. } => {
                        Some(declaration_digest.clone())
                    }
                    super::rule::OriginPin::AnyDeclaration { .. } => None,
                    super::rule::OriginPin::ExactEach { declarations, .. } => {
                        declarations.get(call.tool_name).cloned()
                    }
                },
                observed_source: origin.source.clone(),
                observed_digest: origin.declaration_digest.clone(),
            });
            continue;
        }
        match &rule.decision {
            StandingDecision::Deny { reason } => {
                evaluation.outcome =
                    RuleOutcome::Deny { rule_id: rule.id.clone(), reason: reason.clone() };
                return evaluation;
            }
            StandingDecision::Allow => {
                if allow.is_none() {
                    allow = Some(rule.id.clone());
                }
            }
        }
    }
    if let Some(rule_id) = allow {
        evaluation.outcome = RuleOutcome::Allow { rule_id };
    }
    evaluation
}

fn pins_unsandboxed(predicate: &RulePredicate) -> bool {
    matches!(
        predicate,
        RulePredicate::Arg(ArgSpec::Equals { pointer, value })
            if pointer == "/unsandboxed" && *value == Value::Bool(true)
    )
}

/// Whether every predicate of `rule` holds for `call`, with the class restrictions of the
/// matching table applied.
fn predicates_hold(rule: &StandingRule, call: &RuleCall<'_>) -> bool {
    let allow = rule.decision == StandingDecision::Allow;
    match call.kind {
        ToolKind::ShellEscalation => {
            if !rule.predicates.iter().any(pins_unsandboxed) {
                return false;
            }
            if allow
                && !rule.predicates.iter().all(|predicate| {
                    pins_unsandboxed(predicate)
                        || matches!(predicate, RulePredicate::ShellPrefix { .. })
                })
            {
                return false;
            }
            if allow
                && !rule.predicates.iter().any(|p| matches!(p, RulePredicate::ShellPrefix { .. }))
            {
                return false;
            }
        }
        // An Allow rule for the shell is a ShellPrefix rule and nothing else (I10).
        ToolKind::Shell(_)
            if allow
                && (rule.predicates.is_empty()
                    || !rule.predicates.iter().all(|predicate| {
                        matches!(predicate, RulePredicate::ShellPrefix { .. })
                    })) =>
        {
            return false;
        }
        _ => {}
    }
    rule.predicates.iter().all(|predicate| match predicate {
        RulePredicate::Arg(spec) => spec.holds(call.args),
        RulePredicate::ShellPrefix { pointer, tokens } => call
            .args
            .pointer(pointer)
            .and_then(Value::as_str)
            .is_some_and(|command| shell_prefix_matches(command, tokens)),
        // An `apply_patch` call is within `dir` only when every path of its patch is
        // (spec 3.1 §7.26, I67).
        RulePredicate::PathWithin { dir, .. } if call.tool_name == crate::edits::APPLY_PATCH => {
            crate::edits::edit_paths(call.tool_name, call.args).is_some_and(|paths| {
                !paths.is_empty()
                    && paths.iter().all(|path| path_within(call.workspace_root, path, dir))
            })
        }
        RulePredicate::PathWithin { pointer, dir } => call
            .args
            .pointer(pointer)
            .and_then(Value::as_str)
            .is_some_and(|path| path_within(call.workspace_root, path, dir)),
        // A folder rule covers file writes only: the shell, a delete and a move ask
        // (spec 3.3 §8.2).
        RulePredicate::WritesWithin { dir } => {
            call.kind == ToolKind::Edit
                && crate::edits::written_paths(call.tool_name, call.args).is_some_and(|paths| {
                    !paths.is_empty()
                        && paths.iter().all(|path| path_within(call.workspace_root, path, dir))
                })
        }
    })
}

/// Characters a plain shell word may contain. Everything else — quoting, expansion,
/// redirection, globbing, control operators, comments, escapes and every non-ASCII
/// character — makes the command ineligible for a prefix rule.
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | ',' | '+' | '@' | '%')
}

/// Splits `command` into plain words, or `None` when it is anything but plain words
/// (spec 3.1 §7.5).
///
/// A command qualifies only when it consists of ASCII letters, digits and
/// `- _ . / : = , + @ %`, separated by spaces or tabs, and its first word is not a
/// variable assignment. Any of `; & | ` $( ${ > < ( )`, a newline, a quote, a glob or a
/// non-ASCII character disqualifies it.
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::plain_shell_words;
///
/// assert_eq!(plain_shell_words("cargo test --quiet"), Some(vec!["cargo", "test", "--quiet"]));
/// assert_eq!(plain_shell_words("cargo test && curl evil"), None);
/// assert_eq!(plain_shell_words("FOO=1 cargo test"), None);
/// assert_eq!(plain_shell_words("echo $(id)"), None);
/// ```
pub fn plain_shell_words(command: &str) -> Option<Vec<&str>> {
    if !command.chars().all(|c| c == ' ' || c == '\t' || is_word_char(c)) {
        return None;
    }
    let words: Vec<&str> = command.split([' ', '\t']).filter(|word| !word.is_empty()).collect();
    let first = words.first()?;
    if first.contains('=') {
        return None;
    }
    Some(words)
}

/// Whether `command` is plain words whose first `tokens.len()` words equal `tokens` (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::shell_prefix_matches;
///
/// let tokens = ["cargo".to_string(), "test".to_string()];
/// assert!(shell_prefix_matches("cargo  test --quiet", &tokens));
/// assert!(!shell_prefix_matches("cargo testx", &tokens));
/// assert!(!shell_prefix_matches("cargo test\nrm -rf /", &tokens));
/// ```
pub fn shell_prefix_matches(command: &str, tokens: &[String]) -> bool {
    if tokens.is_empty()
        || !tokens.iter().all(|token| plain_shell_words(token).is_some_and(|w| w.len() == 1))
    {
        return false;
    }
    plain_shell_words(command).is_some_and(|words| {
        words.len() >= tokens.len() && words.iter().zip(tokens).all(|(word, token)| word == token)
    })
}

/// Directory names whose contents are protected at any depth (spec 3.1 §9.1).
const PROTECTED_DIRS: &[&str] = &[".git", ".adk", ".skills", ".claude"];
/// File names protected at any depth: instruction files, memory, and files a shell or
/// hook manager executes (spec 3.1 §9.1).
const PROTECTED_FILES: &[&str] =
    &["agents.md", "agent.md", "claude.md", "memory.md", ".envrc", "lefthook.yml", "lefthook.yaml"];

/// Paths protected from the workspace root, the extended set every harness protects
/// (spec 3.4 §7.5, §10 P1): the evaluator, the governance and approval code, the harness's
/// own policy, CI, scripts, release and build configuration. `dir/**` covers the directory
/// and everything below it.
const ROOT_PROTECTED: &[&str] = &[
    "adk-eval/**",
    "adk-core/src/governance.rs",
    "adk-core/src/policy.rs",
    "adk-core/src/approval.rs",
    "adk-core/src/audit_log.rs",
    "adk-core/src/action_ledger.rs",
    "adk-core/src/spend.rs",
    "adk-core/src/budget.rs",
    "adk-harness/src/policy.rs",
    "adk-harness/src/approvals/**",
    "adk-harness/src/selfimprove/**",
    "adk-harness/tests/suite/**",
    ".github/**",
    "scripts/**",
    "xtask/**",
    "publish.sh",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "CODEOWNERS",
    ".adk/selfimprove/**",
];

/// Whether the folded `components` of a root-relative path are in [`ROOT_PROTECTED`].
fn root_protected(components: &[String]) -> bool {
    ROOT_PROTECTED.iter().any(|pattern| {
        let (prefix, subtree) = match pattern.strip_suffix("/**") {
            Some(prefix) => (prefix, true),
            None => (*pattern, false),
        };
        let parts: Vec<String> = prefix.split('/').map(fold).collect();
        components.len() >= parts.len()
            && components.iter().zip(&parts).all(|(component, part)| component == part)
            && (subtree || components.len() == parts.len())
    })
}

fn fold(component: &str) -> String {
    component.nfc().collect::<String>().to_lowercase()
}

/// Folds display text so wording that only looks different matches: compatibility
/// decomposition (NFKD) with accents and other combining marks dropped, case folded,
/// zero-width, bidirectional and other invisible controls removed, letters from other scripts
/// that look like Latin ones mapped to them, `0` and `1` read as `o` and `l`, apostrophes
/// unified and whitespace collapsed to single spaces.
///
/// It extends [`is_protected_path`]'s path folding to text a person reads: agent-drawn UI may
/// not show a decision's words (spec 3.3 §7.4 rule 5), whatever their spelling.
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::fold_display_text;
///
/// assert_eq!(fold_display_text("Allow once"), "allow once");
/// // Full-width letters, a zero-width space and a Cyrillic `о`.
/// assert_eq!(fold_display_text("Ａllow\u{200B} \u{043E}nce"), "allow once");
/// assert_eq!(fold_display_text("Confirm it\u{2019}s  you"), "confirm it's you");
/// assert_eq!(fold_display_text("D\u{0301}eny"), "deny");
/// // Musical and shorthand format controls draw nothing either.
/// assert_eq!(fold_display_text("Ap\u{1D173}pro\u{1BCA0}ved"), "approved");
/// ```
pub fn fold_display_text(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut space = false;
    for c in text.nfkd() {
        if unicode_normalization::char::is_combining_mark(c) || invisible(c) {
            continue;
        }
        if c.is_whitespace() {
            space = !folded.is_empty();
            continue;
        }
        if space {
            folded.push(' ');
            space = false;
        }
        for lower in c.to_lowercase() {
            folded.push(look_alike(lower));
        }
    }
    folded
}

/// Characters that draw nothing or reorder what is drawn: Unicode's whole
/// Default_Ignorable_Code_Point set — zero-width characters, the soft hyphen, bidirectional
/// controls, fillers, variation selectors, shorthand and musical format controls, tags and
/// the ranges reserved as ignorable — plus the interlinear annotation controls.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// The Latin letter a lowercase character is commonly mistaken for, or the character.
fn look_alike(c: char) -> char {
    match c {
        // Cyrillic.
        'а' => 'a',
        'в' => 'b',
        'с' | 'ϲ' => 'c',
        'ԁ' => 'd',
        'е' | 'ё' | 'є' => 'e',
        'һ' => 'h',
        'і' | 'ї' | 'ı' | 'ɩ' | 'ι' => 'i',
        'ј' | 'ϳ' => 'j',
        'к' | 'κ' => 'k',
        'ӏ' | 'ℓ' | '1' | '|' => 'l',
        'м' => 'm',
        'п' | 'η' | 'ո' => 'n',
        'о' | 'ο' | 'օ' | 'σ' | '0' => 'o',
        'р' | 'ρ' => 'p',
        'ԛ' => 'q',
        'г' => 'r',
        'ѕ' => 's',
        'т' | 'τ' => 't',
        'υ' | 'ս' => 'u',
        'ν' | 'ѵ' => 'v',
        'ԝ' | 'ω' | 'ѡ' => 'w',
        'х' | 'χ' => 'x',
        'у' | 'ү' | 'γ' => 'y',
        'ɑ' | 'α' => 'a',
        'ɡ' => 'g',
        // Apostrophes and primes.
        '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{2032}' | '\u{02BC}' | '\u{FF07}' | '`'
        | '\u{00B4}' => '\'',
        other => other,
    }
}

/// Whether `path` (relative to the workspace root) is a protected path (spec 3.1 §9.2 I29).
///
/// Instruction, memory, hook and tool-configuration names are protected at any depth; the
/// extended set of spec 3.4 §7.5 (`Cargo.toml`, `Cargo.lock`, `.github/**`, `scripts/**`,
/// the evaluator and the governance sources, and more) from the workspace root. Matching
/// is case-folded and Unicode-normalized (NFC), so `Agents.MD`, `.GIT/config` and
/// decomposed spellings are protected too.
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::is_protected_path;
///
/// assert!(is_protected_path("AGENTS.md"));
/// assert!(is_protected_path("crates/x/Agents.MD"));
/// assert!(is_protected_path(".git/hooks/pre-commit"));
/// assert!(is_protected_path(".cargo/config.toml"));
/// assert!(is_protected_path("Cargo.toml"));
/// assert!(is_protected_path(".github/workflows/ci.yml"));
/// assert!(!is_protected_path("crates/x/Cargo.toml"));
/// assert!(!is_protected_path("src/lib.rs"));
/// ```
pub fn is_protected_path(path: &str) -> bool {
    let components: Vec<String> =
        path.split(['/', '\\']).filter(|part| !part.is_empty() && *part != ".").map(fold).collect();
    if components.iter().any(|part| PROTECTED_DIRS.contains(&part.as_str())) {
        return true;
    }
    if components.last().is_some_and(|last| PROTECTED_FILES.contains(&last.as_str()))
        || root_protected(&components)
    {
        return true;
    }
    components
        .windows(2)
        .any(|pair| pair[0] == ".cargo" && (pair[1] == "config.toml" || pair[1] == "config"))
}

/// Whether `raw`, resolved against `root`, is a protected path as written or at the
/// target a write would reach (spec 3.1 §9.2 I11, I29).
///
/// The deepest existing ancestor of the lexically normalized path is canonicalized,
/// which resolves every symlink on the way, and the components that do not exist yet are
/// appended to it: that is the file a write creates or truncates. The path is protected
/// when either spelling is. A path that cannot be resolved — a NUL, a dangling or
/// looping symlink on the way, or a target outside the root — counts as protected.
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::is_protected_target;
///
/// let dir = tempfile::tempdir().unwrap();
/// let root = std::fs::canonicalize(dir.path()).unwrap();
/// std::fs::create_dir(root.join("docs")).unwrap();
/// # #[cfg(unix)]
/// std::os::unix::fs::symlink("../AGENTS.md", root.join("docs/notes.md")).unwrap();
/// std::fs::write(root.join("AGENTS.md"), "rules").unwrap();
/// assert!(is_protected_target(&root, "AGENTS.md"));
/// # #[cfg(unix)]
/// assert!(is_protected_target(&root, "docs/notes.md"));
/// assert!(!is_protected_target(&root, "docs/other.md"));
/// ```
pub fn is_protected_target(root: &Path, raw: &str) -> bool {
    if raw.is_empty() || raw.contains('\0') {
        return true;
    }
    let requested = Path::new(raw);
    let joined =
        if requested.is_absolute() { requested.to_path_buf() } else { root.join(requested) };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    let Ok(relative) = normalized.strip_prefix(root) else {
        return true;
    };
    if is_protected_path(&relative.to_string_lossy()) {
        return true;
    }
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut existing = normalized.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(canonical) => {
                let target = missing.iter().rev().fold(canonical, |path, name| path.join(name));
                return match target.strip_prefix(&canonical_root) {
                    Ok(relative) => is_protected_path(&relative.to_string_lossy()),
                    Err(_) => true,
                };
            }
            // An entry that exists but does not canonicalize is a dangling or looping
            // symlink: a write through it creates a target this check cannot see.
            Err(_) if std::fs::symlink_metadata(existing).is_ok() => return true,
            Err(_) => match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) if parent.starts_with(root) => {
                    missing.push(name.to_os_string());
                    existing = parent;
                }
                _ => return false,
            },
        }
    }
}

/// Lexically normalizes `path`: drops `.` components and repeated separators. `..` is
/// kept so callers can refuse it.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether the deepest existing ancestor of `path` resolves inside `root`.
fn contained_after_symlinks(root: &Path, path: &Path) -> bool {
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut existing = path;
    loop {
        match std::fs::canonicalize(existing) {
            Ok(canonical) => return canonical.starts_with(&canonical_root),
            Err(_) => {
                if std::fs::symlink_metadata(existing).is_ok() {
                    // A dangling or looping symlink.
                    return false;
                }
                match existing.parent() {
                    Some(parent) if parent.starts_with(root) => existing = parent,
                    _ => return true,
                }
            }
        }
    }
}

/// Whether `raw`, resolved against `root`, lies within `dir` and is not protected
/// (spec 3.1 §7.5 `PathWithin`, §9.2 I11).
///
/// Refuses any `..` component, a NUL, a path outside the root lexically or through a
/// symlink, and every path that is protected as written or at the target it resolves to
/// ([`is_protected_target`]).
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::path_within;
/// use std::path::Path;
///
/// let root = Path::new("/repo");
/// assert!(path_within(root, "src/lib.rs", "src"));
/// assert!(path_within(root, "./src//lib.rs", "."));
/// assert!(!path_within(root, "src/../AGENTS.md", "."));
/// assert!(!path_within(root, "docs/AGENTS.md", "."));
/// assert!(!path_within(root, "/etc/passwd", "."));
/// ```
pub fn path_within(root: &Path, raw: &str, dir: &str) -> bool {
    if raw.is_empty() || raw.contains('\0') || dir.contains('\0') {
        return false;
    }
    let has_parent = |text: &str| text.split(['/', '\\']).any(|part| part == "..");
    if has_parent(raw) || has_parent(dir) || Path::new(dir).is_absolute() {
        return false;
    }
    let requested = Path::new(raw);
    let joined =
        if requested.is_absolute() { requested.to_path_buf() } else { root.join(requested) };
    let normalized = lexical(&joined);
    let root = lexical(root);
    let Ok(relative) = normalized.strip_prefix(&root) else {
        return false;
    };
    if relative.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    if is_protected_path(&relative.to_string_lossy()) || is_protected_target(&root, raw) {
        return false;
    }
    if !contained_after_symlinks(&root, &normalized) {
        return false;
    }
    normalized.starts_with(lexical(&root.join(dir)))
}

/// Matches `name` against a glob where `*` matches any run of characters and `?` exactly (spec 3.1 §7.5)
/// one.
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::matcher::glob_matches;
///
/// assert!(glob_matches("mcp_*", "mcp_search"));
/// assert!(glob_matches("bash", "bash"));
/// assert!(!glob_matches("bash", "bash2"));
/// ```
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            backtrack = Some((p, n));
            p += 1;
        } else if let Some((star, matched)) = backtrack {
            p = star + 1;
            n = matched + 1;
            backtrack = Some((star, matched + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decomposed_and_cased_spellings_are_protected() {
        // "claude.md" is ASCII, so exercise NFC on a protected dir spelled with a
        // combining sequence-free name and on case.
        assert!(is_protected_path(".Claude/settings.json"));
        assert!(is_protected_path("MEMORY.MD"));
        assert!(is_protected_path("sub/.SKILLS/x/SKILL.md"));
        assert!(is_protected_path(".cargo\\config"));
        assert!(!is_protected_path("docs/agents.md.bak"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_protected_path_is_protected_and_never_within_a_rule() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("AGENTS.md"), "rules").unwrap();
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        std::fs::write(root.join(".git/config"), "[core]").unwrap();
        std::fs::create_dir(root.join("docs")).unwrap();
        symlink("../AGENTS.md", root.join("docs/notes.md")).unwrap();
        symlink(".git/config", root.join("settings.ini")).unwrap();
        symlink(".git", root.join("cfg")).unwrap();
        symlink("../CLAUDE.md", root.join("docs/dangling.md")).unwrap();
        symlink("loop.md", root.join("loop.md")).unwrap();
        symlink("docs", root.join("alias")).unwrap();

        for path in [
            "docs/notes.md",
            "settings.ini",
            "cfg/hooks/pre-commit",
            "cfg/new-dir/file",
            "docs/dangling.md",
            "loop.md",
            "alias/notes.md",
            "./docs/../docs/notes.md",
        ] {
            assert!(is_protected_target(&root, path), "{path}");
            assert!(!path_within(&root, path, "."), "{path}");
        }
        for path in ["docs/other.md", "alias/other.md", "src/new/lib.rs"] {
            assert!(!is_protected_target(&root, path), "{path}");
            assert!(path_within(&root, path, "."), "{path}");
        }
        assert!(is_protected_target(&root, "/etc/passwd"));
    }

    /// A folder rule over `notes`, matched against `tool` called with `args` as `kind`.
    fn folder_rule_matches(root: &Path, tool: &str, args: &Value, kind: ToolKind) -> bool {
        use super::super::rule::{OriginPin, RuleScope};
        use adk_core::SubjectKind;
        use adk_core::principal::SubjectRef;

        let origin = ToolOrigin::new("static", &serde_json::json!({ "name": tool }), None);
        let declarations = ["write_file", "edit_file", "apply_patch"]
            .iter()
            .map(|name| {
                let pinned = ToolOrigin::new("static", &serde_json::json!({ "name": name }), None);
                (name.to_string(), pinned.declaration_digest)
            })
            .collect();
        let rule = StandingRule {
            id: "folder".into(),
            scope: RuleScope::Session {
                app_name: "app".into(),
                user_id: "ana".into(),
                subject_kind: SubjectKind::Human,
                session_id: "s1".into(),
            },
            tool: "*".into(),
            origin: OriginPin::ExactEach { source: "static".into(), declarations },
            agent: None,
            predicates: vec![RulePredicate::WritesWithin { dir: "notes".into() }],
            decision: StandingDecision::Allow,
            created_by: SubjectRef::human("ana"),
            created_at: Utc::now(),
            expires_at: None,
            label: "file writes: within notes/".into(),
            source_fingerprint: None,
        };
        let acting = ActingContext::derived("app", "harness", "ana", SubjectKind::Human);
        let call = RuleCall {
            app_name: "app",
            user_id: "ana",
            session_id: "s1",
            acting: Some(&acting),
            tool_name: tool,
            args,
            origin: Some(&origin),
            kind,
            workspace_root: root,
        };
        evaluate_standing_rules(std::slice::from_ref(&rule), &call, Utc::now()).outcome
            == RuleOutcome::Allow { rule_id: "folder".into() }
    }

    #[test]
    fn a_folder_rule_covers_file_writes_in_the_folder_and_nothing_else() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let patch =
            |text: &str| json!({ "patch": format!("*** Begin Patch\n{text}\n*** End Patch") });

        for (tool, args) in [
            ("write_file", json!({ "path": "notes/new.md" })),
            ("write_file", json!({ "path": "./notes/deep/new.md" })),
            ("edit_file", json!({ "path": "notes/a.md" })),
            (
                "apply_patch",
                patch("*** Add File: notes/a.md\n+x\n*** Update File: notes/b.md\n@@\n-x\n+y"),
            ),
        ] {
            assert!(folder_rule_matches(&root, tool, &args, ToolKind::Edit), "{tool} {args}");
        }
        for (tool, args) in [
            ("write_file", json!({ "path": "elsewhere.md" })),
            ("write_file", json!({ "path": "notesx/a.md" })),
            ("write_file", json!({ "path": "notes/../a.md" })),
            ("write_file", json!({ "path": "/etc/passwd" })),
            ("write_file", json!({ "path": "notes/AGENTS.md" })),
            ("write_file", json!({ "path": "notes/.git/config" })),
            ("apply_patch", patch("*** Delete File: notes/a.md")),
            (
                "apply_patch",
                patch("*** Update File: notes/a.md\n*** Move to: notes/b.md\n@@\n-x\n+y"),
            ),
            ("apply_patch", patch("*** Add File: notes/a.md\n+x\n*** Add File: other/b.md\n+y")),
        ] {
            assert!(!folder_rule_matches(&root, tool, &args, ToolKind::Edit), "{tool} {args}");
        }
        // A symlink out of the folder, to a directory outside the workspace.
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("notes/out")).unwrap();
            let escape = json!({ "path": "notes/out/a.md" });
            assert!(!folder_rule_matches(&root, "write_file", &escape, ToolKind::Edit));
        }
        // The shell and other classes, whatever their arguments name.
        let shell = json!({ "command": "touch notes/a.md", "path": "notes/a.md" });
        assert!(!folder_rule_matches(
            &root,
            "bash",
            &shell,
            ToolKind::Shell(crate::Isolation::Host)
        ));
        let read = json!({ "path": "notes/a.md" });
        assert!(!folder_rule_matches(&root, "read_file", &read, ToolKind::Read));
        // A tool the rule does not pin, though it writes under the same name class.
        assert!(!folder_rule_matches(&root, "delete_file", &read, ToolKind::Edit));
    }

    #[test]
    fn the_extended_set_is_protected_from_the_root() {
        for path in [
            "Cargo.toml",
            "./cargo.LOCK",
            "rust-toolchain.toml",
            "CODEOWNERS",
            "publish.sh",
            "adk-eval",
            "adk-eval/src/lib.rs",
            "adk-core/src/governance.rs",
            "adk-core/src/policy.rs",
            "adk-core/src/approval.rs",
            "adk-core/src/audit_log.rs",
            "adk-core/src/action_ledger.rs",
            "adk-core/src/spend.rs",
            "adk-core/src/budget.rs",
            "adk-harness/src/policy.rs",
            "adk-harness/src/approvals/rule.rs",
            "adk-harness/src/selfimprove/gate.rs",
            "adk-harness/tests/suite/a.rs",
            ".github/workflows/ci.yml",
            "scripts/check.sh",
            "xtask/src/main.rs",
            ".adk/selfimprove/defects.json",
        ] {
            assert!(is_protected_path(path), "{path}");
        }
        for path in [
            "crates/x/Cargo.toml",
            "adk-core/src/lib.rs",
            "adk-core/src/policy.rs.bak",
            "adk-harness/src/harness.rs",
            "adk-harness/tests/other.rs",
            "src/policy.rs",
            "docs/scripts/x.md",
            "adk-evaluation/x.rs",
        ] {
            assert!(!is_protected_path(path), "{path}");
        }
    }

    #[test]
    fn metacharacters_disqualify_a_command() {
        for command in [
            "ls;id",
            "ls & id",
            "ls | id",
            "ls `id`",
            "ls $(id)",
            "ls ${IFS}id",
            "ls > f",
            "ls < f",
            "(ls)",
            "ls\nid",
            "ls 'x'",
            "ls \"x\"",
            "ls *",
            "ls ~",
            "ls\u{ff1b}id",
            "ls\0",
            "ls \\x",
            "A=1 ls",
            "",
            "   ",
        ] {
            assert_eq!(plain_shell_words(command), None, "{command:?}");
        }
    }
}
