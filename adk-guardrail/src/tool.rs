//! Guardrails for tool calls.
//!
//! [`Guardrail`] validates [`Content`](adk_core::Content) — a user message or a
//! model response. It never sees a tool call, so it cannot express "this tool may run, but not
//! with these arguments." That left argument-level policy with nowhere to live:
//! [`ToolConfirmationPolicy`](adk_core::ToolConfirmationPolicy) decides per *tool name*, and a
//! plugin short-circuit is a general-purpose hook rather than a stated policy.
//!
//! [`ToolGuardrail`] closes that gap. It runs before a tool executes, sees the tool name and the
//! arguments, and can allow, deny, or revise. After the tool runs, it sees the result too
//! ([`ToolGuardrail::validate_result`]) and can deliver, rewrite, or withhold it before the model
//! and the session see it.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;

use crate::{Guardrail, GuardrailResult, Severity};

/// Outcome of validating a tool call.
#[derive(Debug, Clone)]
pub enum ToolGuardrailResult {
    /// The call may proceed unchanged.
    Allow,
    /// The call is refused. The tool does not run.
    Deny {
        /// Why the call was refused. Surfaced to the model so it can adjust.
        reason: String,
        /// How serious the violation is.
        severity: Severity,
    },
    /// The call may proceed, with these arguments instead.
    ///
    /// Use for narrowing rather than broadening — clamping a limit, forcing a dry-run flag,
    /// dropping a field the caller should not set.
    ReviseArgs {
        /// Arguments the tool is invoked with.
        args: Value,
        /// Why the arguments were changed.
        reason: String,
    },
}

impl ToolGuardrailResult {
    /// Refuses the call.
    pub fn deny(reason: impl Into<String>, severity: Severity) -> Self {
        Self::Deny { reason: reason.into(), severity }
    }

    /// Allows the call with replaced arguments.
    pub fn revise(args: Value, reason: impl Into<String>) -> Self {
        Self::ReviseArgs { args, reason: reason.into() }
    }

    /// Whether the call may proceed.
    pub fn is_allowed(&self) -> bool {
        !matches!(self, Self::Deny { .. })
    }
}

/// Outcome of validating a tool's result.
#[derive(Debug, Clone)]
pub enum ToolResultGuardrailResult {
    /// The result is delivered unchanged.
    Allow,
    /// The result is withheld. The model is told the tool ran and why its result is withheld.
    Withhold {
        /// Why the result was withheld. Surfaced to the model, so it must not quote the result.
        reason: String,
        /// How serious the violation is.
        severity: Severity,
    },
    /// The result is delivered as `result` instead, such as with personal data redacted.
    Revise {
        /// The result the model and the session see.
        result: Value,
        /// Why the result was changed.
        reason: String,
    },
}

impl ToolResultGuardrailResult {
    /// Withholds the result.
    pub fn withhold(reason: impl Into<String>, severity: Severity) -> Self {
        Self::Withhold { reason: reason.into(), severity }
    }

    /// Delivers `result` in place of the tool's result.
    pub fn revise(result: Value, reason: impl Into<String>) -> Self {
        Self::Revise { result, reason: reason.into() }
    }
}

/// Validates a tool call before it executes.
///
/// # Example
///
/// ```rust
/// use adk_guardrail::{Severity, ToolGuardrail, ToolGuardrailResult};
/// use async_trait::async_trait;
/// use serde_json::Value;
///
/// /// Refuses a recursive delete regardless of which tool is asked to perform it.
/// struct NoRecursiveDelete;
///
/// #[async_trait]
/// impl ToolGuardrail for NoRecursiveDelete {
///     fn name(&self) -> &str {
///         "no-recursive-delete"
///     }
///
///     async fn validate_call(&self, _tool: &str, args: &Value) -> ToolGuardrailResult {
///         if args.to_string().contains("-rf") {
///             return ToolGuardrailResult::deny("recursive delete is not permitted", Severity::Critical);
///         }
///         ToolGuardrailResult::Allow
///     }
/// }
/// ```
#[async_trait]
pub trait ToolGuardrail: Send + Sync {
    /// Unique name, used in denial messages and logs.
    fn name(&self) -> &str;

    /// Validates a call to `tool_name` with `args`. Defaults to allowing every call, for a
    /// guardrail that screens only results.
    async fn validate_call(&self, _tool_name: &str, _args: &Value) -> ToolGuardrailResult {
        ToolGuardrailResult::Allow
    }

