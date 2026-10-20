//! RFC 8693 token exchange as a [`CredentialBroker`].
//!
//! The broker presents the agent's own credential (from an [`IdentitySource`]) as the
//! `subject_token` and asks the authorization server for a token bound to one resource
//! (RFC 8707 `resource`) and a set of scopes. In on-behalf-of mode
//! ([`TokenExchangeBroker::on_behalf_of`]) the subject's token is the `subject_token` and the
//! agent's credential is the `actor_token`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ActingContext, AdkError, AgentId, CredentialBroker, CredentialKind, CredentialSecret,
    ErrorCategory, ErrorComponent, IdentitySource, RevocationScope, ScopedToken, SubjectRef, Url,
    async_trait,
};
use chrono::Utc;
use serde::Deserialize;
use tokio::sync::RwLock;

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_JWT: &str = "urn:ietf:params:oauth:token-type:jwt";
const TOKEN_TYPE_ACCESS: &str = "urn:ietf:params:oauth:token-type:access_token";
const TOKEN_TYPE_ID: &str = "urn:ietf:params:oauth:token-type:id_token";

/// Tokens are refreshed this long before the server says they expire.
const EXPIRY_SKEW: Duration = Duration::from_secs(30);

/// Longest time one token or revocation request may take by default.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Who a cached token was issued for: the subject, the delegation chain, the agent, the
/// resource, and the sorted scopes. A token is never served to a run that differs in any
/// of them (spec 3.1 §10 F6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    subject: Option<SubjectRef>,
    delegation_chain: Vec<SubjectRef>,
    agent: AgentId,
    resource: Url,
    scopes: Vec<String>,
}

/// A subject's token, presented as the RFC 8693 `subject_token` of an on-behalf-of exchange.
///
/// Implements spec 3.1 §10 F6.
///
/// # Example
///
/// ```rust
/// use adk_auth::SubjectToken;
///
/// let token = SubjectToken::access_token("alice-access-token");
/// assert_eq!(token.token_type, "urn:ietf:params:oauth:token-type:access_token");
/// assert_eq!(format!("{:?}", token.secret), "CredentialSecret([redacted])");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectToken {
    /// The token value.
    pub secret: CredentialSecret,
    /// The RFC 8693 token type URI sent as `subject_token_type`.
    pub token_type: String,
}

impl SubjectToken {
    /// A token of `token_type`, an RFC 8693 token type URI.
    pub fn new(secret: impl Into<String>, token_type: impl Into<String>) -> Self {
        Self { secret: CredentialSecret::new(secret), token_type: token_type.into() }
    }

    /// An OAuth 2.0 access token (`urn:ietf:params:oauth:token-type:access_token`).
    pub fn access_token(secret: impl Into<String>) -> Self {
        Self::new(secret, TOKEN_TYPE_ACCESS)
    }

    /// A JWT (`urn:ietf:params:oauth:token-type:jwt`).
    pub fn jwt(secret: impl Into<String>) -> Self {
        Self::new(secret, TOKEN_TYPE_JWT)
    }

    /// An OpenID Connect ID token (`urn:ietf:params:oauth:token-type:id_token`).
    pub fn id_token(secret: impl Into<String>) -> Self {
        Self::new(secret, TOKEN_TYPE_ID)
    }
}

/// Supplies the subject's token for an on-behalf-of exchange.
///
/// Implement it over wherever the authenticated caller's token lives, such as the token
/// a server's `RequestContextExtractor` verified, kept per subject. The broker asks only
/// for the subject the run acts for, [`ActingContext::on_behalf_of`].
///
/// Implements spec 3.1 §10 F6.
///
/// # Example
///
/// ```rust
/// use std::collections::HashMap;
///
/// use adk_auth::{SubjectToken, SubjectTokenSource};
/// use adk_core::{
///     ActingContext, AdkError, ErrorCategory, ErrorComponent, SubjectRef, async_trait,
/// };
///
/// /// Tokens captured when each user signed in.
/// #[derive(Debug)]
/// struct SignedInUsers(HashMap<String, String>);
///
/// #[async_trait]
/// impl SubjectTokenSource for SignedInUsers {
///     async fn subject_token(
///         &self,
///         subject: &SubjectRef,
///         _acting: &ActingContext,
///     ) -> adk_core::Result<SubjectToken> {
///         self.0.get(&subject.id).map(SubjectToken::access_token).ok_or_else(|| {
///             AdkError::new(
///                 ErrorComponent::Auth,
///                 ErrorCategory::Unauthorized,
///                 "credential.subject_token_unavailable",
///                 format!("'{}' has no token; sign in again", subject.id),
///             )
///         })
///     }
/// }
/// ```
#[async_trait]
pub trait SubjectTokenSource: std::fmt::Debug + Send + Sync {
    /// The token proving `subject`'s identity; `acting` is the run that asks for it.
    ///
    /// # Errors
    ///
    /// Returns an error when no token for `subject` is available; the exchange fails
    /// with it.
    async fn subject_token(
        &self,
        subject: &SubjectRef,
        acting: &ActingContext,
    ) -> adk_core::Result<SubjectToken>;
}

