//! The Bot Connector REST API (Bot Framework v3) and the Microsoft Entra ID token it takes.
//!
//! A message goes to `POST {serviceUrl}/v3/conversations/{conversationId}/activities`, a reply
//! to a card to `…/activities/{activityId}`, each with `Authorization: Bearer` and an access
//! token from the client-credentials grant for `https://api.botframework.com/.default`. The
//! app password and the access token are kept out of every error and log line.

use std::fmt;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::TeamsError;
use crate::channels::shared::{
    MAX_RATE_LIMIT_WAIT, RATE_LIMIT_ATTEMPTS, REQUEST_TIMEOUT, checked_base, checked_token,
};

/// The scope of a Bot Connector access token.
const CONNECTOR_SCOPE: &str = "https://api.botframework.com/.default";
/// The tenant a multi-tenant bot's token comes from.
const MULTI_TENANT: &str = "botframework.com";
/// How long before its expiry a cached access token is renewed.
const RENEW_BEFORE: Duration = Duration::from_secs(5 * 60);
/// Statuses Teams asks bots to retry with backoff: throttling, and 412, 502 and 504.
const RETRIED: [u16; 4] = [412, 429, 502, 504];

/// An access token and when it is renewed.
struct Cached {
    token: String,
    renew_at: Instant,
}

/// The client-credentials grant of one bot (Bot Framework "Authenticate requests from your
/// bot to the Bot Connector service").
pub(crate) struct TokenSource {
    client: reqwest::Client,
    url: String,
    app_id: String,
    password: String,
    cached: Mutex<Option<Cached>>,
}

impl fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSource").field("url", &self.url).finish_non_exhaustive()
    }
}

impl TokenSource {
    /// The grant at `login_base` for `app_id`, from `tenant` for a single-tenant bot or
    /// `botframework.com` for a multi-tenant one.
    pub(crate) fn new(
        client: reqwest::Client,
        login_base: &str,
        tenant: Option<&str>,
        app_id: &str,
        password: &str,
    ) -> Result<Self, String> {
        let base = checked_base(login_base, "the Microsoft Entra login address", false)?;
        let tenant = tenant.unwrap_or(MULTI_TENANT);
        Ok(Self {
            client,
            url: format!("{base}/{tenant}/oauth2/v2.0/token"),
            app_id: app_id.to_string(),
            password: password.to_string(),
            cached: Mutex::new(None),
        })
    }

    /// A current access token; a new one when `renew` or when the cached one is due.
    pub(crate) async fn token(&self, renew: bool) -> Result<String, TeamsError> {
        let mut cached = self.cached.lock().await;
        if !renew
            && let Some(current) = cached.as_ref()
            && Instant::now() < current.renew_at
        {
            return Ok(current.token.clone());
        }
        let fresh = self.fetch().await?;
        let token = fresh.token.clone();
        *cached = Some(fresh);
        Ok(token)
    }

    async fn fetch(&self) -> Result<Cached, TeamsError> {
        let form = [
            ("grant_type", "client_credentials"),
            ("client_id", self.app_id.as_str()),
            ("client_secret", self.password.as_str()),
            ("scope", CONNECTOR_SCOPE),
        ];
        let response =
            self.client.post(&self.url).form(&form).timeout(REQUEST_TIMEOUT).send().await.map_err(
                |error| TeamsError::Unreachable {
                    reason: self.redact(&error.without_url().to_string()),
                },
            )?;
        let status = response.status();
        let text = response.text().await.map_err(|error| TeamsError::Unreachable {
            reason: self.redact(&error.without_url().to_string()),
        })?;
        let reply: Value = serde_json::from_str(&text).map_err(|error| {
            TeamsError::InvalidResponse { reason: format!("token: HTTP {status}, {error}") }
        })?;
        if !status.is_success() {
            // `invalid_client`, `unauthorized_client`, a disabled app: the credentials.
            tracing::debug!(
                http.status = status.as_u16(),
                entra.error = reply["error"].as_str().unwrap_or_default(),
                "microsoft entra refused the bot's credentials"
            );
            return Err(match status.as_u16() {
                400 | 401 => TeamsError::Unauthorized,
                429 => TeamsError::RateLimited { status: 429 },
                _ => TeamsError::Unreachable { reason: format!("token: HTTP {status}") },
            });
        }
        let token = reply["access_token"].as_str().unwrap_or_default();
        checked_token(token, "the access token")
            .map_err(|_| TeamsError::InvalidResponse { reason: "token: no access_token".into() })?;
        let lifetime = Duration::from_secs(reply["expires_in"].as_u64().unwrap_or(3600));
        Ok(Cached {
            token: token.to_string(),
            renew_at: Instant::now() + lifetime.saturating_sub(RENEW_BEFORE),
        })
    }

    fn redact(&self, text: &str) -> String {
        text.replace(&self.password, "[password]")
    }
}

/// The Bot Connector API at the service addresses Teams gives each conversation.
pub(crate) struct ConnectorApi {
    client: reqwest::Client,
    tokens: TokenSource,
}

impl fmt::Debug for ConnectorApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectorApi").field("tokens", &self.tokens).finish_non_exhaustive()
    }
}

/// `{service}/v3/conversations/{conversation}/activities[/{reply_to}]`, each id one escaped
/// path segment.
pub(crate) fn activities_url(
    service: &str,
    conversation: &str,
    reply_to: Option<&str>,
) -> Option<reqwest::Url> {
    let mut url = reqwest::Url::parse(&format!("{}/", service.trim_end_matches('/'))).ok()?;
    {
        let mut segments = url.path_segments_mut().ok()?;
        segments.pop_if_empty().extend(["v3", "conversations", conversation, "activities"]);
        if let Some(id) = reply_to {
            segments.push(id);
        }
    }
    Some(url)
}