    /// Validates the result of a call to `tool_name` with `args`, before the model and the
    /// session see it. Defaults to delivering every result.
    ///
    /// The agent runs it on the result it is about to send, after its after-tool callbacks and
    /// plugins, and on a tool's error response too. `args` are the arguments the model sent.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_guardrail::{Severity, ToolGuardrail, ToolResultGuardrailResult};
    /// use async_trait::async_trait;
    /// use serde_json::Value;
    ///
    /// /// Withholds any result that carries a card number field.
    /// struct NoCardNumbers;
    ///
    /// #[async_trait]
    /// impl ToolGuardrail for NoCardNumbers {
    ///     fn name(&self) -> &str {
    ///         "no-card-numbers"
    ///     }
    ///
    ///     async fn validate_result(
    ///         &self,
    ///         _tool: &str,
    ///         _args: &Value,
    ///         result: &Value,
    ///     ) -> ToolResultGuardrailResult {
    ///         if result.get("card_number").is_some() {
    ///             return ToolResultGuardrailResult::withhold("the result carries a card number", Severity::High);
    ///         }
    ///         ToolResultGuardrailResult::Allow
    ///     }
    /// }
    /// ```
    async fn validate_result(
        &self,
        _tool_name: &str,
        _args: &Value,
        _result: &Value,
    ) -> ToolResultGuardrailResult {
        ToolResultGuardrailResult::Allow
    }

    /// Whether this guardrail applies to `tool_name`. Defaults to every tool.
    ///
    /// Prefer this over an early `Allow` inside
    /// [`validate_call`](Self::validate_call) — a guardrail that declares its scope can be skipped
    /// without being run.
    fn applies_to(&self, _tool_name: &str) -> bool {
        true
    }
}

/// What a [`ToolGuardrailSet`] decided about a call.
#[derive(Debug, Clone)]
pub enum ToolCallDecision {
    /// The call may proceed with these arguments, which may have been revised.
    Allow {
        /// Arguments to invoke the tool with.
        args: Value,
    },
    /// The call is refused.
    Deny {
        /// Guardrail that refused it.
        guardrail: String,
        /// Why.
        reason: String,
        /// How serious.
        severity: Severity,
    },
}

impl ToolCallDecision {
    /// Whether the call may proceed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }
}

/// What a [`ToolGuardrailSet`] decided about a tool's result.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolResultDecision {
    /// The result to deliver, which may have been revised.
    Deliver {
        /// The result the model and the session see.
        result: Value,
    },
    /// The result is withheld.
    Withhold {
        /// Guardrail that withheld it.
        guardrail: String,
        /// Why.
        reason: String,
        /// How serious.
        severity: Severity,
    },
}

/// A collection of [`ToolGuardrail`]s evaluated together.
///
/// # Example
///
/// ```rust
/// use adk_guardrail::{PathAllowList, Severity, ToolGuardrailSet};
///
/// let guardrails = ToolGuardrailSet::new().with(
///     PathAllowList::new("plist-paths", ["path"], ["/Users/me/Library/LaunchAgents"])
///         .on_tools(["plist_write"]),
/// );
///
/// assert_eq!(guardrails.guardrails().len(), 1);
/// ```
#[derive(Default)]
pub struct ToolGuardrailSet {
    guardrails: Vec<Arc<dyn ToolGuardrail>>,
}

impl ToolGuardrailSet {
    /// Creates an empty set.
    pub fn new() -> Self {
        Self { guardrails: Vec::new() }
    }

    /// Adds a guardrail.
    pub fn with(mut self, guardrail: impl ToolGuardrail + 'static) -> Self {
        self.guardrails.push(Arc::new(guardrail));
        self
    }

    /// Adds a pre-wrapped guardrail.
    pub fn with_arc(mut self, guardrail: Arc<dyn ToolGuardrail>) -> Self {
        self.guardrails.push(guardrail);
        self
    }

    /// The registered guardrails.
    pub fn guardrails(&self) -> &[Arc<dyn ToolGuardrail>] {
        &self.guardrails
    }

    /// Whether no guardrails have been added.
    pub fn is_empty(&self) -> bool {
        self.guardrails.is_empty()
    }

