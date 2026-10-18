//! RFC 8693 token exchange as a [`CredentialBroker`].
//!
//! The broker presents the agent's own credential (from an [`IdentitySource`]) as the
//! `subject_token` and asks the authorization server for a token bound to one resource
//! (RFC 8707 `resource`) and a set of scopes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ActingContext, AdkError, AgentId, CredentialBroker, CredentialKind, CredentialSecret,
    ErrorCategory, ErrorComponent, IdentitySource, RevocationScope, ScopedToken, Url, async_trait,
};
use chrono::Utc;
use serde::Deserialize;
use tokio::sync::RwLock;

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_JWT: &str = "urn:ietf:params:oauth:token-type:jwt";
const TOKEN_TYPE_ACCESS: &str = "urn:ietf:params:oauth:token-type:access_token";

/// Tokens are refreshed this long before the server says they expire.
const EXPIRY_SKEW: Duration = Duration::from_secs(30);

/// Longest time one token or revocation request may take by default.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

type CacheKey = (AgentId, Url, Vec<String>);

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
/// | Request field | Value |
/// |---------------|-------|
/// | `grant_type` | `urn:ietf:params:oauth:grant-type:token-exchange` |
/// | `resource`, `audience` | The requested resource |
/// | `scope` | The requested scopes, space-separated (omitted when empty) |
/// | `subject_token` | [`IdentitySource::credential`] |
/// | `subject_token_type` | `…:token-type:jwt` for a JWT-SVID, `…:token-type:access_token` for an opaque credential |
///
/// Issued tokens are cached per agent, resource, and scope set until shortly before
/// they expire. Concurrent requests for a token that is not cached send one exchange
/// and share its result. [`revoke`](CredentialBroker::revoke) drops cached tokens and,
/// when a revocation endpoint is configured, revokes each one there (RFC 7009).
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
        let key = (acting.agent.id.clone(), resource.clone(), sorted);
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
        let subject_token_type = match credential.kind {
            CredentialKind::JwtSvid => TOKEN_TYPE_JWT,
            CredentialKind::Opaque => TOKEN_TYPE_ACCESS,
            other => {
                return Err(exchange_error(
                    ErrorCategory::Unsupported,
                    "credential.exchange_unsupported",
                    format!(
                        "a {other:?} credential cannot be a token-exchange subject_token; use a \
                         JWT-SVID or an opaque access token"
                    ),
                ));
            }
        };

        let scope = key.2.join(" ");
        let mut form = vec![
            ("grant_type", GRANT_TYPE),
            ("resource", resource.as_str()),
            ("audience", resource.as_str()),
            ("subject_token", credential.secret.expose()),
            ("subject_token_type", subject_token_type),
            ("requested_token_type", TOKEN_TYPE_ACCESS),
        ];
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
            None => key.2.clone(),
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
                .filter(|(agent, resource, _)| scope.covers(agent, resource))
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
