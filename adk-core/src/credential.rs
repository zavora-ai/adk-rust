//! Agent credentials, identity sources, and resource-scoped token brokers.
//!
//! | Item | Role |
//! |------|------|
//! | [`IdentitySource`] | Says who the running agent is ([`AgentPrincipal`]) and proves it ([`AgentCredential`]) |
//! | [`StaticIdentitySource`] | A fixed principal and optional credential, for configuration-driven deployments |
//! | [`CredentialBroker`] | Mints a [`ScopedToken`] for one acting agent, one resource, and a set of scopes |
//! | [`ScopedToken`] | A secret bound to one audience; [`ScopedToken::authorize_for`] refuses any other resource |
//!
//! SPIFFE Workload API and cloud metadata servers are extension points: implement
//! [`IdentitySource`] over the client of your choice and pass it to the runner.

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    ActingContext, AdkError, AgentId, AgentPrincipal, ErrorCategory, ErrorComponent, Result,
};

/// A secret value whose `Debug` and `Display` output is redacted.
///
/// # Example
///
/// ```rust
/// use adk_core::CredentialSecret;
///
/// let secret = CredentialSecret::new("s3cr3t");
/// assert_eq!(secret.expose(), "s3cr3t");
/// assert_eq!(format!("{secret:?}"), "CredentialSecret([redacted])");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialSecret(String);

impl CredentialSecret {
    /// Wraps `value`.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret value. Pass it only to the party it was issued for.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialSecret([redacted])")
    }
}

impl fmt::Display for CredentialSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// The form of an [`AgentCredential`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CredentialKind {
    /// A JWT-SVID (SPIFFE JWT identity document) or another signed JWT.
    JwtSvid,
    /// A PEM-encoded X.509 certificate chain, such as an X.509-SVID.
    X509,
    /// Any other bearer value, such as an API key or a platform-issued token.
    Opaque,
}

/// A credential proving an agent's identity to a third party.
///
/// # Example
///
/// ```rust
/// use adk_core::{AgentCredential, AgentId, CredentialKind};
///
/// let credential = AgentCredential::new(
///     AgentId::new("spiffe://example.org/agent/billing")?,
///     CredentialKind::JwtSvid,
///     "eyJhbGciOi...",
/// );
/// assert!(credential.is_live(chrono::Utc::now()));
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCredential {
    /// The agent the credential identifies.
    pub subject: AgentId,
    /// The credential's form.
    pub kind: CredentialKind,
    /// The credential value.
    pub secret: CredentialSecret,
    /// When the credential stops being valid, if it expires.
    pub expires_at: Option<DateTime<Utc>>,
    /// The audiences the credential was issued for; empty when unrestricted.
    pub audience: Vec<String>,
}

impl AgentCredential {
    /// A credential of `kind` for `subject` with no expiry or audience.
    pub fn new(subject: AgentId, kind: CredentialKind, secret: impl Into<String>) -> Self {
        Self {
            subject,
            kind,
            secret: CredentialSecret::new(secret),
            expires_at: None,
            audience: Vec::new(),
        }
    }

    /// Sets the expiry.
    #[must_use]
    pub fn expires_at(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Adds an audience.
    #[must_use]
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience.push(audience.into());
        self
    }

    /// Whether the credential is still valid at `now`.
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|at| at > now)
    }
}

/// Says who the running agents are, and proves it.
///
/// The runner reads the source at the start of every run, so a source backed by a
/// rotating workload identity (SPIFFE Workload API, a cloud metadata server) is read
/// fresh each time. The root agent acts as [`principal_for`](Self::principal_for) its
/// name, falling back to [`current`](Self::current); every other agent in the tree acts
/// as `principal_for` its name, falling back to
/// [`AgentPrincipal::derived`](crate::AgentPrincipal::derived). A principal's
/// [`AgentId`] must stay the same across redeploys; the deployment belongs in
/// [`AgentPrincipal::instance`].
///
/// # Example
///
/// ```rust
/// use adk_core::{AgentId, AgentPrincipal, IdentitySource, StaticIdentitySource};
///
/// # futures::executor::block_on(async {
/// let source = StaticIdentitySource::new(
///     AgentPrincipal::new(AgentId::new("urn:adk:agent:billing")?).with_tenant("acme"),
/// );
/// assert_eq!(source.current().await?.id.as_str(), "urn:adk:agent:billing");
/// assert!(source.credential().await.is_err());
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[async_trait]
pub trait IdentitySource: fmt::Debug + Send + Sync {
    /// The principal the workload runs as; the root agent's unless
    /// [`principal_for`](Self::principal_for) maps it.
    async fn current(&self) -> Result<AgentPrincipal>;

