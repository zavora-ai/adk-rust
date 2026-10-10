//! Tool execution policy.
//!
//! A [`ToolPolicy`] decides, for every tool call, whether it may execute, must not
//! execute, or may execute only after a person approves it. Agents consult the run's
//! policy ([`RunConfig::tool_policy`](crate::RunConfig::tool_policy)) on the final
//! arguments of each call, after every plugin and callback rewrite and immediately
//! before the tool runs.
//!
//! [`DeclarativePolicy`] is a rule list matched in order on the tool name and the
//! call's arguments. Its default decision is **deny**: a tool that no rule permits
//! does not run.

use async_trait::async_trait;
use serde_json::Value;

/// The outcome of evaluating a [`ToolPolicy`] for one tool call.
///
/// # Example
///
/// ```rust
/// use adk_core::PolicyDecision;
///
/// let strict = PolicyDecision::Allow.stricter(PolicyDecision::require_approval("large"));
/// assert_eq!(strict, PolicyDecision::require_approval("large"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// The call may execute.
    Allow,
    /// The call must not execute. `reason` is reported to the model as the call's error.
    Deny {
        /// Why the call was refused.
        reason: String,
    },
    /// The call may execute only after a person approves it.
    RequireApproval {
        /// Why approval is needed, shown with the confirmation request.
        reason: String,
    },
}

impl PolicyDecision {
    /// A [`PolicyDecision::Deny`] with `reason`.
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny { reason: reason.into() }
    }

    /// A [`PolicyDecision::RequireApproval`] with `reason`.
    pub fn require_approval(reason: impl Into<String>) -> Self {
        Self::RequireApproval { reason: reason.into() }
    }

    /// Returns the stricter of `self` and `other`.
    ///
    /// `Deny` is stricter than `RequireApproval`, which is stricter than `Allow`. On a
    /// tie, `self` is kept.
    #[must_use]
    pub fn stricter(self, other: Self) -> Self {
        if other.severity() > self.severity() { other } else { self }
    }

    fn severity(&self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::RequireApproval { .. } => 1,
            Self::Deny { .. } => 2,
        }
    }
}

/// The facts a [`ToolPolicy`] decides on for one tool call.
///
/// Every field is set by the framework at the call site from the call it is about to
/// execute; the arguments are the final ones, after plugin and callback rewrites.
///
/// # Example
///
/// ```rust
/// use adk_core::ToolPolicyRequest;
/// use serde_json::json;
///
/// let request = ToolPolicyRequest::new("transfer", json!({ "amount": 25 }))
///     .with_agent_name("treasurer")
///     .with_identity("payments", "user-1", "session-1")
///     .with_invocation_id("inv-1");
/// assert_eq!(request.tool_name, "transfer");
/// assert!(!request.read_only);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct ToolPolicyRequest {
    /// Name of the tool the model called.
    pub tool_name: String,
    /// Final arguments the tool would execute with.
    pub args: Value,
    /// Whether the tool declares itself read-only ([`Tool::is_read_only`](crate::Tool::is_read_only)).
    pub read_only: bool,
    /// The agent making the call.
    pub agent_name: String,
    /// Application the run belongs to.
    pub app_name: String,
    /// User the run belongs to.
    pub user_id: String,
    /// Session the run belongs to.
    pub session_id: String,
    /// Invocation the call happened in.
    pub invocation_id: String,
}

impl ToolPolicyRequest {
    /// Creates a request for a call to `tool_name` with `args` and no identity attached.
    pub fn new(tool_name: impl Into<String>, args: Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            args,
            read_only: false,
            agent_name: String::new(),
            app_name: String::new(),
            user_id: String::new(),
            session_id: String::new(),
            invocation_id: String::new(),
        }
    }

    /// Marks the called tool as read-only.
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Attaches the calling agent's name.
    #[must_use]
    pub fn with_agent_name(mut self, agent_name: impl Into<String>) -> Self {
        self.agent_name = agent_name.into();
        self
    }

    /// Attaches the run's identity.
    #[must_use]
    pub fn with_identity(
        mut self,
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        self.app_name = app_name.into();
        self.user_id = user_id.into();
        self.session_id = session_id.into();
        self
    }

    /// Attaches the invocation the call happened in.
    #[must_use]
    pub fn with_invocation_id(mut self, invocation_id: impl Into<String>) -> Self {
        self.invocation_id = invocation_id.into();
        self
    }
}

