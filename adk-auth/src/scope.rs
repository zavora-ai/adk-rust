//! Scope-based access control for tools.
//!
//! Scopes provide a declarative security model where tools declare what scopes
//! they require, and the framework automatically enforces them before execution.
//!
//! # Overview
//!
//! Unlike role-based access control (which maps users → roles → permissions),
//! scope-based access works at the tool level:
//!
//! 1. Tools declare required scopes via [`Tool::required_scopes()`]
//! 2. User scopes are resolved from session state, JWT claims, or a custom provider
//! 3. The [`ScopeGuard`] checks that the user has **all** required scopes
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_auth::{ScopeGuard, ContextScopeResolver};
//!
//! // Tools declare their requirements
//! let transfer = FunctionTool::new("transfer", "Transfer funds", handler)
//!     .with_scopes(&["finance:write", "verified"]);
//!
//! // Guard enforces scopes automatically
//! let guard = ScopeGuard::new(ContextScopeResolver);
//! let protected = guard.protect(transfer);
//! ```

use crate::audit::{AuditEvent, AuditOutcome, AuditSink};
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

/// Resolves the set of scopes granted to the current user.
///
/// Implementations can pull scopes from session state, JWT claims,
/// an external identity provider, or any other source.
#[async_trait]
pub trait ScopeResolver: Send + Sync {
    /// Returns the scopes granted to the user in the given tool context.
    async fn resolve(&self, ctx: &dyn ToolContext) -> Vec<String>;
}

/// Resolves user scopes from the `user_scopes()` method on [`ToolContext`].
///
/// This is the default resolver — it delegates directly to the context,
/// which may pull scopes from JWT claims, session state, or any other source
/// configured at the context level.
pub struct ContextScopeResolver;

#[async_trait]
impl ScopeResolver for ContextScopeResolver {
    async fn resolve(&self, ctx: &dyn ToolContext) -> Vec<String> {
        ctx.user_scopes()
    }
}

/// A static resolver that always returns a fixed set of scopes.
///
/// Useful for testing or when scopes are known at configuration time.
///
/// # Example
///
/// ```rust,ignore
/// let resolver = StaticScopeResolver::new(vec!["admin", "finance:write"]);
/// ```
pub struct StaticScopeResolver {
    scopes: Vec<String>,
}

impl StaticScopeResolver {
    /// Create a resolver with a fixed set of scopes.
    pub fn new(scopes: Vec<impl Into<String>>) -> Self {
        Self { scopes: scopes.into_iter().map(Into::into).collect() }
    }
}

#[async_trait]
impl ScopeResolver for StaticScopeResolver {
    async fn resolve(&self, _ctx: &dyn ToolContext) -> Vec<String> {
        self.scopes.clone()
    }
}

/// Checks whether a user's scopes satisfy a tool's requirements.
///
/// Returns `Ok(())` if the user has all required scopes, or an error
/// listing the missing scopes.
pub fn check_scopes(required: &[&str], granted: &[String]) -> std::result::Result<(), ScopeDenied> {
    if required.is_empty() {
        return Ok(());
    }

    let granted_set: HashSet<&str> = granted.iter().map(String::as_str).collect();
    let missing: Vec<String> =
        required.iter().filter(|s| !granted_set.contains(**s)).map(|s| s.to_string()).collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(ScopeDenied { required: required.iter().map(|s| s.to_string()).collect(), missing })
    }
}

/// Error returned when a user lacks required scopes.
#[derive(Debug, Clone)]
pub struct ScopeDenied {
    /// All scopes the tool requires.
    pub required: Vec<String>,
    /// Scopes the user is missing.
    pub missing: Vec<String>,
}

impl std::fmt::Display for ScopeDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "missing required scopes: [{}] (tool requires: [{}])",
            self.missing.join(", "),
            self.required.join(", ")
        )
    }
}

impl std::error::Error for ScopeDenied {}

/// Declarative scope enforcement for tools.
///
/// Wraps tools and automatically checks that the user has all scopes
/// declared by [`Tool::required_scopes()`] before allowing execution.
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::{ScopeGuard, ContextScopeResolver};
///
/// let guard = ScopeGuard::new(ContextScopeResolver);
///
/// // Wrap a single tool
/// let protected = guard.protect(my_tool);
///
/// // Wrap all tools in a vec
/// let protected_tools = guard.protect_all(tools);
/// ```
pub struct ScopeGuard {
    resolver: Arc<dyn ScopeResolver>,
    audit_sink: Option<Arc<dyn AuditSink>>,
}

impl ScopeGuard {
    /// Create a scope guard with the given resolver.
    pub fn new(resolver: impl ScopeResolver + 'static) -> Self {
        Self { resolver: Arc::new(resolver), audit_sink: None }
    }

    /// Create a scope guard with audit logging.
    pub fn with_audit(
        resolver: impl ScopeResolver + 'static,
        audit_sink: impl AuditSink + 'static,
    ) -> Self {
        Self { resolver: Arc::new(resolver), audit_sink: Some(Arc::new(audit_sink)) }
    }