    /// The principal of the agent named `agent_name`, when the source maps it.
    ///
    /// The default maps no agent, so only the root agent acts as
    /// [`current`](Self::current).
    async fn principal_for(&self, agent_name: &str) -> Result<Option<AgentPrincipal>> {
        let _ = agent_name;
        Ok(None)
    }

    /// A credential proving the principal's identity.
    ///
    /// # Errors
    ///
    /// The default returns `identity.credential.unavailable`.
    async fn credential(&self) -> Result<AgentCredential> {
        Err(AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Unsupported,
            "identity.credential.unavailable",
            "this identity source issues no credential; configure one that does, such as \
             StaticIdentitySource::with_credential or a SPIFFE Workload API source",
        ))
    }
}

/// An [`IdentitySource`] with a fixed principal, optional fixed principals for named
/// agents, and, optionally, a fixed credential.
///
/// # Example
///
/// ```rust
/// use adk_core::{AgentId, AgentPrincipal, IdentitySource, StaticIdentitySource};
///
/// # futures::executor::block_on(async {
/// let source = StaticIdentitySource::new(AgentPrincipal::new(AgentId::new("spiffe://acme/ops")?))
///     .with_agent("auditor", AgentPrincipal::new(AgentId::new("spiffe://acme/auditor")?));
/// assert_eq!(
///     source.principal_for("auditor").await?.unwrap().id.as_str(),
///     "spiffe://acme/auditor"
/// );
/// assert!(source.principal_for("writer").await?.is_none());
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct StaticIdentitySource {
    principal: AgentPrincipal,
    agents: std::collections::BTreeMap<String, AgentPrincipal>,
    credential: Option<AgentCredential>,
}

impl StaticIdentitySource {
    /// A source whose root agent acts as `principal`.
    pub fn new(principal: AgentPrincipal) -> Self {
        Self { principal, agents: std::collections::BTreeMap::new(), credential: None }
    }

    /// Maps the agent named `agent_name` to `principal`.
    #[must_use]
    pub fn with_agent(mut self, agent_name: impl Into<String>, principal: AgentPrincipal) -> Self {
        self.agents.insert(agent_name.into(), principal);
        self
    }

    /// Sets the credential the source hands out.
    #[must_use]
    pub fn with_credential(mut self, credential: AgentCredential) -> Self {
        self.credential = Some(credential);
        self
    }
}

#[async_trait]
impl IdentitySource for StaticIdentitySource {
    async fn current(&self) -> Result<AgentPrincipal> {
        Ok(self.principal.clone())
    }

    async fn principal_for(&self, agent_name: &str) -> Result<Option<AgentPrincipal>> {
        Ok(self.agents.get(agent_name).cloned())
    }

    async fn credential(&self) -> Result<AgentCredential> {
        let Some(credential) = &self.credential else {
            return Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unsupported,
                "identity.credential.unavailable",
                "StaticIdentitySource has no credential; set one with with_credential",
            ));
        };
        if credential.subject != self.principal.id {
            return Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "identity.credential.subject_mismatch",
                format!(
                    "the configured credential identifies '{}' but the principal is '{}'; \
                     issue the credential for the principal's agent id",
                    credential.subject, self.principal.id
                ),
            ));
        }
        if !credential.is_live(Utc::now()) {
            return Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unauthorized,
                "identity.credential.expired",
                "the configured credential has expired; rotate it",
            ));
        }
        Ok(credential.clone())
    }
}

/// Validates `resource` as an RFC 8707 resource indicator: an absolute `http` or
/// `https` URI without a fragment or user information.
///
/// # Errors
///
/// Returns `credential.resource.invalid` otherwise.
///
/// # Example
///
/// ```rust
/// use adk_core::resource_indicator;
///
/// assert!(resource_indicator("https://api.example.com/mcp").is_ok());
/// assert!(resource_indicator("https://api.example.com/mcp#x").is_err());
/// assert!(resource_indicator("https://user@api.example.com/").is_err());
/// ```
pub fn resource_indicator(resource: &str) -> Result<Url> {
    let invalid = |why: &str| {
        AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::InvalidInput,
            "credential.resource.invalid",
            format!(
                "'{resource}' is not a resource indicator: {why}; pass an absolute https URL \
                 such as 'https://api.example.com/mcp' (RFC 8707)"
            ),
        )
    };
    let url = Url::parse(resource).map_err(|error| invalid(&error.to_string()))?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err(invalid("the scheme must be http or https"));
    }
    if url.fragment().is_some() {
        return Err(invalid("it must not carry a fragment"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("it must not carry user information"));
    }
    if url.host_str().is_none() {
        return Err(invalid("it must name a host"));
    }
    Ok(url)
}

