//! Google Cloud: workload identity federation through the Security Token Service.

use std::sync::Arc;

use adk_core::{
    ActingContext, AgentPrincipal, CredentialBroker, CredentialSecret, RevocationScope,
    ScopedToken, Url, async_trait,
};
use chrono::{DateTime, Utc};

use super::exchange::{REQUEST_TIMEOUT, TokenCache, oauth_token};
use super::{AgentTokenSource, CloudProvider, FederationError};

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_ACCESS: &str = "urn:ietf:params:oauth:token-type:access_token";
const TOKEN_TYPE_JWT: &str = "urn:ietf:params:oauth:token-type:jwt";

/// A Google Cloud federated access token.
///
/// `Debug` never prints the token.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::GoogleAccessToken;
/// use adk_core::CredentialSecret;
///
/// let token = GoogleAccessToken {
///     secret: CredentialSecret::new("ya29.d.example"),
///     expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
///     scopes: vec!["https://www.googleapis.com/auth/cloud-platform".into()],
/// };
/// assert!(!format!("{token:?}").contains("ya29"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleAccessToken {
    /// The bearer token.
    pub secret: CredentialSecret,
    /// When the token stops being valid.
    pub expires_at: DateTime<Utc>,
    /// The OAuth scopes the token was requested with.
    pub scopes: Vec<String>,
}

/// Exchanges an agent's token for a Google Cloud federated access token (RFC 8693 at
/// `sts.googleapis.com`).
///
/// A workload identity pool provider names the issuer and maps `google.subject` from the
/// token's `sub`, which Google limits to 127 bytes. The exchange carries no client
/// authentication. The agent's token is requested for the audience
/// [`token_audience`](Self::token_audience).
///
/// As a [`CredentialBroker`] it issues tokens for `https` resources on `googleapis.com`
/// only, with the requested scopes or [`DEFAULT_SCOPE`](Self::DEFAULT_SCOPE), and caches
/// each one per agent, resource and scope set until shortly before it expires. Google does
/// not bind a federated token to one API, so the binding to the resource is the
/// [`ScopedToken`]'s. [`revoke`](CredentialBroker::revoke) drops cached tokens; Google
/// offers no revocation for them.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_auth::federation::{AgentTokenIssuer, GoogleWorkloadIdentity};
/// use adk_core::{AgentId, AgentPrincipal};
///
/// # async fn example(pem: &str) -> Result<(), Box<dyn std::error::Error>> {
/// let issuer = AgentTokenIssuer::from_rsa_pem("https://agents.example.com", "agents-1", pem)?;
/// let cashier = AgentPrincipal::new(AgentId::new("did:web:shop.example.com:agents:cashier")?);
///
/// let google =
///     GoogleWorkloadIdentity::new("123456789012", "agents-pool", "agents-issuer", Arc::new(issuer));
/// let token = google.access_token(&cashier).await?;
/// assert!(token.expires_at > chrono::Utc::now());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct GoogleWorkloadIdentity {
    audience: String,
    endpoint: String,
    tokens: Arc<dyn AgentTokenSource>,
    client: reqwest::Client,
    cache: TokenCache,
}

impl GoogleWorkloadIdentity {
    /// The scope requested when the caller names none.
    pub const DEFAULT_SCOPE: &'static str = "https://www.googleapis.com/auth/cloud-platform";

    /// A client for provider `provider_id` of workload identity pool `pool_id` in the
    /// project numbered `project_number`, with tokens from `tokens`.
    pub fn new(
        project_number: impl AsRef<str>,
        pool_id: impl AsRef<str>,
        provider_id: impl AsRef<str>,
        tokens: Arc<dyn AgentTokenSource>,
    ) -> Self {
        Self {
            audience: format!(
                "//iam.googleapis.com/projects/{}/locations/global/workloadIdentityPools/{}/providers/{}",
                project_number.as_ref(),
                pool_id.as_ref(),
                provider_id.as_ref()
            ),
            endpoint: "https://sts.googleapis.com/v1/token".to_string(),
            tokens,
            client: crate::http::client(REQUEST_TIMEOUT),
            cache: TokenCache::default(),
        }
    }

