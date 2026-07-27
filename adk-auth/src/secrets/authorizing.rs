//! Per-tool authorization for secret access.
//!
//! A tool holding a context can name any secret, and a
//! [`SecretProvider`](super::provider::SecretProvider) receives only
//! that name. Policy therefore collapses to whatever the backing cloud credentials can
//! read, and nothing distinguishes a weather tool asking for its own API key from the
//! same tool asking for a payment credential.
//!
//! [`AuthorizingSecretService`] closes that at the ADK layer: a declarative grant per
//! tool decides before the provider is called, and every decision is recorded without
//! the secret value.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{AdkError, Result, SecretRequest, SecretService};
use async_trait::async_trait;

/// What a single tool may read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretGrant {
    /// Secret names allowed verbatim.
    names: Vec<String>,
    /// Name prefixes allowed, for namespaced secrets such as `billing/`.
    prefixes: Vec<String>,
}

impl SecretGrant {
    /// A grant that allows nothing.
    pub fn none() -> Self {
        Self::default()
    }

    /// Allow an exact secret name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.names.push(name.into());
        self
    }

    /// Allow every name beginning with `prefix`.
    ///
    /// Use this for a namespace the tool owns. A prefix is a blunt instrument: prefer
    /// exact names where the set is known.
    #[must_use]
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefixes.push(prefix.into());
        self
    }

    /// Whether this grant covers `name`.
    fn allows(&self, name: &str) -> bool {
        self.names.iter().any(|allowed| allowed == name)
            || self.prefixes.iter().any(|prefix| name.starts_with(prefix.as_str()))
    }
}

/// Records secret access decisions.
///
/// Implementations must not receive or log secret values — only the decision and the
/// identity around it.
pub trait SecretAuditSink: Send + Sync {
    /// Called once per decision, before the provider is consulted on an allow.
    fn record(&self, decision: SecretAccessDecision<'_>);
}

/// A single allow or deny, carrying no secret value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretAccessDecision<'a> {
    /// Whether access was permitted.
    pub allowed: bool,
    /// The secret name requested.
    pub name: &'a str,
    /// The requesting tool, when the access came from one.
    pub tool_name: Option<&'a str>,
    /// The user the run belongs to.
    pub user_id: Option<&'a str>,
    /// The invocation the access happened in.
    pub invocation_id: Option<&'a str>,
    /// Why the decision went the way it did.
    pub reason: &'static str,
}

/// Wraps a [`SecretService`] with declarative per-tool grants and an audit record.
///
/// A request whose tool has no grant, or whose grant does not cover the name, is
/// refused **before** the inner service is called, so a denied name never reaches the
/// provider.
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::secrets::{AuthorizingSecretService, SecretGrant};
/// use std::sync::Arc;
///
/// let service = AuthorizingSecretService::new(inner)
///     .grant("weather_lookup", SecretGrant::none().name("weather-api-key"))
///     .grant("charge_card", SecretGrant::none().prefix("billing/"));
/// ```
pub struct AuthorizingSecretService {
    inner: Arc<dyn SecretService>,
    grants: HashMap<String, SecretGrant>,
    /// What an access with no tool identity may read.
    untooled: SecretGrant,
    audit: Option<Arc<dyn SecretAuditSink>>,
}

impl AuthorizingSecretService {
    /// Wrap `inner`, denying everything until grants are added.
    pub fn new(inner: Arc<dyn SecretService>) -> Self {
        Self { inner, grants: HashMap::new(), untooled: SecretGrant::none(), audit: None }
    }

    /// Grant `tool_name` access to the secrets described by `grant`.
    #[must_use]
    pub fn grant(mut self, tool_name: impl Into<String>, grant: SecretGrant) -> Self {
        self.grants.insert(tool_name.into(), grant);
        self
    }

    /// Grant access for requests that carry no tool identity.
    ///
    /// These are accesses made by the agent itself rather than by a dispatched tool.
    /// They are denied by default, because a request with no identity cannot be
    /// attributed.
    #[must_use]
    pub fn grant_untooled(mut self, grant: SecretGrant) -> Self {
        self.untooled = grant;
        self
    }

    /// Record every decision to `sink`.
    #[must_use]
    pub fn with_audit_sink(mut self, sink: Arc<dyn SecretAuditSink>) -> Self {
        self.audit = Some(sink);
        self
    }

    /// Decide whether `request` is permitted, and why.
    fn decide(&self, request: &SecretRequest) -> (bool, &'static str) {
        match &request.tool_name {
            Some(tool_name) => match self.grants.get(tool_name) {
                Some(grant) if grant.allows(&request.name) => (true, "granted to tool"),
                Some(_) => (false, "secret not in the tool's grant"),
                None => (false, "no grant for tool"),
            },
            None => {
                if self.untooled.allows(&request.name) {
                    (true, "granted without tool identity")
                } else {
                    (false, "no grant for a request without tool identity")
                }
            }
        }
    }

    fn record(&self, request: &SecretRequest, allowed: bool, reason: &'static str) {
        let decision = SecretAccessDecision {
            allowed,
            name: &request.name,
            tool_name: request.tool_name.as_deref(),
            user_id: request.user_id.as_deref(),
            invocation_id: request.invocation_id.as_deref(),
            reason,
        };
        if allowed {
            tracing::info!(
                secret.name = %decision.name,
                tool.name = decision.tool_name.unwrap_or("<none>"),
                user.id = decision.user_id.unwrap_or("<unknown>"),
                invocation.id = decision.invocation_id.unwrap_or("<unknown>"),
                decision.reason = reason,
                "secret access allowed"
            );
        } else {
            tracing::warn!(
                secret.name = %decision.name,
                tool.name = decision.tool_name.unwrap_or("<none>"),
                user.id = decision.user_id.unwrap_or("<unknown>"),
                invocation.id = decision.invocation_id.unwrap_or("<unknown>"),
                decision.reason = reason,
                "secret access denied"
            );
        }
        if let Some(sink) = &self.audit {
            sink.record(decision);
        }
    }
}

impl std::fmt::Debug for AuthorizingSecretService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizingSecretService")
            .field("granted_tools", &self.grants.keys().collect::<Vec<_>>())
            .field("audited", &self.audit.is_some())
            .finish()
    }
}

#[async_trait]
impl SecretService for AuthorizingSecretService {
    /// Denies unconditionally.
    ///
    /// # Errors
    ///
    /// A bare name carries no identity, so there is nothing to authorize against.
    /// Callers reach this service through
    /// [`SecretService::get_secret_for`], which the framework uses.
    async fn get_secret(&self, name: &str) -> Result<String> {
        let request = SecretRequest::new(name);
        self.record(&request, false, "no identity supplied");
        Err(AdkError::unauthorized(
            adk_core::ErrorComponent::Tool,
            "secret.no_identity",
            format!("secret '{name}' was requested without identity, so it cannot be authorized"),
        ))
    }

    async fn get_secret_for(&self, request: &SecretRequest) -> Result<String> {
        let (allowed, reason) = self.decide(request);
        self.record(request, allowed, reason);
        if !allowed {
            // The provider is never consulted, so a denied name is not even looked up.
            return Err(AdkError::unauthorized(
                adk_core::ErrorComponent::Tool,
                "secret.access_denied",
                format!(
                    "tool {} is not permitted to read secret '{}': {reason}",
                    request.tool_name.as_deref().unwrap_or("<none>"),
                    request.name
                ),
            ));
        }
        self.inner.get_secret_for(request).await
    }
}