    /// Evaluates a call against every applicable guardrail.
    ///
    /// Guardrails run in order and revisions compose: a later guardrail sees the arguments an
    /// earlier one produced. Evaluation is sequential rather than parallel because a revision has
    /// to be visible to whatever runs next — parallel evaluation would make the outcome depend on
    /// completion order. The first denial stops evaluation, so a denied call is never revised and
    /// never reaches a later guardrail.
    pub async fn evaluate(&self, tool_name: &str, args: &Value) -> ToolCallDecision {
        let mut current = args.clone();

        for guardrail in &self.guardrails {
            if !guardrail.applies_to(tool_name) {
                continue;
            }

            match guardrail.validate_call(tool_name, &current).await {
                ToolGuardrailResult::Allow => {}
                ToolGuardrailResult::Deny { reason, severity } => {
                    // A reason can quote the arguments, such as a refused path, so it is
                    // returned to the caller but not logged.
                    tracing::warn!(
                        guardrail = guardrail.name(),
                        tool = tool_name,
                        ?severity,
                        "tool call denied by guardrail"
                    );
                    return ToolCallDecision::Deny {
                        guardrail: guardrail.name().to_string(),
                        reason,
                        severity,
                    };
                }
                ToolGuardrailResult::ReviseArgs { args: revised, reason: _ } => {
                    tracing::debug!(
                        guardrail = guardrail.name(),
                        tool = tool_name,
                        "tool call arguments revised by guardrail"
                    );
                    current = revised;
                }
            }
        }

        ToolCallDecision::Allow { args: current }
    }

    /// Evaluates a tool's result against every applicable guardrail.
    ///
    /// Guardrails run in order and revisions compose, as in [`evaluate`](Self::evaluate). The
    /// first guardrail to withhold the result stops evaluation.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_guardrail::{PiiRedactor, ResultTextGuardrail, ToolGuardrailSet, ToolResultDecision};
    /// use serde_json::json;
    ///
    /// # futures::executor::block_on(async {
    /// let guardrails = ToolGuardrailSet::new().with(ResultTextGuardrail::new(PiiRedactor::new()));
    /// let decision = guardrails
    ///     .evaluate_result("lookup", &json!({}), &json!({"ssn": "123-45-6789"}))
    ///     .await;
    /// assert_eq!(
    ///     decision,
    ///     ToolResultDecision::Deliver { result: json!({"ssn": "[SSN REDACTED]"}) }
    /// );
    /// # });
    /// ```
    pub async fn evaluate_result(
        &self,
        tool_name: &str,
        args: &Value,
        result: &Value,
    ) -> ToolResultDecision {
        let mut current = result.clone();

        for guardrail in &self.guardrails {
            if !guardrail.applies_to(tool_name) {
                continue;
            }

            match guardrail.validate_result(tool_name, args, &current).await {
                ToolResultGuardrailResult::Allow => {}
                ToolResultGuardrailResult::Withhold { reason, severity } => {
                    // The reason is returned to the caller, which sends it to the model, but
                    // is not logged.
                    tracing::warn!(
                        guardrail = guardrail.name(),
                        tool = tool_name,
                        ?severity,
                        "tool result withheld by guardrail"
                    );
                    return ToolResultDecision::Withhold {
                        guardrail: guardrail.name().to_string(),
                        reason,
                        severity,
                    };
                }
                // A reason can quote the result it replaced, so it is not logged.
                ToolResultGuardrailResult::Revise { result: revised, reason: _ } => {
                    tracing::debug!(
                        guardrail = guardrail.name(),
                        tool = tool_name,
                        "tool result revised by guardrail"
                    );
                    current = revised;
                }
            }
        }

        ToolResultDecision::Deliver { result: current }
    }
}

/// Screens results with [`ToolGuardrailSet::evaluate_result`], so a set can be handed to any
/// agent that accepts a [`ToolResultScreen`](adk_core::ToolResultScreen).
#[async_trait]
impl adk_core::ToolResultScreen for ToolGuardrailSet {
    async fn screen_result(
        &self,
        tool_name: &str,
        args: &Value,
        result: &Value,
    ) -> std::result::Result<Value, String> {
        match self.evaluate_result(tool_name, args, result).await {
            ToolResultDecision::Deliver { result } => Ok(result),
            ToolResultDecision::Withhold { guardrail, reason, severity } => {
                Err(format!("tool guardrail '{guardrail}' ({severity:?}): {reason}"))
            }
        }
    }
}

/// Applies a content [`Guardrail`] to every text value of a tool's result.
///
/// Each string in the result JSON, at any depth, is validated as one text part: a transform
/// replaces the string, so [`PiiRedactor`](crate::PiiRedactor) redacts a result field by field
/// and the result keeps its shape, and a failure above [`Severity::Low`] withholds the whole
/// result. Object keys are not screened.
///
/// # Example
///
/// ```rust
/// use adk_guardrail::{ContentFilter, PiiRedactor, ResultTextGuardrail, ToolGuardrailSet};
///
/// # fn main() -> Result<(), adk_guardrail::GuardrailError> {
/// let guardrails = ToolGuardrailSet::new()
///     .with(ResultTextGuardrail::new(PiiRedactor::new()))
///     .with(
///         ResultTextGuardrail::new(ContentFilter::blocked_keywords(vec!["internal only".into()])?)
///             .on_tools(["search_wiki"]),
///     );
/// # Ok(())
/// # }
/// ```
pub struct ResultTextGuardrail<G> {
    guardrail: G,
    tools: Option<Vec<String>>,
}