/// A secret minted for one agent, one resource (its audience), and a set of scopes.
///
/// A token is presented only to its audience: [`authorize_for`](Self::authorize_for)
/// returns the secret for a request URL under the audience and refuses every other.
///
/// # Example
///
/// ```rust
/// use adk_core::{AgentId, ScopedToken, resource_indicator};
///
/// let token = ScopedToken::new(
///     AgentId::new("urn:adk:agent:billing")?,
///     "tok",
///     resource_indicator("https://a.example.com/mcp")?,
///     vec!["tools.read".into()],
/// );
/// assert!(token.authorize_for(&resource_indicator("https://a.example.com/mcp/tools")?).is_ok());
/// let refused = token.authorize_for(&resource_indicator("https://b.example.com/mcp")?);
/// assert_eq!(refused.unwrap_err().code, "credential.audience_mismatch");
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedToken {
    /// The agent the token was minted for.
    pub agent: AgentId,
    /// The token value.
    pub secret: CredentialSecret,
    /// The resource the token may be presented to (RFC 8707).
    pub audience: Url,
    /// The scopes granted.
    pub scopes: Vec<String>,
    /// When the token stops being valid, if it expires.
    pub expires_at: Option<DateTime<Utc>>,
}

impl ScopedToken {
    /// A token for `agent` at `audience` with `scopes` and no expiry.
    pub fn new(
        agent: AgentId,
        secret: impl Into<String>,
        audience: Url,
        scopes: Vec<String>,
    ) -> Self {
        Self { agent, secret: CredentialSecret::new(secret), audience, scopes, expires_at: None }
    }

    /// Sets the expiry.
    #[must_use]
    pub fn expires_at(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Whether the token is still valid at `now`.
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|at| at > now)
    }

    /// Whether `request` falls under the token's audience: same scheme, host, and port,
    /// and a path equal to the audience path or below it at a segment boundary.
    pub fn covers(&self, request: &Url) -> bool {
        let audience = &self.audience;
        if request.scheme() != audience.scheme()
            || request.host_str().map(str::to_ascii_lowercase)
                != audience.host_str().map(str::to_ascii_lowercase)
            || request.port_or_known_default() != audience.port_or_known_default()
            || !request.username().is_empty()
            || request.password().is_some()
        {
            return false;
        }
        let base = audience.path().trim_end_matches('/');
        let path = request.path();
        base.is_empty()
            || path == base
            || path.strip_prefix(base).is_some_and(|rest| rest.starts_with('/'))
    }

    /// The secret, for a request to `request`.
    ///
    /// # Errors
    ///
    /// Returns `credential.audience_mismatch` when `request` is outside the token's
    /// audience, and `credential.expired` when the token has expired.
    pub fn authorize_for(&self, request: &Url) -> Result<&CredentialSecret> {
        if !self.covers(request) {
            tracing::warn!(
                agent.id = %self.agent,
                token.audience = %self.audience,
                request.url = %request,
                "token refused for a resource outside its audience"
            );
            return Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Forbidden,
                "credential.audience_mismatch",
                format!(
                    "a token minted for '{}' must not be sent to '{request}'; request a token \
                     for that resource from the credential broker",
                    self.audience
                ),
            ));
        }
        if !self.is_live(Utc::now()) {
            return Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unauthorized,
                "credential.expired",
                format!("the token for '{}' has expired; request a new one", self.audience),
            ));
        }
        Ok(&self.secret)
    }
}

/// Which brokered tokens [`CredentialBroker::revoke`] withdraws.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RevocationScope {
    /// Every token the broker issued.
    All,
    /// Every token issued to one agent.
    Agent(AgentId),
    /// Every token issued for one resource.
    Resource(Url),
    /// Tokens issued to one agent for one resource.
    AgentResource {
        /// The agent.
        agent: AgentId,
        /// The resource.
        resource: Url,
    },
}

impl RevocationScope {
    /// Whether a token for `agent` at `resource` falls in this scope.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{AgentId, RevocationScope, resource_indicator};
    ///
    /// let agent = AgentId::new("urn:adk:agent:a")?;
    /// let resource = resource_indicator("https://api.example.com/")?;
    /// assert!(RevocationScope::Agent(agent.clone()).covers(&agent, &resource));
    /// assert!(!RevocationScope::Agent(AgentId::new("urn:adk:agent:b")?).covers(&agent, &resource));
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn covers(&self, agent: &AgentId, resource: &Url) -> bool {
        match self {
            Self::All => true,
            Self::Agent(scope) => scope == agent,
            Self::Resource(scope) => scope == resource,
            Self::AgentResource { agent: scope_agent, resource: scope_resource } => {
                scope_agent == agent && scope_resource == resource
            }
        }
    }
}