#[derive(Debug, Deserialize)]
struct ExchangeResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

fn exchange_error(category: ErrorCategory, code: &'static str, message: String) -> AdkError {
    AdkError::new(ErrorComponent::Auth, category, code, message)
}

/// Exchanges the agent's credential for resource-scoped tokens (RFC 8693).
///
/// | Request field | Agent-only mode (default) | On-behalf-of mode |
/// |---------------|---------------------------|-------------------|
/// | `grant_type` | `urn:ietf:params:oauth:grant-type:token-exchange` | the same |
/// | `resource`, `audience` | The requested resource | the same |
/// | `scope` | The requested scopes, space-separated (omitted when empty) | the same |
/// | `subject_token` | [`IdentitySource::credential`] | [`SubjectTokenSource::subject_token`] for [`ActingContext::on_behalf_of`] |
/// | `subject_token_type` | `…:token-type:jwt` for a JWT-SVID, `…:token-type:access_token` for an opaque credential | [`SubjectToken::token_type`] |
/// | `actor_token`, `actor_token_type` | Not sent | [`IdentitySource::credential`] and its type |
///
/// Issued tokens are cached per subject, delegation chain, agent, resource, and scope set
/// until shortly before they expire, so a token minted for one subject is never served to
/// a run acting for another. Concurrent requests for a token that is not cached send one
/// exchange and share its result. [`revoke`](CredentialBroker::revoke) drops cached tokens
/// and, when a revocation endpoint is configured, revokes each one there (RFC 7009).
///
/// Requests give up after 30 seconds (connect timeout 5 seconds); pass a client with
/// other timeouts to [`with_http_client`](Self::with_http_client).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_auth::TokenExchangeBroker;
/// use adk_core::{
///     AgentCredential, AgentId, AgentPrincipal, CredentialKind, StaticIdentitySource,
///     resource_indicator,
/// };
///
/// let id = AgentId::new("spiffe://example.org/agent/billing")?;
/// let source = StaticIdentitySource::new(AgentPrincipal::new(id.clone()))
///     .with_credential(AgentCredential::new(id, CredentialKind::JwtSvid, "eyJ..."));
/// let broker = TokenExchangeBroker::new(
///     resource_indicator("https://sts.example.com/token")?,
///     Arc::new(source),
/// )
/// .with_client_credentials("billing-agent", "client-secret");
/// # Ok::<(), adk_core::AdkError>(())
/// ```
pub struct TokenExchangeBroker {
    token_endpoint: Url,
    revocation_endpoint: Option<Url>,
    identity: Arc<dyn IdentitySource>,
    client: reqwest::Client,
    client_auth: Option<(String, CredentialSecret)>,
    subjects: Option<Arc<dyn SubjectTokenSource>>,
    cache: RwLock<HashMap<CacheKey, ScopedToken>>,
    /// One gate per key with an exchange in flight; waiters reuse its result.
    exchanges: std::sync::Mutex<HashMap<CacheKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for TokenExchangeBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenExchangeBroker")
            .field("token_endpoint", &self.token_endpoint.as_str())
            .field("revocation_endpoint", &self.revocation_endpoint.as_ref().map(Url::as_str))
            .field("identity", &self.identity)
            .field("on_behalf_of", &self.subjects)
            .finish_non_exhaustive()
    }
}