/// Decides whether a tool call may execute.
///
/// Set one on a run with [`RunConfigBuilder::tool_policy`](crate::RunConfigBuilder::tool_policy)
/// or on a runner with its builder's `tool_policy`. The policy travels with the run's
/// [`RunConfig`](crate::RunConfig), so transfer targets and agents behind an agent tool
/// are governed by it too.
///
/// # Example
///
/// ```rust
/// use adk_core::{PolicyDecision, ToolPolicy, ToolPolicyRequest, async_trait};
///
/// /// Refuses everything outside business hours.
/// #[derive(Debug)]
/// struct OfficeHours {
///     open: bool,
/// }
///
/// #[async_trait]
/// impl ToolPolicy for OfficeHours {
///     async fn evaluate(&self, _request: &ToolPolicyRequest) -> PolicyDecision {
///         if self.open { PolicyDecision::Allow } else { PolicyDecision::deny("closed") }
///     }
/// }
/// ```
#[async_trait]
pub trait ToolPolicy: std::fmt::Debug + Send + Sync {
    /// Decides one call.
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision;
}

/// A predicate over one argument, addressed by a JSON pointer (RFC 6901).
///
/// A predicate whose pointer does not resolve, or resolves to a value of the wrong
/// type, does not hold, so the rule it belongs to does not match.
///
/// # Example
///
/// ```rust
/// use adk_core::ArgPredicate;
/// use serde_json::json;
///
/// let small = ArgPredicate::at_most("/amount", 100.0);
/// assert!(small.holds(&json!({ "amount": 99.5 })));
/// assert!(!small.holds(&json!({ "amount": 101 })));
/// assert!(!small.holds(&json!({ "amount": "99" })));
///
/// let docs = ArgPredicate::domain_in("/url", ["docs.rs", "*.example.com"]);
/// assert!(docs.holds(&json!({ "url": "https://docs.rs/serde" })));
/// assert!(docs.holds(&json!({ "url": "https://api.example.com/v1" })));
/// assert!(!docs.holds(&json!({ "url": "https://docs.rs@evil.test/" })));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum ArgPredicate {
    /// The argument equals `value`.
    Equals {
        /// JSON pointer to the argument.
        pointer: String,
        /// The required value.
        value: Value,
    },
    /// The argument equals one of `values`.
    InSet {
        /// JSON pointer to the argument.
        pointer: String,
        /// The permitted values.
        values: Vec<Value>,
    },
    /// The argument is a JSON number no greater than `max`.
    AtMost {
        /// JSON pointer to the argument.
        pointer: String,
        /// The largest permitted value.
        max: f64,
    },
    /// The argument is a string starting with `prefix`.
    ///
    /// The comparison is lexical. For filesystem paths, where `..` segments defeat a
    /// prefix, screen the call with a path guardrail as well.
    StartsWith {
        /// JSON pointer to the argument.
        pointer: String,
        /// The required prefix.
        prefix: String,
    },
    /// The argument is an `http` or `https` URL whose host is in `domains`.
    ///
    /// An entry matches its host exactly; an entry written `*.example.com` matches any
    /// subdomain of `example.com` but not `example.com` itself. A URL with user
    /// information, a backslash, percent-encoding, or any character outside
    /// `[a-z0-9.-]` in its host does not match, so a URL that parsers could read two
    /// ways is refused.
    DomainIn {
        /// JSON pointer to the argument.
        pointer: String,
        /// The permitted hosts.
        domains: Vec<String>,
    },
}

impl ArgPredicate {
    /// The argument at `pointer` equals `value`.
    pub fn equals(pointer: impl Into<String>, value: Value) -> Self {
        Self::Equals { pointer: pointer.into(), value }
    }

    /// The argument at `pointer` equals one of `values`.
    pub fn in_set(pointer: impl Into<String>, values: impl IntoIterator<Item = Value>) -> Self {
        Self::InSet { pointer: pointer.into(), values: values.into_iter().collect() }
    }

    /// The argument at `pointer` is a number no greater than `max`.
    pub fn at_most(pointer: impl Into<String>, max: f64) -> Self {
        Self::AtMost { pointer: pointer.into(), max }
    }

    /// The argument at `pointer` is a string starting with `prefix`.
    pub fn starts_with(pointer: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self::StartsWith { pointer: pointer.into(), prefix: prefix.into() }
    }

    /// The argument at `pointer` is an `http(s)` URL whose host is in `domains`.
    pub fn domain_in(
        pointer: impl Into<String>,
        domains: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::DomainIn {
            pointer: pointer.into(),
            domains: domains.into_iter().map(|d| d.into().to_ascii_lowercase()).collect(),
        }
    }

