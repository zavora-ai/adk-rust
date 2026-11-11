//! Microsoft Entra Agent ID: the agent's token as the federated credential of an agent
//! identity blueprint, exchanged in two steps for a token of the agent identity.

use std::sync::Arc;

use adk_core::{
    ActingContext, AgentPrincipal, CredentialBroker, CredentialSecret, RevocationScope,
    ScopedToken, Url, async_trait,
};
use chrono::{DateTime, Utc};

use super::exchange::{REQUEST_TIMEOUT, TokenCache, oauth_token};
use super::{AgentTokenSource, CloudProvider, FederationError};

const ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// An access token Microsoft Entra issued to an agent identity.
///
/// `Debug` never prints the token.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::EntraAccessToken;
/// use adk_core::CredentialSecret;
///
/// let token = EntraAccessToken {
///     secret: CredentialSecret::new("eyJ0eXAiOi..."),
///     expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
///     scope: "https://graph.microsoft.com/.default".into(),
/// };
/// assert!(!format!("{token:?}").contains("eyJ0"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntraAccessToken {
    /// The bearer token.
    pub secret: CredentialSecret,
    /// When the token stops being valid.
    pub expires_at: DateTime<Utc>,
    /// The scope the token was requested for: `{resource}/.default`.
    pub scope: String,
}

/// Exchanges an agent's token for an access token of a Microsoft Entra agent identity.
///
/// The agent identity blueprint holds a federated identity credential naming the issuer and
/// the agent's [`AgentId`](adk_core::AgentId) as its subject. Both requests go to
/// `{authority}/{tenant}/oauth2/v2.0/token` with `grant_type=client_credentials`:
///
/// | Step | `client_id` | `scope` | `client_assertion` | Further fields |
/// |------|-------------|---------|--------------------|----------------|
/// | 1 | The blueprint's application id | `api://AzureADTokenExchange/.default` | The agent's token, audience [`ASSERTION_AUDIENCE`](Self::ASSERTION_AUDIENCE) | `fmi_path`: the agent identity's id |
/// | 2 | The agent identity's id | `{resource}/.default` | The access token of step 1 | |
///
/// As a [`CredentialBroker`] it issues the token for `{resource}/.default`, the resource
/// URL without a trailing slash, and caches it per agent and resource until shortly before
/// it expires. An application token has no narrower scope, so a request naming scopes is
/// refused. [`revoke`](CredentialBroker::revoke) drops cached tokens; Entra offers no
/// revocation for them.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_auth::federation::{AgentTokenIssuer, EntraAgentIdentity};
/// use adk_core::{AgentId, AgentPrincipal};
///
/// # async fn example(pem: &str) -> Result<(), Box<dyn std::error::Error>> {
/// let issuer = AgentTokenIssuer::from_rsa_pem("https://agents.example.com", "agents-1", pem)?;
/// let cashier = AgentPrincipal::new(AgentId::new("did:web:shop.example.com:agents:cashier")?);
///
/// let entra = EntraAgentIdentity::new(
///     "8f2c6a1e-0000-4000-8000-000000000001",
///     "8f2c6a1e-0000-4000-8000-000000000002",
///     "8f2c6a1e-0000-4000-8000-000000000003",
///     Arc::new(issuer),
/// );
/// let token = entra.access_token(&cashier, "https://graph.microsoft.com").await?;
/// assert_eq!(token.scope, "https://graph.microsoft.com/.default");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct EntraAgentIdentity {
    authority: String,
    tenant_id: String,
    blueprint_app_id: String,
    agent_identity_id: String,
    tokens: Arc<dyn AgentTokenSource>,
    client: reqwest::Client,
    cache: TokenCache,
}

impl EntraAgentIdentity {
    /// The audience the agent's token is issued for.
    pub const ASSERTION_AUDIENCE: &'static str = "api://AzureADTokenExchange";