impl<G: Guardrail> ResultTextGuardrail<G> {
    /// Applies `guardrail` to the text of every tool's result.
    pub fn new(guardrail: G) -> Self {
        Self { guardrail, tools: None }
    }

    /// Restricts this guardrail to the named tools. Without this it applies to every tool.
    pub fn on_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    /// Screens every string in `value`, returning the failure that withholds the result.
    async fn screen(&self, value: &mut Value) -> std::result::Result<bool, (String, Severity)> {
        let mut changed = false;
        let mut pending = vec![value];
        while let Some(value) = pending.pop() {
            match value {
                Value::String(text) => {
                    let content = adk_core::Content::new("tool").with_text(text.as_str());
                    match self.guardrail.validate(&content).await {
                        GuardrailResult::Pass => {}
                        GuardrailResult::Fail { severity: Severity::Low, .. } => {
                            tracing::warn!(
                                guardrail.name = self.guardrail.name(),
                                "guardrail flagged a tool result at low severity; the result passes"
                            );
                        }
                        GuardrailResult::Fail { reason, severity } => {
                            return Err((reason, severity));
                        }
                        GuardrailResult::Transform { new_content, .. } => {
                            let screened: String =
                                new_content.parts.iter().filter_map(adk_core::Part::text).collect();
                            if screened != *text {
                                *text = screened;
                                changed = true;
                            }
                        }
                    }
                }
                Value::Array(items) => pending.extend(items.iter_mut()),
                Value::Object(fields) => pending.extend(fields.values_mut()),
                Value::Null | Value::Bool(_) | Value::Number(_) => {}
            }
        }
        Ok(changed)
    }
}

#[async_trait]
impl<G: Guardrail> ToolGuardrail for ResultTextGuardrail<G> {
    fn name(&self) -> &str {
        self.guardrail.name()
    }

    fn applies_to(&self, tool_name: &str) -> bool {
        match &self.tools {
            Some(tools) => tools.iter().any(|t| t == tool_name),
            None => true,
        }
    }

    async fn validate_result(
        &self,
        _tool_name: &str,
        _args: &Value,
        result: &Value,
    ) -> ToolResultGuardrailResult {
        let mut screened = result.clone();
        match self.screen(&mut screened).await {
            Ok(false) => ToolResultGuardrailResult::Allow,
            Ok(true) => ToolResultGuardrailResult::revise(
                screened,
                format!("{} rewrote the result's text", self.guardrail.name()),
            ),
            Err((reason, severity)) => ToolResultGuardrailResult::withhold(reason, severity),
        }
    }
}

/// Denies a call whose serialized arguments match a pattern.
///
/// The pattern is matched against the JSON encoding of the whole argument object, so it catches a
/// value wherever it appears rather than requiring the field to be named up front.
///
/// # Example
///
/// ```rust
/// use adk_guardrail::{DeniedArgumentPattern, Severity};
///
/// # fn main() -> Result<(), regex::Error> {
/// let guardrail = DeniedArgumentPattern::new("no-force-push", r"--force\b", Severity::High)?
///     .on_tools(["run_command"]);
/// # Ok(())
/// # }
/// ```
pub struct DeniedArgumentPattern {
    name: String,
    pattern: Regex,
    severity: Severity,
    tools: Option<Vec<String>>,
}

impl DeniedArgumentPattern {
    /// Creates a guardrail denying calls whose arguments match `pattern`.
    ///
    /// # Errors
    ///
    /// Returns an error if `pattern` is not a valid regular expression.
    pub fn new(
        name: impl Into<String>,
        pattern: &str,
        severity: Severity,
    ) -> std::result::Result<Self, regex::Error> {
        Ok(Self { name: name.into(), pattern: Regex::new(pattern)?, severity, tools: None })
    }

    /// Restricts this guardrail to the named tools. Without this it applies to every tool.
    pub fn on_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }
}

#[async_trait]
impl ToolGuardrail for DeniedArgumentPattern {
    fn name(&self) -> &str {
        &self.name
    }

    fn applies_to(&self, tool_name: &str) -> bool {
        match &self.tools {
            Some(tools) => tools.iter().any(|t| t == tool_name),
            None => true,
        }
    }