    /// Whether the predicate holds for `args`.
    pub fn holds(&self, args: &Value) -> bool {
        match self {
            Self::Equals { pointer, value } => args.pointer(pointer) == Some(value),
            Self::InSet { pointer, values } => {
                args.pointer(pointer).is_some_and(|actual| values.contains(actual))
            }
            Self::AtMost { pointer, max } => {
                args.pointer(pointer).and_then(Value::as_f64).is_some_and(|actual| actual <= *max)
            }
            Self::StartsWith { pointer, prefix } => args
                .pointer(pointer)
                .and_then(Value::as_str)
                .is_some_and(|actual| actual.starts_with(prefix.as_str())),
            Self::DomainIn { pointer, domains } => args
                .pointer(pointer)
                .and_then(Value::as_str)
                .and_then(url_host)
                .is_some_and(|host| domains.iter().any(|domain| domain_matches(domain, &host))),
        }
    }
}

/// Extracts the host of an `http` or `https` URL, refusing anything ambiguous.
///
/// Parsers disagree about backslashes, user information, percent-encoded hosts, and
/// embedded whitespace, so any of those yields `None` rather than a host that a client
/// might resolve differently.
fn url_host(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    if rest[end..].starts_with('\\') {
        return None;
    }
    let authority = &rest[..end];
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        Some(_) => return None,
        None => authority,
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    let valid = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && !host.split('.').any(str::is_empty);
    valid.then(|| host.to_string())
}

fn domain_matches(domain: &str, host: &str) -> bool {
    match domain.strip_prefix("*.") {
        Some(parent) => host.strip_suffix(parent).is_some_and(|sub| sub.ends_with('.')),
        None => domain == host,
    }
}

/// Matches `name` against a glob where `*` matches any run of characters and `?`
/// matches exactly one.
fn glob_matches(pattern: &str, name: &str) -> bool {
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

/// One rule of a [`DeclarativePolicy`].
///
/// A rule matches a call when its tool-name glob matches, its read-only condition (if
/// any) holds, and every argument predicate holds. The first matching rule decides.
///
/// # Example
///
/// ```rust
/// use adk_core::{ArgPredicate, PolicyRule, ToolPolicyRequest};
/// use serde_json::json;
///
/// let rule = PolicyRule::allow("transfer_*").when(ArgPredicate::at_most("/amount", 50.0));
/// assert!(rule.matches(&ToolPolicyRequest::new("transfer_usd", json!({ "amount": 20 }))));
/// assert!(!rule.matches(&ToolPolicyRequest::new("transfer_usd", json!({ "amount": 80 }))));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyRule {
    tool_pattern: String,
    read_only: Option<bool>,
    predicates: Vec<ArgPredicate>,
    decision: PolicyDecision,
}

impl PolicyRule {
    /// A rule that decides `decision` for calls to tools matching `tool_pattern`.
    pub fn new(tool_pattern: impl Into<String>, decision: PolicyDecision) -> Self {
        Self {
            tool_pattern: tool_pattern.into(),
            read_only: None,
            predicates: Vec::new(),
            decision,
        }
    }

    /// A rule allowing calls to tools matching `tool_pattern`.
    pub fn allow(tool_pattern: impl Into<String>) -> Self {
        Self::new(tool_pattern, PolicyDecision::Allow)
    }

    /// A rule refusing calls to tools matching `tool_pattern`.
    pub fn deny(tool_pattern: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(tool_pattern, PolicyDecision::deny(reason))
    }

    /// A rule requiring approval for calls to tools matching `tool_pattern`.
    pub fn require_approval(tool_pattern: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(tool_pattern, PolicyDecision::require_approval(reason))
    }

    /// Adds an argument predicate; every predicate must hold for the rule to match.
    #[must_use]
    pub fn when(mut self, predicate: ArgPredicate) -> Self {
        self.predicates.push(predicate);
        self
    }

    /// Restricts the rule to read-only tools.
    #[must_use]
    pub fn read_only_tools(mut self) -> Self {
        self.read_only = Some(true);
        self
    }

    /// The decision the rule makes when it matches.
    pub fn decision(&self) -> &PolicyDecision {
        &self.decision
    }

