//! What the cloud exchanges share: the HTTP round trip, OAuth token responses, and the
//! token cache behind the [`CredentialBroker`](adk_core::CredentialBroker) implementations.

use std::collections::HashMap;
use std::time::Duration;

use adk_core::{AgentId, CredentialSecret, RevocationScope, ScopedToken, Url};
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use super::{CloudProvider, FederationError};

/// Longest time one exchange request may take by default.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Cached tokens are replaced this long before the cloud says they expire.
const EXPIRY_SKEW: Duration = Duration::from_secs(30);

/// Sent with every exchange. Amazon Bedrock AgentCore's front end answers a request without
/// a `User-Agent` with an HTML 403 before the service sees it.
const USER_AGENT: &str = concat!("adk-auth/", env!("CARGO_PKG_VERSION"));

/// A cloud's error text as an error carries it: at most 512 characters.
pub(crate) fn shorten(message: &str) -> String {
    message.chars().take(512).collect()
}

/// Sends `request` and returns the status, headers and body of the answer.
pub(crate) async fn send(
    provider: CloudProvider,
    endpoint: &str,
    request: reqwest::RequestBuilder,
) -> Result<(u16, reqwest::header::HeaderMap, String), FederationError> {
    let transport = |error: reqwest::Error| FederationError::Transport {
        provider,
        endpoint: endpoint.to_string(),
        // The URL is dropped so a query string never reaches an error message.
        reason: error.without_url().to_string(),
    };
    let response = request.header("user-agent", USER_AGENT).send().await.map_err(transport)?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response.text().await.map_err(transport)?;
    Ok((status, headers, body))
}

/// Posts `form` to an OAuth 2.0 token endpoint and returns the access token and its expiry.
///
/// A refusal carries the endpoint's `error` as the code and its `error_description` as the
/// message (RFC 6749 §5.2); Google's `{"error": {"status", "message"}}` form is read too.
pub(crate) async fn oauth_token(
    provider: CloudProvider,
    client: &reqwest::Client,
    endpoint: &str,
    form: &[(&str, &str)],
) -> Result<(CredentialSecret, DateTime<Utc>), FederationError> {
    let (status, _, body) = send(provider, endpoint, client.post(endpoint).form(form)).await?;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    if !(200..300).contains(&status) {
        let (code, message) = match &json["error"] {
            serde_json::Value::String(code) => {
                (code.clone(), json["error_description"].as_str().unwrap_or_default().to_string())
            }
            serde_json::Value::Object(error) => (
                error.get("status").and_then(|v| v.as_str()).unwrap_or("error").to_string(),
                error.get("message").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            ),
            _ => (format!("http_{status}"), body.clone()),
        };
        return Err(FederationError::Refused {
            provider,
            status,
            code,
            message: shorten(&message),
        });
    }
    let invalid = |reason: &str| FederationError::InvalidResponse {
        provider,
        reason: format!("the token response {reason}"),
    };
    let access_token = json["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| invalid("has no `access_token`"))?;
    // `expires_in` is a number (RFC 6749 §5.1); some endpoints send it as a string.
    let expires_in = json["expires_in"]
        .as_i64()
        .or_else(|| json["expires_in"].as_str().and_then(|text| text.parse().ok()))
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| invalid("has no `expires_in`"))?;
    Ok((CredentialSecret::new(access_token), Utc::now() + chrono::Duration::seconds(expires_in)))
}

/// The agent, the resource and the sorted scopes a brokered token was issued for.
pub(crate) type CacheKey = (AgentId, Url, Vec<String>);

/// Brokered tokens by [`CacheKey`].
///
/// The exchanges mint tokens for the agent alone, never for the subject it acts for, so the
/// subject is not part of the key.
#[derive(Debug, Default)]
pub(crate) struct TokenCache(RwLock<HashMap<CacheKey, ScopedToken>>);

impl TokenCache {
    /// The cached token for `key`, unless it expires within [`EXPIRY_SKEW`].
    pub(crate) async fn live(&self, key: &CacheKey) -> Option<ScopedToken> {
        let refresh_at = Utc::now() + chrono::Duration::from_std(EXPIRY_SKEW).unwrap_or_default();
        self.0.read().await.get(key).filter(|token| token.is_live(refresh_at)).cloned()
    }

    pub(crate) async fn store(&self, key: CacheKey, token: ScopedToken) {
        self.0.write().await.insert(key, token);
    }

    /// Drops the tokens in `scope` and returns how many there were.
    pub(crate) async fn revoke(&self, scope: &RevocationScope) -> u64 {
        let mut cache = self.0.write().await;
        let before = cache.len();
        cache.retain(|(agent, resource, _), _| !scope.covers(agent, resource));
        (before - cache.len()) as u64
    }
}