    async fn validate_call(&self, tool_name: &str, args: &Value) -> ToolGuardrailResult {
        if self.pattern.is_match(&args.to_string()) {
            return ToolGuardrailResult::deny(
                format!(
                    "arguments to `{tool_name}` match the denied pattern `{}`",
                    self.pattern.as_str()
                ),
                self.severity,
            );
        }
        ToolGuardrailResult::Allow
    }
}

/// Denies a call whose path-valued arguments fall outside a set of allowed roots.
///
/// Requires every named argument to be present and to be an absolute path contained by one of the
/// allowed roots. A call that omits one is denied, because the guardrail cannot vouch for a path it
/// was not shown; [`allow_missing`](Self::allow_missing) permits calls without it. Without
/// [`on_tools`](Self::on_tools) the guardrail applies to every tool, so a tool that takes no path
/// is denied too — scope it to the tools that take paths. Containment is compared by path component and after resolving the
/// allowed root and every existing candidate component, so string-prefix, dangling-symlink, and
/// resolved-symlink escapes are refused. Any path containing a `..` component is denied outright.
///
/// This is a preflight policy check, not a replacement for opening filesystem paths relative to a
/// trusted directory handle. A hostile process able to replace path components between validation
/// and tool execution can create a time-of-check/time-of-use race; filesystem tools operating
/// across such a trust boundary must still use platform secure-open primitives.
///
/// # Example
///
/// ```rust
/// use adk_guardrail::PathAllowList;
///
/// let guardrail = PathAllowList::new(
///     "launch-agents-only",
///     ["path"],
///     ["/Users/me/Library/LaunchAgents"],
/// )
/// .on_tools(["plist_write", "plist_read"]);
/// ```
pub struct PathAllowList {
    name: String,
    arg_names: Vec<String>,
    allowed_roots: Vec<PathBuf>,
    severity: Severity,
    tools: Option<Vec<String>>,
    allow_missing: bool,
}

impl PathAllowList {
    /// Creates a guardrail confining `arg_names` to `allowed_roots`.
    pub fn new<A, S, R, P>(name: impl Into<String>, arg_names: A, allowed_roots: R) -> Self
    where
        A: IntoIterator<Item = S>,
        S: Into<String>,
        R: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            name: name.into(),
            arg_names: arg_names.into_iter().map(Into::into).collect(),
            allowed_roots: allowed_roots.into_iter().map(Into::into).collect(),
            severity: Severity::Critical,
            tools: None,
            allow_missing: false,
        }
    }

    /// Sets the severity reported on denial. Defaults to [`Severity::Critical`].
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    /// Restricts this guardrail to the named tools. Without this it applies to every tool.
    ///
    /// A tool not named here is not checked: its calls pass this guardrail whatever their
    /// arguments. List every tool that takes one of the configured path arguments.
    pub fn on_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    /// Permits a call that omits a configured path argument.
    ///
    /// By default such a call is denied. Use this when the guardrail names arguments that only
    /// some of its tools take, such as `source` and `destination` beside `path`; every argument
    /// that is present is still checked.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_guardrail::PathAllowList;
    ///
    /// let guardrail = PathAllowList::new("workspace", ["path", "source", "destination"], ["/srv"])
    ///     .on_tools(["read_file", "move_file"])
    ///     .allow_missing();
    /// ```
    pub fn allow_missing(mut self) -> Self {
        self.allow_missing = true;
        self
    }

    /// Whether `candidate` is an absolute, traversal-free path inside an allowed root.
    fn is_permitted(&self, candidate: &str) -> bool {
        let path = Path::new(candidate);

        if !path.is_absolute() {
            return false;
        }

        // A `..` cannot be resolved for a path that may not exist, so it is refused rather than
        // normalized.
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return false;
        }

        self.allowed_roots.iter().any(|root| {
            if !root.is_absolute()
                || root.components().any(|component| matches!(component, Component::ParentDir))
                || !path.starts_with(root)
            {
                return false;
            }

            let Ok(canonical_root) = std::fs::canonicalize(root) else {
                // An unresolved policy root cannot establish a trustworthy boundary.
                return false;
            };

            let Ok(relative) = path.strip_prefix(root) else {
                return false;
            };
            let mut current = root.clone();
            for component in relative.components() {
                current.push(component);
                match std::fs::symlink_metadata(&current) {
                    Ok(_) => {
                        let Ok(canonical) = std::fs::canonicalize(&current) else {
                            // Includes dangling symlinks, whose eventual target cannot be trusted.
                            return false;
                        };
                        if !canonical.starts_with(&canonical_root) {
                            return false;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        // Once a component is absent, every remaining component is new and cannot
                        // currently hide a symlink. The tool may create this suffix.
                        break;
                    }
                    Err(_) => return false,
                }
            }

            true
        })
    }
}