    /// A client for the agent identity `agent_identity_id`, created from the blueprint
    /// application `blueprint_app_id` in tenant `tenant_id`, with tokens from `tokens`.
    pub fn new(
        tenant_id: impl Into<String>,
        blueprint_app_id: impl Into<String>,
        agent_identity_id: impl Into<String>,
        tokens: Arc<dyn AgentTokenSource>,
    ) -> Self {
        Self {
            authority: "https://login.microsoftonline.com".to_string(),
            tenant_id: tenant_id.into(),
            blueprint_app_id: blueprint_app_id.into(),
            agent_identity_id: agent_identity_id.into(),
            tokens,
            client: crate::http::client(REQUEST_TIMEOUT),
            cache: TokenCache::default(),
        }
    }

    /// Sends requests to `authority` instead of `https://login.microsoftonline.com`, for a
    /// national cloud such as `https://login.microsoftonline.us`.
    #[must_use]
    pub fn with_authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = authority.into().trim_end_matches('/').to_string();
        self
    }

    /// Uses `client` for every request.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Exchanges `agent`'s token for the agent identity's access token to `resource`, an
    /// application id URI such as `https://graph.microsoft.com` or `api://{application id}`.
    ///
    /// # Errors
    ///
    /// Returns the token source's error when it yields no token,
    /// [`FederationError::Transport`] when Entra cannot be reached,
    /// [`FederationError::Refused`] with Entra's error (`invalid_client` and an `AADSTS`
    /// message, ...) when either step is refused, and
    /// [`FederationError::InvalidResponse`] when an answer cannot be read.
    pub async fn access_token(
        &self,
        agent: &AgentPrincipal,
        resource: &str,
    ) -> Result<EntraAccessToken, FederationError> {
        const PROVIDER: CloudProvider = CloudProvider::Entra;
        let endpoint = format!("{}/{}/oauth2/v2.0/token", self.authority, self.tenant_id);
        let assertion = self.tokens.agent_token(agent, Self::ASSERTION_AUDIENCE).await?;
        let blueprint = [
            ("client_id", self.blueprint_app_id.as_str()),
            ("scope", "api://AzureADTokenExchange/.default"),
            ("grant_type", "client_credentials"),
            ("fmi_path", self.agent_identity_id.as_str()),
            ("client_assertion_type", ASSERTION_TYPE),
            ("client_assertion", assertion.secret.expose()),
        ];
        let (blueprint_token, _) =
            oauth_token(PROVIDER, &self.client, &endpoint, &blueprint).await?;

        let scope = format!("{resource}/.default");
        let identity = [
            ("client_id", self.agent_identity_id.as_str()),
            ("scope", scope.as_str()),
            ("grant_type", "client_credentials"),
            ("client_assertion_type", ASSERTION_TYPE),
            ("client_assertion", blueprint_token.expose()),
        ];
        let (secret, expires_at) =
            oauth_token(PROVIDER, &self.client, &endpoint, &identity).await?;
        tracing::debug!(
            agent.id = %agent.id,
            entra.agent_identity = %self.agent_identity_id,
            token.scope = %scope,
            "entra agent identity token issued"
        );
        Ok(EntraAccessToken { secret, expires_at, scope })
    }
}

#[async_trait]
impl CredentialBroker for EntraAgentIdentity {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        scopes: &[String],
    ) -> adk_core::Result<ScopedToken> {
        if !scopes.is_empty() {
            return Err(FederationError::UnsupportedRequest {
                provider: CloudProvider::Entra,
                resource: resource.to_string(),
                reason: "an agent identity's application token covers the resource's `.default` \
                         scope only; request the token with no scopes"
                    .into(),
            }
            .into());
        }
        let key = (acting.agent.id.clone(), resource.clone(), Vec::new());
        if let Some(cached) = self.cache.live(&key).await {
            return Ok(cached);
        }
        let issued =
            self.access_token(&acting.agent, resource.as_str().trim_end_matches('/')).await?;
        let token = ScopedToken::new(
            acting.agent.id.clone(),
            issued.secret.expose(),
            resource.clone(),
            vec![issued.scope],
        )
        .expires_at(issued.expires_at);
        self.cache.store(key, token.clone()).await;
        Ok(token)
    }

    async fn revoke(&self, scope: &RevocationScope) -> adk_core::Result<u64> {
        Ok(self.cache.revoke(scope).await)
    }
}