    /// Whether the rule matches `request`.
    pub fn matches(&self, request: &ToolPolicyRequest) -> bool {
        glob_matches(&self.tool_pattern, &request.tool_name)
            && self.read_only.is_none_or(|read_only| read_only == request.read_only)
            && self.predicates.iter().all(|predicate| predicate.holds(&request.args))
    }
}

/// A rule list matched in order, with a default decision for calls no rule matches.
///
/// The default decision is **deny**, so a tool that no rule permits does not run.
/// Build one with [`DeclarativePolicy::builder`].
///
/// # Example
///
/// ```rust
/// use adk_core::{ArgPredicate, DeclarativePolicy, PolicyDecision, PolicyRule, ToolPolicyRequest};
/// use serde_json::json;
///
/// let policy = DeclarativePolicy::builder()
///     .allow_read_only()
///     .rule(PolicyRule::allow("transfer").when(ArgPredicate::at_most("/amount", 100.0)))
///     .rule(PolicyRule::require_approval("transfer", "transfers over 100 need approval"))
///     .build();
///
/// let small = ToolPolicyRequest::new("transfer", json!({ "amount": 40 }));
/// let large = ToolPolicyRequest::new("transfer", json!({ "amount": 4000 }));
/// let unlisted = ToolPolicyRequest::new("delete_account", json!({}));
///
/// assert_eq!(policy.decide(&small), PolicyDecision::Allow);
/// assert!(matches!(policy.decide(&large), PolicyDecision::RequireApproval { .. }));
/// assert!(matches!(policy.decide(&unlisted), PolicyDecision::Deny { .. }));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct DeclarativePolicy {
    rules: Vec<PolicyRule>,
    default_decision: Option<PolicyDecision>,
}

impl DeclarativePolicy {
    /// Starts a policy with no rules and a default decision of deny.
    pub fn builder() -> DeclarativePolicyBuilder {
        DeclarativePolicyBuilder::default()
    }

    /// The rules, in match order.
    pub fn rules(&self) -> &[PolicyRule] {
        &self.rules
    }

    /// Decides one call: the first matching rule's decision, else the default.
    pub fn decide(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        if let Some(rule) = self.rules.iter().find(|rule| rule.matches(request)) {
            return rule.decision.clone();
        }
        self.default_decision.clone().unwrap_or_else(|| {
            PolicyDecision::deny(format!(
                "no policy rule permits tool '{}'; it is denied by default",
                request.tool_name
            ))
        })
    }
}

#[async_trait]
impl ToolPolicy for DeclarativePolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.decide(request)
    }
}

/// Builder for [`DeclarativePolicy`].
#[derive(Debug, Clone, Default)]
pub struct DeclarativePolicyBuilder {
    rules: Vec<PolicyRule>,
    default_decision: Option<PolicyDecision>,
}