impl TokenExchangeBroker {
    /// A broker exchanging `identity`'s credential at `token_endpoint`.
    pub fn new(token_endpoint: Url, identity: Arc<dyn IdentitySource>) -> Self {
        Self {
            token_endpoint,
            revocation_endpoint: None,
            identity,
            client: crate::http::client(REQUEST_TIMEOUT),
            client_auth: None,
            subjects: None,
            cache: RwLock::new(HashMap::new()),
            exchanges: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Authenticates to the token and revocation endpoints with HTTP Basic client credentials.
    #[must_use]
    pub fn with_client_credentials(
        mut self,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        self.client_auth = Some((client_id.into(), CredentialSecret::new(client_secret)));
        self
    }

    /// Revokes withdrawn tokens at `endpoint` (RFC 7009).
    #[must_use]
    pub fn with_revocation_endpoint(mut self, endpoint: Url) -> Self {
        self.revocation_endpoint = Some(endpoint);
        self
    }

    /// Exchanges on behalf of the subject each run acts for (RFC 8693 delegation).
    ///
    /// Every exchange sends the token `subjects` supplies for
    /// [`ActingContext::on_behalf_of`] as the `subject_token` and the agent's credential
    /// as the `actor_token`, so the issued token carries the subject's authority with the
    /// agent recorded as the actor. A run acting for no subject fails with
    /// `credential.subject_missing` and sends nothing. Without this call the broker
    /// exchanges the agent's own credential.
    ///
    /// Implements spec 3.1 §10 F6.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_auth::{SubjectToken, SubjectTokenSource, TokenExchangeBroker};
    /// use adk_core::{
    ///     ActingContext, AgentCredential, AgentId, AgentPrincipal, CredentialKind,
    ///     StaticIdentitySource, SubjectRef, async_trait, resource_indicator,
    /// };
    ///
    /// #[derive(Debug)]
    /// struct SessionTokens;
    ///
    /// #[async_trait]
    /// impl SubjectTokenSource for SessionTokens {
    ///     async fn subject_token(
    ///         &self,
    ///         subject: &SubjectRef,
    ///         _acting: &ActingContext,
    ///     ) -> adk_core::Result<SubjectToken> {
    ///         Ok(SubjectToken::access_token(format!("token-of-{}", subject.id)))
    ///     }
    /// }
    ///
    /// let id = AgentId::new("spiffe://example.org/agent/github")?;
    /// let source = StaticIdentitySource::new(AgentPrincipal::new(id.clone()))
    ///     .with_credential(AgentCredential::new(id, CredentialKind::JwtSvid, "eyJ..."));
    /// let broker = TokenExchangeBroker::new(
    ///     resource_indicator("https://sts.example.com/token")?,
    ///     Arc::new(source),
    /// )
    /// .on_behalf_of(Arc::new(SessionTokens));
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    #[must_use]
    pub fn on_behalf_of(mut self, subjects: Arc<dyn SubjectTokenSource>) -> Self {
        self.subjects = Some(subjects);
        self
    }

    /// Uses `client` for every request.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    fn authenticated(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.client_auth {
            Some((id, secret)) => request.basic_auth(id, Some(secret.expose())),
            None => request,
        }
    }

    /// The cached token for `key`, unless it expires within [`EXPIRY_SKEW`].
    async fn cached(&self, key: &CacheKey) -> Option<ScopedToken> {
        let refresh_at = Utc::now() + chrono::Duration::from_std(EXPIRY_SKEW).unwrap_or_default();
        self.cache.read().await.get(key).filter(|token| token.is_live(refresh_at)).cloned()
    }