#[async_trait]
impl ToolGuardrail for PathAllowList {
    fn name(&self) -> &str {
        &self.name
    }

    fn applies_to(&self, tool_name: &str) -> bool {
        match &self.tools {
            Some(tools) => tools.iter().any(|t| t == tool_name),
            None => true,
        }
    }

    async fn validate_call(&self, tool_name: &str, args: &Value) -> ToolGuardrailResult {
        for arg_name in &self.arg_names {
            let Some(value) = args.get(arg_name) else {
                if self.allow_missing {
                    continue;
                }
                return ToolGuardrailResult::deny(
                    format!(
                        "argument `{arg_name}` of `{tool_name}` is missing, so its path cannot be \
                         checked; configure the guardrail with allow_missing() to permit calls \
                         without it"
                    ),
                    self.severity,
                );
            };

            let Some(candidate) = value.as_str() else {
                return ToolGuardrailResult::deny(
                    format!(
                        "argument `{arg_name}` of `{tool_name}` must be a path string, got \
                         {value}"
                    ),
                    self.severity,
                );
            };

            if !self.is_permitted(candidate) {
                let roots: Vec<_> =
                    self.allowed_roots.iter().map(|r| r.display().to_string()).collect();
                return ToolGuardrailResult::deny(
                    format!(
                        "argument `{arg_name}` of `{tool_name}` is {candidate:?}, which is not an \
                         absolute path inside an allowed root ({})",
                        roots.join(", ")
                    ),
                    self.severity,
                );
            }
        }

        ToolGuardrailResult::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentFilter, PiiRedactor};
    use adk_core::ToolResultScreen;
    use serde_json::json;

    #[tokio::test]
    async fn result_text_guardrail_redacts_every_string_and_keeps_the_shape() {
        let guardrails = ToolGuardrailSet::new().with(ResultTextGuardrail::new(PiiRedactor::new()));
        let result = json!({
            "order": 42,
            "customer": {"email": "jane@example.com", "notes": ["ssn 123-45-6789", true]},
        });
        assert_eq!(
            guardrails.evaluate_result("lookup", &json!({}), &result).await,
            ToolResultDecision::Deliver {
                result: json!({
                    "order": 42,
                    "customer": {"email": "[EMAIL REDACTED]", "notes": ["ssn [SSN REDACTED]", true]},
                }),
            }
        );
    }

    #[tokio::test]
    async fn a_failing_text_check_withholds_the_result() {
        let filter = ContentFilter::blocked_keywords(vec!["internal only".into()]).unwrap();
        let guardrails = ToolGuardrailSet::new().with(ResultTextGuardrail::new(filter));
        let decision = guardrails
            .evaluate_result("wiki", &json!({}), &json!({"page": ["INTERNAL ONLY: roadmap"]}))
            .await;
        assert!(matches!(
            decision,
            ToolResultDecision::Withhold { ref guardrail, severity: Severity::High, .. }
                if guardrail == "blocked_keywords"
        ));
        let screened =
            guardrails.screen_result("wiki", &json!({}), &json!({"page": "internal only"})).await;
        assert!(screened.unwrap_err().starts_with("tool guardrail 'blocked_keywords' (High)"));
    }

    #[tokio::test]
    async fn result_guardrails_are_scoped_and_compose() {
        let guardrails = ToolGuardrailSet::new()
            .with(ResultTextGuardrail::new(PiiRedactor::new()).on_tools(["lookup"]))
            .with(ResultTextGuardrail::new(
                ContentFilter::blocked_keywords(vec!["redacted".into()]).unwrap(),
            ));
        let other =
            guardrails.evaluate_result("search", &json!({}), &json!("mail jane@example.com")).await;
        assert_eq!(other, ToolResultDecision::Deliver { result: json!("mail jane@example.com") });
        // The second guardrail sees what the first produced.
        let lookup =
            guardrails.evaluate_result("lookup", &json!({}), &json!("mail jane@example.com")).await;
        assert!(matches!(lookup, ToolResultDecision::Withhold { .. }));
    }

    #[tokio::test]
    async fn call_guardrails_deliver_results_unchanged() {
        let guardrails = ToolGuardrailSet::new().with(ForceDryRun);
        let result = json!({"ok": true});
        assert_eq!(
            guardrails.evaluate_result("run", &json!({}), &result).await,
            ToolResultDecision::Deliver { result }
        );
    }