impl ConnectorApi {
    pub(crate) fn new(client: reqwest::Client, tokens: TokenSource) -> Self {
        Self { client, tokens }
    }

    /// Posts `activity` to `conversation` at `service`, as a reply to `reply_to` when set,
    /// waiting out throttling a few times when `patient`; returns the activity's id.
    pub(crate) async fn post(
        &self,
        service: &str,
        conversation: &str,
        reply_to: Option<&str>,
        activity: &Value,
        patient: bool,
    ) -> Result<String, TeamsError> {
        let url = activities_url(service, conversation, reply_to).ok_or_else(|| {
            TeamsError::Refused { status: 400, message: "the conversation id is malformed".into() }
        })?;
        let mut attempt = 1;
        let mut renewed = false;
        loop {
            let token = self.tokens.token(false).await?;
            match self.post_once(&url, &token, activity).await {
                Ok(id) => return Ok(id),
                // A token Teams no longer takes is renewed once.
                Err((TeamsError::Unauthorized, _)) if !renewed => {
                    renewed = true;
                    self.tokens.token(true).await?;
                }
                Err((TeamsError::RateLimited { status }, retry_after))
                    if attempt < RATE_LIMIT_ATTEMPTS =>
                {
                    let wait = retry_wait(attempt, retry_after);
                    if !patient || wait > MAX_RATE_LIMIT_WAIT {
                        return Err(TeamsError::RateLimited { status });
                    }
                    tracing::debug!(
                        attempt,
                        http.status = status,
                        "teams throttled a send, waiting"
                    );
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                Err((error, _)) => return Err(error),
            }
        }
    }

    async fn post_once(
        &self,
        url: &reqwest::Url,
        token: &str,
        activity: &Value,
    ) -> Result<String, (TeamsError, Option<Duration>)> {
        let unreachable = |error: reqwest::Error| {
            let reason = error.without_url().to_string().replace(token, "[token]");
            (TeamsError::Unreachable { reason }, None)
        };
        let response = self
            .client
            .post(url.clone())
            .bearer_auth(token)
            .json(activity)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let text = response.text().await.map_err(unreachable)?;
        if (200..300).contains(&status) {
            // A 201 or 202 may carry no body.
            let reply: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            return Ok(reply["id"].as_str().unwrap_or_default().to_string());
        }
        let reply: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let message = reply["error"]["message"]
            .as_str()
            .or_else(|| reply["message"].as_str())
            .unwrap_or("no message")
            .replace(token, "[token]");
        tracing::debug!(http.status = status, "teams refused a send");
        let error = match status {
            401 => TeamsError::Unauthorized,
            403 => TeamsError::Forbidden { message },
            status if RETRIED.contains(&status) => TeamsError::RateLimited { status },
            status if status >= 500 => {
                TeamsError::Unreachable { reason: format!("HTTP {status}: {message}") }
            }
            status => TeamsError::Refused { status, message },
        };
        Err((error, retry_after))
    }
}

/// How long to wait before attempt `attempt + 1`: Teams' `Retry-After` when it gave one, else
/// 1, 2, 4… seconds with up to a fifth more, so retries from many bots do not collide.
fn retry_wait(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(wait) = retry_after {
        return wait;
    }
    let base = Duration::from_secs(1u64 << (attempt - 1).min(5));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|now| now.subsec_nanos())
        .unwrap_or_default();
    base + base.mul_f64(f64::from(nanos % 200) / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_escaped_path_segments() {
        let url =
            activities_url("https://smba.trafficmanager.net/amer/", "a:1x/y?z", None).unwrap();
        assert_eq!(
            url.as_str(),
            "https://smba.trafficmanager.net/amer/v3/conversations/a:1x%2Fy%3Fz/activities"
        );
        let reply =
            activities_url("https://smba.trafficmanager.net/teams", "a:1", Some("17")).unwrap();
        assert_eq!(
            reply.as_str(),
            "https://smba.trafficmanager.net/teams/v3/conversations/a:1/activities/17"
        );
    }

    #[test]
    fn backoff_doubles_with_jitter_unless_teams_says_when() {
        assert_eq!(retry_wait(1, Some(Duration::from_secs(7))), Duration::from_secs(7));
        let first = retry_wait(1, None);
        assert!(
            first >= Duration::from_secs(1) && first < Duration::from_millis(1200),
            "{first:?}"
        );
        let second = retry_wait(2, None);
        assert!(second >= Duration::from_secs(2) && second < Duration::from_millis(2400));
    }

    #[test]
    fn the_password_never_reaches_an_error() {
        let tokens = TokenSource::new(
            reqwest::Client::new(),
            "https://login.microsoftonline.com",
            None,
            "app",
            "s3cret-pass",
        )
        .unwrap();
        assert_eq!(tokens.redact("bad s3cret-pass here"), "bad [password] here");
        assert!(!format!("{tokens:?}").contains("s3cret"));
        assert!(tokens.url.ends_with("/botframework.com/oauth2/v2.0/token"), "{}", tokens.url);
        let single = TokenSource::new(
            reqwest::Client::new(),
            "https://login.microsoftonline.com/",
            Some("72f988bf-86f1-41af-91ab-2d7cd011db47"),
            "app",
            "p",
        )
        .unwrap();
        assert_eq!(
            single.url,
            "https://login.microsoftonline.com/72f988bf-86f1-41af-91ab-2d7cd011db47/oauth2/v2.0/token"
        );
        assert!(
            TokenSource::new(reqwest::Client::new(), "http://login.example", None, "a", "p")
                .is_err()
        );
    }
}