/// Mints resource-scoped tokens for acting agents.
///
/// A broker issues a token for exactly the resource it was asked for (RFC 8707
/// `resource`, carried as the token's audience) and never returns a token minted for a
/// different resource. Callers present it with [`ScopedToken::authorize_for`], which
/// refuses any request outside the audience.
///
/// # Example
///
/// ```rust
/// use adk_core::{
///     ActingContext, CredentialBroker, Result, RevocationScope, ScopedToken, async_trait,
/// };
/// use url::Url;
///
/// /// Issues a fixed development token.
/// #[derive(Debug)]
/// struct DevBroker;
///
/// #[async_trait]
/// impl CredentialBroker for DevBroker {
///     async fn token_for(
///         &self,
///         acting: &ActingContext,
///         resource: &Url,
///         scopes: &[String],
///     ) -> Result<ScopedToken> {
///         Ok(ScopedToken::new(acting.agent.id.clone(), "dev", resource.clone(), scopes.to_vec()))
///     }
///
///     async fn revoke(&self, _scope: &RevocationScope) -> Result<u64> {
///         Ok(0)
///     }
/// }
/// ```
#[async_trait]
pub trait CredentialBroker: fmt::Debug + Send + Sync {
    /// A token for `acting`'s agent to present at `resource` with `scopes`.
    ///
    /// # Errors
    ///
    /// Returns an error when the broker holds no credential for the agent and resource,
    /// when the requested scopes exceed what it may grant, or when the issuer fails.
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        scopes: &[String],
    ) -> Result<ScopedToken>;

    /// Withdraws the tokens in `scope`, returning how many were withdrawn.
    async fn revoke(&self, scope: &RevocationScope) -> Result<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(audience: &str) -> ScopedToken {
        ScopedToken::new(
            AgentId::new("urn:adk:agent:t").unwrap(),
            "secret",
            resource_indicator(audience).unwrap(),
            vec![],
        )
    }

    #[test]
    fn audience_matching_is_strict() {
        let mcp = token("https://a.example.com/mcp");
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(mcp.covers(&url("https://a.example.com/mcp")));
        assert!(mcp.covers(&url("https://A.example.com:443/mcp/tools?x=1")));
        assert!(!mcp.covers(&url("https://a.example.com/mcpx")));
        assert!(!mcp.covers(&url("https://a.example.com/other")));
        assert!(!mcp.covers(&url("http://a.example.com/mcp")));
        assert!(!mcp.covers(&url("https://a.example.com:8443/mcp")));
        assert!(!mcp.covers(&url("https://b.example.com/mcp")));
        assert!(!mcp.covers(&url("https://a.example.com.evil.test/mcp")));
        assert!(!mcp.covers(&url("https://x@a.example.com/mcp")));

        let root = token("https://a.example.com/");
        assert!(root.covers(&url("https://a.example.com/anything")));
        assert!(!root.covers(&url("https://b.example.com/anything")));
    }

    #[test]
    fn expired_tokens_are_refused() {
        let expired =
            token("https://a.example.com/").expires_at(Utc::now() - chrono::Duration::seconds(1));
        let error =
            expired.authorize_for(&Url::parse("https://a.example.com/").unwrap()).unwrap_err();
        assert_eq!(error.code, "credential.expired");
    }

    #[test]
    fn secrets_never_print() {
        let credential = AgentCredential::new(
            AgentId::new("urn:adk:agent:t").unwrap(),
            CredentialKind::Opaque,
            "hunter2",
        );
        assert!(!format!("{credential:?}").contains("hunter2"));
        assert!(!format!("{:?}", token("https://a.example.com/")).contains("secret\""));
    }

    #[tokio::test]
    async fn static_source_refuses_a_credential_for_another_agent() {
        let principal = AgentPrincipal::new(AgentId::new("urn:adk:agent:a").unwrap());
        let other = AgentCredential::new(
            AgentId::new("urn:adk:agent:b").unwrap(),
            CredentialKind::JwtSvid,
            "jwt",
        );
        let source = StaticIdentitySource::new(principal).with_credential(other);
        assert_eq!(
            source.credential().await.unwrap_err().code,
            "identity.credential.subject_mismatch"
        );
    }
}
