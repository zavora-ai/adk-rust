//! [`CredentialBroker`] backed by a [`SecretProvider`].
//!
//! Each binding names the secret one agent may present at one resource and the scopes
//! that secret grants. A request for an agent and resource without a binding fails
//! loudly, so an agent never receives another agent's secret or a secret meant for a
//! different resource.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ActingContext, AdkError, AgentId, CredentialBroker, ErrorCategory, ErrorComponent,
    RevocationScope, ScopedToken, Url, async_trait,
};
use chrono::Utc;
use tokio::sync::RwLock;

use crate::secrets::SecretProvider;

#[derive(Debug, Clone)]
struct Binding {
    secret_name: String,
    granted_scopes: Vec<String>,
}

type BindingKey = (AgentId, Url);

/// Brokers resource-scoped tokens out of a [`SecretProvider`].
///
/// Bind each `(agent, resource)` pair to the secret holding its credential with
/// [`bind`](Self::bind). [`token_for`](CredentialBroker::token_for) returns the secret
/// as a [`ScopedToken`] whose audience is the requested resource, and caches it for the
/// [`with_ttl`](Self::with_ttl) period. [`revoke`](CredentialBroker::revoke) drops cached
/// tokens; rotate or disable the secret itself in the secret manager to withdraw it at
/// the source.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_auth::SecretCredentialBroker;
/// use adk_auth::secrets::SecretProvider;
/// use adk_core::{ActingContext, AdkError, CredentialBroker, async_trait, resource_indicator};
///
/// struct Vault;
///
/// #[async_trait]
/// impl SecretProvider for Vault {
///     async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
///         Ok(format!("value-of-{name}"))
///     }
/// }
///
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let acting = ActingContext::derived("shop", "cashier", "alice", "inv-1");
/// let crm = resource_indicator("https://crm.example.com/api")?;
/// let broker = SecretCredentialBroker::new(Arc::new(Vault))
///     .bind(acting.agent.id.clone(), crm.clone(), "cashier-crm-key", vec!["crm.read".into()]);
///
/// let token = broker.token_for(&acting, &crm, &["crm.read".into()]).await?;
/// assert_eq!(token.audience, crm);
/// assert_eq!(token.secret.expose(), "value-of-cashier-crm-key");
/// # Ok::<(), AdkError>(())
/// # }).unwrap();
/// ```
pub struct SecretCredentialBroker {
    provider: Arc<dyn SecretProvider>,
    bindings: HashMap<BindingKey, Binding>,
    ttl: Option<Duration>,
    cache: RwLock<HashMap<BindingKey, ScopedToken>>,
}

impl std::fmt::Debug for SecretCredentialBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretCredentialBroker")
            .field("bindings", &self.bindings.len())
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl SecretCredentialBroker {
    /// A broker with no bindings over `provider`.
    pub fn new(provider: Arc<dyn SecretProvider>) -> Self {
        Self { provider, bindings: HashMap::new(), ttl: None, cache: RwLock::new(HashMap::new()) }
    }

    /// Binds `agent` at `resource` to the secret `secret_name`, which grants `granted_scopes`.
    #[must_use]
    pub fn bind(
        mut self,
        agent: AgentId,
        resource: Url,
        secret_name: impl Into<String>,
        granted_scopes: Vec<String>,
    ) -> Self {
        self.bindings
            .insert((agent, resource), Binding { secret_name: secret_name.into(), granted_scopes });
        self
    }

    /// Caches issued tokens for `ttl` and stamps them with that expiry. Without a TTL the
    /// secret is read on every request and tokens carry no expiry.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }
}