impl DeclarativePolicyBuilder {
    /// Appends a rule. Rules are matched in the order they are added.
    #[must_use]
    pub fn rule(mut self, rule: PolicyRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Appends a rule allowing calls to tools matching `tool_pattern`.
    #[must_use]
    pub fn allow(self, tool_pattern: impl Into<String>) -> Self {
        self.rule(PolicyRule::allow(tool_pattern))
    }

    /// Appends a rule refusing calls to tools matching `tool_pattern`.
    #[must_use]
    pub fn deny(self, tool_pattern: impl Into<String>, reason: impl Into<String>) -> Self {
        self.rule(PolicyRule::deny(tool_pattern, reason))
    }

    /// Appends a rule requiring approval for calls to tools matching `tool_pattern`.
    #[must_use]
    pub fn require_approval(
        self,
        tool_pattern: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        self.rule(PolicyRule::require_approval(tool_pattern, reason))
    }

    /// Appends a rule allowing every read-only tool.
    ///
    /// Rules added before it still take precedence, so a read-only tool can be denied
    /// by name ahead of this rule.
    #[must_use]
    pub fn allow_read_only(self) -> Self {
        self.rule(PolicyRule::allow("*").read_only_tools())
    }

    /// Sets the decision for calls no rule matches. The default is deny.
    #[must_use]
    pub fn default_decision(mut self, decision: PolicyDecision) -> Self {
        self.default_decision = Some(decision);
        self
    }

    /// Builds the policy.
    pub fn build(self) -> DeclarativePolicy {
        DeclarativePolicy { rules: self.rules, default_decision: self.default_decision }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn glob_matching() {
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("send_*", "send_email"));
        assert!(!glob_matches("send_*", "resend_email"));
        assert!(glob_matches("*_file", "delete_file"));
        assert!(glob_matches("get_?", "get_x"));
        assert!(!glob_matches("get_?", "get_xy"));
        assert!(glob_matches("a*b*c", "aXXbYYc"));
        assert!(!glob_matches("a*b*c", "aXXbYY"));
        assert!(glob_matches("exact", "exact"));
        assert!(!glob_matches("exact", "exactly"));
    }

    #[test]
    fn url_hosts_refuse_ambiguous_forms() {
        assert_eq!(url_host("https://Docs.RS/serde").as_deref(), Some("docs.rs"));
        assert_eq!(url_host("http://example.com:8080/x").as_deref(), Some("example.com"));
        assert_eq!(url_host("https://example.com.").as_deref(), Some("example.com"));
        assert_eq!(url_host("https://example.com?q=1").as_deref(), Some("example.com"));
        assert_eq!(url_host("https://example.com#frag").as_deref(), Some("example.com"));
        for ambiguous in [
            "https://allowed.com@evil.test/",
            "https://evil.test\\@allowed.com",
            "https://allowed.com\\.evil.test",
            "https://%61llowed.com/",
            "https://[::1]/",
            "https://allowed.com:/",
            "https://allowed.com:80x/",
            "https://allowed.com\t.evil.test",
            "https://allowed..com",
            "https:///path",
            "ftp://allowed.com/",
            "allowed.com/path",
            " https://allowed.com",
        ] {
            assert_eq!(url_host(ambiguous), None, "{ambiguous}");
        }
    }

    #[test]
    fn domain_patterns() {
        assert!(domain_matches("example.com", "example.com"));
        assert!(!domain_matches("example.com", "api.example.com"));
        assert!(domain_matches("*.example.com", "api.example.com"));
        assert!(!domain_matches("*.example.com", "example.com"));
        assert!(!domain_matches("*.example.com", "badexample.com"));
    }

    #[test]
    fn predicates_fail_closed_on_missing_or_mistyped_arguments() {
        let args = json!({ "amount": "5", "nested": { "region": "eu" } });
        assert!(!ArgPredicate::at_most("/amount", 10.0).holds(&args));
        assert!(!ArgPredicate::at_most("/missing", 10.0).holds(&args));
        assert!(ArgPredicate::equals("/nested/region", json!("eu")).holds(&args));
        assert!(ArgPredicate::in_set("/nested/region", [json!("us"), json!("eu")]).holds(&args));
        assert!(!ArgPredicate::in_set("/nested/region", [json!("us")]).holds(&args));
        assert!(!ArgPredicate::starts_with("/amount", "6").holds(&args));
        assert!(!ArgPredicate::domain_in("/nested", ["eu"]).holds(&args));
    }

    #[test]
    fn rules_match_in_order_and_unlisted_tools_are_denied() {
        let policy = DeclarativePolicy::builder()
            .deny("lookup_secret", "secrets are off limits")
            .allow_read_only()
            .rule(PolicyRule::allow("fetch").when(ArgPredicate::domain_in("/url", ["docs.rs"])))
            .build();

        let read = |name: &str| ToolPolicyRequest::new(name, json!({})).with_read_only(true);
        assert_eq!(policy.decide(&read("search")), PolicyDecision::Allow);
        assert_eq!(
            policy.decide(&read("lookup_secret")),
            PolicyDecision::deny("secrets are off limits")
        );
        assert_eq!(
            policy.decide(&ToolPolicyRequest::new("fetch", json!({ "url": "https://docs.rs" }))),
            PolicyDecision::Allow
        );
        assert!(matches!(
            policy.decide(&ToolPolicyRequest::new("fetch", json!({ "url": "https://evil.test" }))),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            policy.decide(&ToolPolicyRequest::new("delete", json!({}))),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn the_default_decision_can_be_replaced() {
        let policy = DeclarativePolicy::builder()
            .default_decision(PolicyDecision::require_approval("unlisted"))
            .build();
        assert_eq!(
            policy.decide(&ToolPolicyRequest::new("anything", json!({}))),
            PolicyDecision::require_approval("unlisted")
        );
    }

    #[test]
    fn stricter_orders_decisions() {
        let allow = PolicyDecision::Allow;
        let approve = PolicyDecision::require_approval("a");
        let deny = PolicyDecision::deny("d");
        assert_eq!(allow.clone().stricter(approve.clone()), approve);
        assert_eq!(approve.clone().stricter(deny.clone()), deny);
        assert_eq!(deny.clone().stricter(allow), deny);
    }
}