    /// Sends the exchange to `endpoint` instead of `https://sts.googleapis.com/v1/token`,
    /// for a regional or private endpoint.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Uses `client` for every request.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// The provider's resource name, sent as the exchange's `audience`:
    /// `//iam.googleapis.com/projects/{number}/locations/global/workloadIdentityPools/{pool}/providers/{provider}`.
    pub fn audience(&self) -> &str {
        &self.audience
    }

    /// The `aud` claim Google expects in the agent's token: `https:` followed by
    /// [`audience`](Self::audience).
    pub fn token_audience(&self) -> String {
        format!("https:{}", self.audience)
    }

    /// Exchanges `agent`'s token for an access token with
    /// [`DEFAULT_SCOPE`](Self::DEFAULT_SCOPE).
    ///
    /// # Errors
    ///
    /// Returns the token source's error when it yields no token,
    /// [`FederationError::Transport`] when the Security Token Service cannot be reached,
    /// [`FederationError::Refused`] with its error code (`invalid_grant`, ...) when it
    /// refuses, and [`FederationError::InvalidResponse`] when its answer cannot be read.
    pub async fn access_token(
        &self,
        agent: &AgentPrincipal,
    ) -> Result<GoogleAccessToken, FederationError> {
        self.exchange(agent, &[Self::DEFAULT_SCOPE.to_string()]).await
    }

    async fn exchange(
        &self,
        agent: &AgentPrincipal,
        scopes: &[String],
    ) -> Result<GoogleAccessToken, FederationError> {
        let token = self.tokens.agent_token(agent, &self.token_audience()).await?;
        let scope = scopes.join(" ");
        let form = [
            ("grant_type", GRANT_TYPE),
            ("audience", self.audience.as_str()),
            ("scope", scope.as_str()),
            ("requested_token_type", TOKEN_TYPE_ACCESS),
            ("subject_token_type", TOKEN_TYPE_JWT),
            ("subject_token", token.secret.expose()),
        ];
        let (secret, expires_at) =
            oauth_token(CloudProvider::Google, &self.client, &self.endpoint, &form).await?;
        tracing::debug!(agent.id = %agent.id, "google federated access token issued");
        Ok(GoogleAccessToken { secret, expires_at, scopes: scopes.to_vec() })
    }
}

#[async_trait]
impl CredentialBroker for GoogleWorkloadIdentity {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        scopes: &[String],
    ) -> adk_core::Result<ScopedToken> {
        let google_api = resource.scheme() == "https"
            && resource
                .host_str()
                .is_some_and(|host| host == "googleapis.com" || host.ends_with(".googleapis.com"));
        if !google_api {
            return Err(FederationError::UnsupportedRequest {
                provider: CloudProvider::Google,
                resource: resource.to_string(),
                reason: "a Google access token is sent only to an https URL on googleapis.com; \
                         use another broker for this resource"
                    .into(),
            }
            .into());
        }
        let mut scopes = scopes.to_vec();
        if scopes.is_empty() {
            scopes.push(Self::DEFAULT_SCOPE.to_string());
        }
        scopes.sort();
        scopes.dedup();
        let key = (acting.agent.id.clone(), resource.clone(), scopes);
        if let Some(cached) = self.cache.live(&key).await {
            return Ok(cached);
        }
        let issued = self.exchange(&acting.agent, &key.2).await?;
        let token = ScopedToken::new(
            acting.agent.id.clone(),
            issued.secret.expose(),
            resource.clone(),
            issued.scopes,
        )
        .expires_at(issued.expires_at);
        self.cache.store(key, token.clone()).await;
        Ok(token)
    }

    async fn revoke(&self, scope: &RevocationScope) -> adk_core::Result<u64> {
        Ok(self.cache.revoke(scope).await)
    }
}