    fn lock_exchanges(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<CacheKey, Arc<tokio::sync::Mutex<()>>>> {
        self.exchanges.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl CredentialBroker for TokenExchangeBroker {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        scopes: &[String],
    ) -> adk_core::Result<ScopedToken> {
        let mut sorted = scopes.to_vec();
        sorted.sort();
        sorted.dedup();
        let key = CacheKey {
            subject: acting.on_behalf_of.clone(),
            delegation_chain: acting.delegation_chain.clone(),
            agent: acting.agent.id.clone(),
            resource: resource.clone(),
            scopes: sorted,
        };
        if let Some(cached) = self.cached(&key).await {
            return Ok(cached);
        }

        let gate = self.lock_exchanges().entry(key.clone()).or_default().clone();
        let exchanged = {
            let _exchanging = gate.lock().await;
            // A caller that waited here is answered by the exchange it waited for.
            match self.cached(&key).await {
                Some(cached) => Ok(cached),
                None => self.exchange(acting, resource, &key).await,
            }
        };
        let mut exchanges = self.lock_exchanges();
        // The map's handle and this one: no other caller waits on the gate.
        if Arc::strong_count(&gate) == 2
            && exchanges.get(&key).is_some_and(|current| Arc::ptr_eq(current, &gate))
        {
            exchanges.remove(&key);
        }
        exchanged
    }

    async fn revoke(&self, scope: &RevocationScope) -> adk_core::Result<u64> {
        self.revoke_cached(scope).await
    }
}

impl TokenExchangeBroker {
    /// Sends one RFC 8693 exchange for `key` and caches the issued token.
    async fn exchange(
        &self,
        acting: &ActingContext,
        resource: &Url,
        key: &CacheKey,
    ) -> adk_core::Result<ScopedToken> {
        // Resolved before the agent's credential, so a run acting for no subject sends nothing.
        let subject_token = match &self.subjects {
            Some(subjects) => {
                let Some(subject) = &acting.on_behalf_of else {
                    return Err(exchange_error(
                        ErrorCategory::Forbidden,
                        "credential.subject_missing",
                        format!(
                            "an on-behalf-of exchange needs the subject agent '{}' acts for, and \
                             the run acts for none; set RunConfig::acting or use a broker \
                             without on_behalf_of",
                            acting.agent.id
                        ),
                    ));
                };
                Some(subjects.subject_token(subject, acting).await?)
            }
            None => None,
        };
        let credential = self.identity.credential().await?;
        if credential.subject != acting.agent.id {
            return Err(exchange_error(
                ErrorCategory::Forbidden,
                "credential.subject_mismatch",
                format!(
                    "the identity source's credential identifies '{}' but the acting agent is \
                     '{}'; give each agent a broker over its own identity source",
                    credential.subject, acting.agent.id
                ),
            ));
        }
        let credential_type = match credential.kind {
            CredentialKind::JwtSvid => TOKEN_TYPE_JWT,
            CredentialKind::Opaque => TOKEN_TYPE_ACCESS,
            other => {
                return Err(exchange_error(
                    ErrorCategory::Unsupported,
                    "credential.exchange_unsupported",
                    format!(
                        "a {other:?} credential cannot be a token-exchange subject_token or \
                         actor_token; use a JWT-SVID or an opaque access token"
                    ),
                ));
            }
        };

        let scope = key.scopes.join(" ");
        let mut form = vec![
            ("grant_type", GRANT_TYPE),
            ("resource", resource.as_str()),
            ("audience", resource.as_str()),
        ];
        match &subject_token {
            Some(subject) => form.extend([
                ("subject_token", subject.secret.expose()),
                ("subject_token_type", subject.token_type.as_str()),
                ("actor_token", credential.secret.expose()),
                ("actor_token_type", credential_type),
            ]),
            None => form.extend([
                ("subject_token", credential.secret.expose()),
                ("subject_token_type", credential_type),
            ]),
        }
        form.push(("requested_token_type", TOKEN_TYPE_ACCESS));
        if !scope.is_empty() {
            form.push(("scope", scope.as_str()));
        }
        let response = self
            .authenticated(self.client.post(self.token_endpoint.clone()))
            .form(&form)
            .send()
            .await
            .map_err(|error| {
                exchange_error(
                    ErrorCategory::Unavailable,
                    "credential.exchange_failed",
                    format!("token exchange at '{}' failed: {error}", self.token_endpoint),
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let category = if status.is_server_error() {
                ErrorCategory::Unavailable
            } else {
                ErrorCategory::Unauthorized
            };
            return Err(exchange_error(
                category,
                "credential.exchange_failed",
                format!(
                    "token exchange for '{resource}' was refused with {status}: {}",
                    body.chars().take(512).collect::<String>()
                ),
            )
            .with_upstream_status(status.as_u16()));
        }
        let issued: ExchangeResponse = response.json().await.map_err(|error| {
            exchange_error(
                ErrorCategory::Internal,
                "credential.exchange_failed",
                format!("token exchange response is not valid: {error}"),
            )
        })?;

        let granted = match issued.scope {
            Some(scope) => scope.split_whitespace().map(str::to_string).collect(),
            None => key.scopes.clone(),
        };
        let mut token = ScopedToken::new(
            acting.agent.id.clone(),
            issued.access_token,
            resource.clone(),
            granted,
        );
        if let Some(seconds) = issued.expires_in {
            token = token.expires_at(Utc::now() + chrono::Duration::seconds(seconds as i64));
        }
        tracing::debug!(
            agent.id = %acting.agent.id,
            credential.resource = %resource,
            credential.on_behalf_of = subject_token.is_some(),
            "token exchanged"
        );
        self.cache.write().await.insert(key.clone(), token.clone());
        Ok(token)
    }

    async fn revoke_cached(&self, scope: &RevocationScope) -> adk_core::Result<u64> {
        let withdrawn: Vec<ScopedToken> = {
            let mut cache = self.cache.write().await;
            let keys: Vec<CacheKey> = cache
                .keys()
                .filter(|key| scope.covers(&key.agent, &key.resource))
                .cloned()
                .collect();
            keys.iter().filter_map(|key| cache.remove(key)).collect()
        };
        if let Some(endpoint) = &self.revocation_endpoint {
            for token in &withdrawn {
                let response = self
                    .authenticated(self.client.post(endpoint.clone()))
                    .form(&[("token", token.secret.expose()), ("token_type_hint", "access_token")])
                    .send()
                    .await
                    .map_err(|error| {
                        exchange_error(
                            ErrorCategory::Unavailable,
                            "credential.revocation_failed",
                            format!("token revocation at '{endpoint}' failed: {error}"),
                        )
                    })?;
                if !response.status().is_success() {
                    let status = response.status();
                    return Err(exchange_error(
                        ErrorCategory::Unavailable,
                        "credential.revocation_failed",
                        format!("token revocation at '{endpoint}' returned {status}"),
                    )
                    .with_upstream_status(status.as_u16()));
                }
            }
        }
        Ok(withdrawn.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{AgentCredential, AgentPrincipal, StaticIdentitySource, resource_indicator};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn agent() -> AgentId {
        AgentId::new("spiffe://example.org/agent/billing").unwrap()
    }

    fn broker(server: &MockServer) -> TokenExchangeBroker {
        let source = StaticIdentitySource::new(AgentPrincipal::new(agent()))
            .with_credential(AgentCredential::new(agent(), CredentialKind::JwtSvid, "svid-jwt"));
        TokenExchangeBroker::new(
            Url::parse(&format!("{}/token", server.uri())).unwrap(),
            Arc::new(source),
        )
        .with_client_credentials("client", "secret")
    }

    fn acting() -> ActingContext {
        ActingContext::new(AgentPrincipal::new(agent()))
    }

    #[tokio::test]
    async fn exchanges_once_per_resource_and_binds_the_audience() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange",
            ))
            .and(body_string_contains("resource=https%3A%2F%2Fa.example.com%2Fmcp"))
            .and(body_string_contains("subject_token=svid-jwt"))
            .and(body_string_contains(
                "subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Ajwt",
            ))
            .and(body_string_contains("scope=tools.read"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "token-a",
                "issued_token_type": TOKEN_TYPE_ACCESS,
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("resource=https%3A%2F%2Fb.example.com%2Fmcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "token-b",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let broker = broker(&server);
        let a = resource_indicator("https://a.example.com/mcp").unwrap();
        let b = resource_indicator("https://b.example.com/mcp").unwrap();
        let scopes = vec!["tools.read".to_string()];

        let token_a = broker.token_for(&acting(), &a, &scopes).await.unwrap();
        assert_eq!(token_a.audience, a);
        assert_eq!(token_a.scopes, scopes);
        assert!(token_a.expires_at.is_some());
        let again = broker.token_for(&acting(), &a, &scopes).await.unwrap();
        assert_eq!(again, token_a, "the second request is served from the cache");

        let token_b = broker.token_for(&acting(), &b, &scopes).await.unwrap();
        assert_eq!(token_b.secret.expose(), "token-b");
        assert_eq!(
            token_a.authorize_for(&b).unwrap_err().code,
            "credential.audience_mismatch",
            "a token minted for A is refused at B"
        );
        assert_eq!(token_a.authorize_for(&a).unwrap().expose(), "token-a");
    }

    #[tokio::test]
    async fn refusals_carry_the_upstream_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error": "invalid_target"})),
            )
            .mount(&server)
            .await;
        let error = broker(&server)
            .token_for(&acting(), &resource_indicator("https://a.example.com/").unwrap(), &[])
            .await
            .unwrap_err();
        assert_eq!(error.code, "credential.exchange_failed");
        assert_eq!(error.details.upstream_status_code, Some(400));
        assert!(error.message.contains("invalid_target"));
    }

    #[tokio::test]
    async fn a_credential_for_another_agent_is_not_exchanged() {
        let server = MockServer::start().await;
        let other =
            ActingContext::new(AgentPrincipal::new(AgentId::new("urn:adk:agent:other").unwrap()));
        let error = broker(&server)
            .token_for(&other, &resource_indicator("https://a.example.com/").unwrap(), &[])
            .await
            .unwrap_err();
        assert_eq!(error.code, "credential.subject_mismatch");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn revoke_withdraws_tokens_at_the_revocation_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "token-a",
                "expires_in": 3600,
            })))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/revoke"))
            .and(body_string_contains("token=token-a"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let broker = broker(&server)
            .with_revocation_endpoint(Url::parse(&format!("{}/revoke", server.uri())).unwrap());
        let a = resource_indicator("https://a.example.com/").unwrap();
        broker.token_for(&acting(), &a, &[]).await.unwrap();
        let unrelated =
            RevocationScope::Resource(resource_indicator("https://z.example.com/").unwrap());
        assert_eq!(broker.revoke(&unrelated).await.unwrap(), 0);
        assert_eq!(broker.revoke(&RevocationScope::Agent(agent())).await.unwrap(), 1);
        broker.token_for(&acting(), &a, &[]).await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_misses_for_one_token_send_one_exchange() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200).set_delay(Duration::from_millis(200)).set_body_json(
                    serde_json::json!({"access_token": "shared", "expires_in": 3600}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let broker = Arc::new(broker(&server));
        let resource = resource_indicator("https://a.example.com/mcp").unwrap();

        let calls: Vec<_> = (0..8)
            .map(|_| {
                let (broker, resource) = (broker.clone(), resource.clone());
                tokio::spawn(async move { broker.token_for(&acting(), &resource, &[]).await })
            })
            .collect();
        for call in calls {
            assert_eq!(call.await.unwrap().unwrap().secret.expose(), "shared");
        }
        assert!(broker.lock_exchanges().is_empty(), "finished exchanges leave no gate behind");
    }

    /// Answers every exchange with a fresh token, `token-1`, `token-2`, ...
    struct Minting(std::sync::atomic::AtomicUsize);

    impl wiremock::Respond for Minting {
        fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
            let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"access_token": format!("token-{n}"), "expires_in": 3600}),
            )
        }
    }

    fn acting_for(subject: &str) -> ActingContext {
        acting().on_behalf_of(SubjectRef::human(subject))
    }

    fn form(request: &wiremock::Request) -> HashMap<String, String> {
        let body = String::from_utf8_lossy(&request.body);
        Url::parse(&format!("http://form.invalid/?{body}"))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }

    #[tokio::test]
    async fn two_subjects_never_share_a_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(Minting(Default::default()))
            .expect(2)
            .mount(&server)
            .await;
        let broker = broker(&server);
        let resource = resource_indicator("https://api.github.com/").unwrap();
        let scopes = vec!["repo".to_string()];

        let alice = broker.token_for(&acting_for("alice"), &resource, &scopes).await.unwrap();
        let bob = broker.token_for(&acting_for("bob"), &resource, &scopes).await.unwrap();
        assert_ne!(alice.secret, bob.secret, "bob is never served alice's token");
        let alice_again = broker.token_for(&acting_for("alice"), &resource, &scopes).await.unwrap();
        assert_eq!(alice_again, alice, "each subject's token is still cached");

        for request in server.received_requests().await.unwrap() {
            let form = form(&request);
            assert_eq!(form["subject_token"], "svid-jwt", "agent-only mode is unchanged");
            assert!(!form.contains_key("actor_token"));
        }
    }

    #[tokio::test]
    async fn the_delegation_chain_is_part_of_the_cache_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(Minting(Default::default()))
            .expect(2)
            .mount(&server)
            .await;
        let broker = broker(&server);
        let resource = resource_indicator("https://api.github.com/").unwrap();
        let orchestrator = AgentId::new("urn:adk:agent:orchestrator").unwrap();

        let direct = broker.token_for(&acting_for("alice"), &resource, &[]).await.unwrap();
        let delegated = broker
            .token_for(
                &acting_for("alice").delegated_by(SubjectRef::agent(&orchestrator)),
                &resource,
                &[],
            )
            .await
            .unwrap();
        assert_ne!(direct.secret, delegated.secret);
    }

    #[derive(Debug)]
    struct SignedIn;

    #[async_trait]
    impl SubjectTokenSource for SignedIn {
        async fn subject_token(
            &self,
            subject: &SubjectRef,
            acting: &ActingContext,
        ) -> adk_core::Result<SubjectToken> {
            assert_eq!(acting.on_behalf_of.as_ref(), Some(subject));
            Ok(SubjectToken::access_token(format!("{}-access-token", subject.id)))
        }
    }

    #[tokio::test]
    async fn on_behalf_of_sends_the_subject_token_and_the_agent_as_actor() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(Minting(Default::default()))
            .expect(2)
            .mount(&server)
            .await;
        let broker = broker(&server).on_behalf_of(Arc::new(SignedIn));
        let resource = resource_indicator("https://api.github.com/").unwrap();
        let scopes = vec!["repo".to_string()];

        let alice = broker.token_for(&acting_for("alice"), &resource, &scopes).await.unwrap();
        let bob = broker.token_for(&acting_for("bob"), &resource, &scopes).await.unwrap();
        assert_ne!(alice.secret, bob.secret);
        assert_eq!(alice.agent, agent());

        let requests = server.received_requests().await.unwrap();
        let sent: Vec<HashMap<String, String>> = requests.iter().map(form).collect();
        let expected = |subject: &str| -> HashMap<String, String> {
            [
                ("grant_type", GRANT_TYPE),
                ("resource", "https://api.github.com/"),
                ("audience", "https://api.github.com/"),
                ("subject_token", &format!("{subject}-access-token")),
                ("subject_token_type", TOKEN_TYPE_ACCESS),
                ("actor_token", "svid-jwt"),
                ("actor_token_type", TOKEN_TYPE_JWT),
                ("requested_token_type", TOKEN_TYPE_ACCESS),
                ("scope", "repo"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
        };
        assert_eq!(sent, vec![expected("alice"), expected("bob")]);
    }

    #[tokio::test]
    async fn on_behalf_of_refuses_a_run_acting_for_no_subject() {
        let server = MockServer::start().await;
        let error = broker(&server)
            .on_behalf_of(Arc::new(SignedIn))
            .token_for(&acting(), &resource_indicator("https://api.github.com/").unwrap(), &[])
            .await
            .unwrap_err();
        assert_eq!(error.code, "credential.subject_missing");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn revocation_by_agent_withdraws_every_subjects_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(Minting(Default::default()))
            .mount(&server)
            .await;
        let broker = broker(&server);
        let resource = resource_indicator("https://api.github.com/").unwrap();
        broker.token_for(&acting_for("alice"), &resource, &[]).await.unwrap();
        broker.token_for(&acting_for("bob"), &resource, &[]).await.unwrap();
        assert_eq!(broker.revoke(&RevocationScope::Agent(agent())).await.unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_token_endpoint_that_never_answers_fails_the_exchange() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let source = StaticIdentitySource::new(AgentPrincipal::new(agent()))
            .with_credential(AgentCredential::new(agent(), CredentialKind::JwtSvid, "svid-jwt"));
        let broker = TokenExchangeBroker::new(Url::parse(&endpoint).unwrap(), Arc::new(source));
        let resource = resource_indicator("https://a.example.com/mcp").unwrap();

        let outcome =
            tokio::time::timeout(REQUEST_TIMEOUT * 4, broker.token_for(&acting(), &resource, &[]))
                .await
                .expect("the exchange gives up on its own");
        assert_eq!(outcome.unwrap_err().code, "credential.exchange_failed");
    }
}