    /// Wrap a tool with scope enforcement.
    ///
    /// If the tool declares no required scopes, the wrapper is a no-op passthrough.
    pub fn protect<T: Tool + 'static>(&self, tool: T) -> ScopedTool<T> {
        ScopedTool {
            inner: tool,
            resolver: self.resolver.clone(),
            audit_sink: self.audit_sink.clone(),
        }
    }

    /// Wrap all tools in a vec with scope enforcement.
    pub fn protect_all(&self, tools: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
        tools
            .into_iter()
            .map(|t| {
                let wrapped = ScopedToolDyn {
                    inner: t,
                    resolver: self.resolver.clone(),
                    audit_sink: self.audit_sink.clone(),
                };
                Arc::new(wrapped) as Arc<dyn Tool>
            })
            .collect()
    }
}

/// A tool wrapper that enforces scope requirements before execution.
pub struct ScopedTool<T: Tool> {
    inner: T,
    resolver: Arc<dyn ScopeResolver>,
    audit_sink: Option<Arc<dyn AuditSink>>,
}

#[async_trait]
impl<T: Tool + Send + Sync> Tool for ScopedTool<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let required = self.inner.required_scopes();
        if !required.is_empty() {
            let granted = self.resolver.resolve(ctx.as_ref()).await;
            let result = check_scopes(required, &granted);

            if let Some(sink) = &self.audit_sink {
                let outcome =
                    if result.is_ok() { AuditOutcome::Allowed } else { AuditOutcome::Denied };
                let event = AuditEvent::tool_access(ctx.user_id(), self.name(), outcome)
                    .with_session(ctx.session_id());
                let _ = sink.log(event).await;
            }

            if let Err(denied) = result {
                tracing::warn!(
                    tool.name = %self.name(),
                    user.id = %ctx.user_id(),
                    missing_scopes = ?denied.missing,
                    "scope check failed"
                );
                return Err(adk_core::AdkError::Tool(denied.to_string()));
            }
        }

        self.inner.execute(ctx, args).await
    }
}

/// Dynamic version of [`ScopedTool`] for `Arc<dyn Tool>`.
pub struct ScopedToolDyn {
    inner: Arc<dyn Tool>,
    resolver: Arc<dyn ScopeResolver>,
    audit_sink: Option<Arc<dyn AuditSink>>,
}

#[async_trait]
impl Tool for ScopedToolDyn {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let required = self.inner.required_scopes();
        if !required.is_empty() {
            let granted = self.resolver.resolve(ctx.as_ref()).await;
            let result = check_scopes(required, &granted);

            if let Some(sink) = &self.audit_sink {
                let outcome =
                    if result.is_ok() { AuditOutcome::Allowed } else { AuditOutcome::Denied };
                let event = AuditEvent::tool_access(ctx.user_id(), self.name(), outcome)
                    .with_session(ctx.session_id());
                let _ = sink.log(event).await;
            }

            if let Err(denied) = result {
                tracing::warn!(
                    tool.name = %self.name(),
                    user.id = %ctx.user_id(),
                    missing_scopes = ?denied.missing,
                    "scope check failed"
                );
                return Err(adk_core::AdkError::Tool(denied.to_string()));
            }
        }

        self.inner.execute(ctx, args).await
    }
}

/// Extension trait for easily wrapping tools with scope enforcement.
pub trait ScopeToolExt: Tool + Sized {
    /// Wrap this tool with scope enforcement using the given resolver.
    fn with_scope_guard(self, resolver: impl ScopeResolver + 'static) -> ScopedTool<Self> {
        ScopedTool { inner: self, resolver: Arc::new(resolver), audit_sink: None }
    }
}

impl<T: Tool> ScopeToolExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_scopes_empty_required() {
        assert!(check_scopes(&[], &[]).is_ok());
        assert!(check_scopes(&[], &["admin".to_string()]).is_ok());
    }

    #[test]
    fn test_check_scopes_all_granted() {
        let granted = vec!["finance:read".to_string(), "finance:write".to_string()];
        assert!(check_scopes(&["finance:read", "finance:write"], &granted).is_ok());
    }

    #[test]
    fn test_check_scopes_subset_granted() {
        let granted =
            vec!["finance:read".to_string(), "finance:write".to_string(), "admin".to_string()];
        assert!(check_scopes(&["finance:write"], &granted).is_ok());
    }

    #[test]
    fn test_check_scopes_missing() {
        let granted = vec!["finance:read".to_string()];
        let err = check_scopes(&["finance:read", "finance:write"], &granted).unwrap_err();
        assert_eq!(err.missing, vec!["finance:write"]);
    }

    #[test]
    fn test_check_scopes_none_granted() {
        let err = check_scopes(&["admin"], &[]).unwrap_err();
        assert_eq!(err.missing, vec!["admin"]);
    }

    #[test]
    fn test_scope_denied_display() {
        let denied =
            ScopeDenied { required: vec!["a".into(), "b".into()], missing: vec!["b".into()] };
        let msg = denied.to_string();
        assert!(msg.contains("missing required scopes"));
        assert!(msg.contains("b"));
    }

    #[test]
    fn test_static_scope_resolver() {
        let resolver = StaticScopeResolver::new(vec!["admin", "finance:write"]);
        assert_eq!(resolver.scopes, vec!["admin", "finance:write"]);
    }
}