#[async_trait]
impl CredentialBroker for SecretCredentialBroker {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        scopes: &[String],
    ) -> adk_core::Result<ScopedToken> {
        let key = (acting.agent.id.clone(), resource.clone());
        let Some(binding) = self.bindings.get(&key) else {
            tracing::warn!(
                agent.id = %acting.agent.id,
                credential.resource = %resource,
                "no credential bound for agent and resource"
            );
            return Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Forbidden,
                "credential.not_bound",
                format!(
                    "no credential is bound for agent '{}' at '{resource}'; bind one with \
                     SecretCredentialBroker::bind",
                    acting.agent.id
                ),
            ));
        };
        if let Some(missing) = scopes.iter().find(|scope| !binding.granted_scopes.contains(scope)) {
            return Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Forbidden,
                "credential.scope_exceeded",
                format!(
                    "scope '{missing}' is not granted to agent '{}' at '{resource}'; granted: {:?}",
                    acting.agent.id, binding.granted_scopes
                ),
            ));
        }
        let scopes =
            if scopes.is_empty() { binding.granted_scopes.clone() } else { scopes.to_vec() };

        if self.ttl.is_some()
            && let Some(cached) = self.cache.read().await.get(&key)
            && cached.is_live(Utc::now())
        {
            let mut token = cached.clone();
            token.scopes = scopes;
            return Ok(token);
        }

        let secret = self.provider.get_secret(&binding.secret_name).await?;
        let mut token = ScopedToken::new(acting.agent.id.clone(), secret, resource.clone(), scopes);
        if let Some(ttl) = self.ttl {
            let ttl = chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::MAX);
            token = token.expires_at(Utc::now() + ttl);
            self.cache.write().await.insert(key, token.clone());
        }
        tracing::debug!(
            agent.id = %acting.agent.id,
            credential.resource = %resource,
            "credential issued from secret provider"
        );
        Ok(token)
    }

    async fn revoke(&self, scope: &RevocationScope) -> adk_core::Result<u64> {
        let mut cache = self.cache.write().await;
        let before = cache.len();
        cache.retain(|(agent, resource), _| !scope.covers(agent, resource));
        Ok((before - cache.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{AgentPrincipal, resource_indicator};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingVault(AtomicUsize);

    #[async_trait]
    impl SecretProvider for CountingVault {
        async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(format!("secret:{name}"))
        }
    }

    fn acting(id: &str) -> ActingContext {
        ActingContext::new(AgentPrincipal::new(AgentId::new(id).unwrap()), "inv")
    }

    fn url(s: &str) -> Url {
        resource_indicator(s).unwrap()
    }

    fn broker(vault: Arc<CountingVault>) -> SecretCredentialBroker {
        SecretCredentialBroker::new(vault).bind(
            AgentId::new("urn:adk:agent:a").unwrap(),
            url("https://a.example.com/mcp"),
            "a-key",
            vec!["read".into(), "write".into()],
        )
    }

    #[tokio::test]
    async fn token_for_a_is_refused_at_b() {
        let broker = broker(Arc::default());
        let me = acting("urn:adk:agent:a");
        let token = broker.token_for(&me, &url("https://a.example.com/mcp"), &[]).await.unwrap();
        assert_eq!(token.audience, url("https://a.example.com/mcp"));
        assert_eq!(token.scopes, vec!["read".to_string(), "write".to_string()]);
        assert_eq!(token.agent, me.agent.id);
        let refused = token.authorize_for(&url("https://b.example.com/mcp")).unwrap_err();
        assert_eq!(refused.code, "credential.audience_mismatch");

        let unbound =
            broker.token_for(&me, &url("https://b.example.com/mcp"), &[]).await.unwrap_err();
        assert_eq!(unbound.code, "credential.not_bound");
    }

    #[tokio::test]
    async fn another_agent_never_gets_the_binding() {
        let broker = broker(Arc::default());
        let error = broker
            .token_for(&acting("urn:adk:agent:b"), &url("https://a.example.com/mcp"), &[])
            .await
            .unwrap_err();
        assert_eq!(error.code, "credential.not_bound");
    }

    #[tokio::test]
    async fn scopes_beyond_the_grant_are_refused() {
        let broker = broker(Arc::default());
        let error = broker
            .token_for(
                &acting("urn:adk:agent:a"),
                &url("https://a.example.com/mcp"),
                &["admin".into()],
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "credential.scope_exceeded");
    }

    #[tokio::test]
    async fn cached_tokens_are_revoked_by_scope() {
        let vault = Arc::new(CountingVault::default());
        let broker = broker(vault.clone()).with_ttl(Duration::from_secs(60));
        let me = acting("urn:adk:agent:a");
        let mcp = url("https://a.example.com/mcp");
        let first = broker.token_for(&me, &mcp, &["read".into()]).await.unwrap();
        assert!(first.expires_at.is_some());
        broker.token_for(&me, &mcp, &[]).await.unwrap();
        assert_eq!(vault.0.load(Ordering::SeqCst), 1, "second request served from cache");

        let other = RevocationScope::Agent(AgentId::new("urn:adk:agent:b").unwrap());
        assert_eq!(broker.revoke(&other).await.unwrap(), 0);
        assert_eq!(broker.revoke(&RevocationScope::Agent(me.agent.id.clone())).await.unwrap(), 1);
        broker.token_for(&me, &mcp, &[]).await.unwrap();
        assert_eq!(vault.0.load(Ordering::SeqCst), 2, "a revoked token is fetched again");
        assert_eq!(broker.revoke(&RevocationScope::All).await.unwrap(), 1);
    }
}