    /// Revises rather than denies, so composition can be observed.
    struct ForceDryRun;

    #[async_trait]
    impl ToolGuardrail for ForceDryRun {
        fn name(&self) -> &str {
            "force-dry-run"
        }
        async fn validate_call(&self, _tool: &str, args: &Value) -> ToolGuardrailResult {
            let mut revised = args.clone();
            if let Some(object) = revised.as_object_mut() {
                object.insert("dry_run".to_string(), json!(true));
            }
            ToolGuardrailResult::revise(revised, "dry-run is mandatory here")
        }
    }

    struct DenyAll;

    #[async_trait]
    impl ToolGuardrail for DenyAll {
        fn name(&self) -> &str {
            "deny-all"
        }
        async fn validate_call(&self, _tool: &str, _args: &Value) -> ToolGuardrailResult {
            ToolGuardrailResult::deny("nothing is permitted", Severity::Critical)
        }
    }

    #[tokio::test]
    async fn an_empty_set_allows_the_call_unchanged() {
        let decision = ToolGuardrailSet::new().evaluate("any", &json!({ "a": 1 })).await;

        match decision {
            ToolCallDecision::Allow { args } => assert_eq!(args, json!({ "a": 1 })),
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_revision_is_returned_to_the_caller() {
        let set = ToolGuardrailSet::new().with(ForceDryRun);

        match set.evaluate("delete", &json!({ "path": "/tmp/x" })).await {
            ToolCallDecision::Allow { args } => {
                assert_eq!(args, json!({ "path": "/tmp/x", "dry_run": true }));
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_denial_names_the_guardrail_that_refused() {
        let set = ToolGuardrailSet::new().with(DenyAll);

        match set.evaluate("delete", &json!({})).await {
            ToolCallDecision::Deny { guardrail, severity, .. } => {
                assert_eq!(guardrail, "deny-all");
                assert_eq!(severity, Severity::Critical);
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_denial_stops_evaluation_so_a_denied_call_is_never_revised() {
        let set = ToolGuardrailSet::new().with(DenyAll).with(ForceDryRun);

        assert!(!set.evaluate("delete", &json!({})).await.is_allowed());
    }

    #[tokio::test]
    async fn a_later_guardrail_sees_an_earlier_revision() {
        /// Denies unless a previous guardrail already set `dry_run`.
        struct RequireDryRun;

        #[async_trait]
        impl ToolGuardrail for RequireDryRun {
            fn name(&self) -> &str {
                "require-dry-run"
            }
            async fn validate_call(&self, _tool: &str, args: &Value) -> ToolGuardrailResult {
                if args.get("dry_run") == Some(&json!(true)) {
                    ToolGuardrailResult::Allow
                } else {
                    ToolGuardrailResult::deny("dry_run was not set", Severity::High)
                }
            }
        }

        let set = ToolGuardrailSet::new().with(ForceDryRun).with(RequireDryRun);
        assert!(
            set.evaluate("delete", &json!({})).await.is_allowed(),
            "revisions must compose in order"
        );

        let reversed = ToolGuardrailSet::new().with(RequireDryRun).with(ForceDryRun);
        assert!(
            !reversed.evaluate("delete", &json!({})).await.is_allowed(),
            "order is meaningful and must not be silently reordered"
        );
    }

    #[tokio::test]
    async fn applies_to_skips_an_unrelated_tool() {
        let set = ToolGuardrailSet::new().with(
            DeniedArgumentPattern::new("no-rf", r"-rf", Severity::Critical)
                .expect("valid pattern")
                .on_tools(["run_command"]),
        );

        assert!(set.evaluate("read_file", &json!({ "flags": "-rf" })).await.is_allowed());
        assert!(!set.evaluate("run_command", &json!({ "flags": "-rf" })).await.is_allowed());
    }

    #[tokio::test]
    async fn a_denied_pattern_matches_anywhere_in_the_arguments() {
        let guardrail = DeniedArgumentPattern::new("no-rf", r"-rf\b", Severity::Critical)
            .expect("valid pattern");

        for args in [
            json!({ "cmd": "rm -rf /" }),
            json!({ "nested": { "cmd": "rm -rf ." } }),
            json!({ "argv": ["rm", "-rf", "/tmp"] }),
        ] {
            assert!(
                !guardrail.validate_call("run_command", &args).await.is_allowed(),
                "should deny {args}"
            );
        }

        assert!(
            guardrail.validate_call("run_command", &json!({ "cmd": "ls -l" })).await.is_allowed()
        );
    }

    #[test]
    fn an_invalid_pattern_is_reported() {
        assert!(DeniedArgumentPattern::new("bad", "([unclosed", Severity::Low).is_err());
    }

    #[tokio::test]
    async fn a_path_inside_an_allowed_root_is_permitted() {
        let root = tempfile::tempdir().expect("allowed root");
        let guardrail = PathAllowList::new("agents", ["path"], [root.path()]);
        let candidate = root.path().join("x.plist");

        assert!(
            guardrail
                .validate_call("plist_write", &json!({ "path": candidate }))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn traversal_and_escape_attempts_are_denied() {
        let guardrail = PathAllowList::new("agents", ["path"], ["/Users/me/Library/LaunchAgents"]);

        for candidate in [
            "/Users/me/Library/LaunchAgents/../../../etc/passwd",
            "/etc/passwd",
            "relative/path.plist",
            "/Users/me/Library/LaunchAgentsEvil/x.plist",
        ] {
            assert!(
                !guardrail
                    .validate_call("plist_write", &json!({ "path": candidate }))
                    .await
                    .is_allowed(),
                "should deny {candidate:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_sibling_root_is_not_admitted_by_string_prefix() {
        // `/etc/passwd-backup` shares a string prefix with `/etc/passwd` but is a different file.
        let guardrail = PathAllowList::new("etc", ["path"], ["/etc/passwd"]);

        assert!(
            !guardrail
                .validate_call("read", &json!({ "path": "/etc/passwd-backup" }))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn a_non_string_path_argument_is_denied() {
        let guardrail = PathAllowList::new("agents", ["path"], ["/tmp"]);

        assert!(
            !guardrail.validate_call("write", &json!({ "path": 42 })).await.is_allowed(),
            "a non-string path cannot be checked and must not be waved through"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_inside_the_root_cannot_escape_it() {
        let root = tempfile::tempdir().expect("allowed root");
        let outside = tempfile::tempdir().expect("outside root");
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape"))
            .expect("create symlink");
        let guardrail = PathAllowList::new("root", ["path"], [root.path()]);
        let candidate = root.path().join("escape/secret.txt");

        assert!(
            !guardrail.validate_call("write", &json!({ "path": candidate })).await.is_allowed(),
            "a lexical child resolving outside the allowed root must be denied"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_symlink_inside_the_root_is_denied() {
        let root = tempfile::tempdir().expect("allowed root");
        let outside = tempfile::tempdir().expect("outside root");
        let missing_target = outside.path().join("not-created");
        std::os::unix::fs::symlink(&missing_target, root.path().join("escape"))
            .expect("create dangling symlink");
        let guardrail = PathAllowList::new("root", ["path"], [root.path()]);

        assert!(
            !guardrail
                .validate_call("write", &json!({ "path": root.path().join("escape/secret.txt") }))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn an_unresolvable_allowed_root_is_fail_closed() {
        let root = tempfile::tempdir().expect("root");
        let missing = root.path().join("not-created");
        let guardrail = PathAllowList::new("missing", ["path"], [&missing]);

        assert!(
            !guardrail
                .validate_call("write", &json!({ "path": missing.join("file.txt") }))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn an_absent_path_argument_is_denied() {
        let guardrail = PathAllowList::new("agents", ["path"], ["/tmp"]);

        for args in [json!({ "other": 1 }), json!({ "Path": "/etc/passwd" }), json!("/etc/passwd")]
        {
            assert!(
                !guardrail.validate_call("write", &args).await.is_allowed(),
                "a call without `path` cannot be shown to stay inside the roots: {args}"
            );
        }
    }

    #[tokio::test]
    async fn allow_missing_permits_an_absent_argument_but_checks_a_present_one() {
        let root = tempfile::tempdir().expect("allowed root");
        let guardrail =
            PathAllowList::new("moves", ["source", "destination"], [root.path()]).allow_missing();
        let inside = root.path().join("a.txt");

        assert!(guardrail.validate_call("rm", &json!({ "source": inside })).await.is_allowed());
        assert!(
            !guardrail
                .validate_call("mv", &json!({ "source": inside, "destination": "/etc/x" }))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn a_tool_outside_on_tools_is_not_checked() {
        let set = ToolGuardrailSet::new()
            .with(PathAllowList::new("writes", ["path"], ["/srv"]).on_tools(["write_file"]));

        assert!(set.evaluate("read_file", &json!({ "path": "/etc/passwd" })).await.is_allowed());
        assert!(!set.evaluate("write_file", &json!({ "path": "/etc/passwd" })).await.is_allowed());
        assert!(!set.evaluate("write_file", &json!({})).await.is_allowed());
    }
}
