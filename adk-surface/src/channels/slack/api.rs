//! The Slack Web API client: one JSON `POST` per method, the bot token for messages and the
//! app-level token for `apps.connections.open`. Both travel in a header and are kept out of
//! every error and log line.

use std::fmt;
use std::time::Duration;

use serde_json::Value;

use super::SlackError;
use crate::channels::shared::{
    MAX_RATE_LIMIT_WAIT, RATE_LIMIT_ATTEMPTS, REQUEST_TIMEOUT, checked_base, checked_token,
};

/// Which token a method takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Token {
    /// The app-level token (`xapp-…`), for Socket Mode.
    App,
    /// The bot token (`xoxb-…`), for everything else.
    Bot,
}

/// The Web API of one Slack app.
pub(crate) struct SlackApi {
    client: reqwest::Client,
    base: String,
    app_token: String,
    bot_token: String,
}

impl fmt::Debug for SlackApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackApi").field("base", &self.base).finish_non_exhaustive()
    }
}

impl SlackApi {
    /// The API at `base` for the app with `app_token` and `bot_token`, or why they cannot be
    /// used.
    pub(crate) fn new(base: &str, app_token: &str, bot_token: &str) -> Result<Self, String> {
        checked_token(app_token, "the Slack app-level token (SLACK_APP_TOKEN)")?;
        checked_token(bot_token, "the Slack bot token (SLACK_BOT_TOKEN)")?;
        let base = checked_base(base, "the Slack API address", false)?;
        let client = reqwest::Client::builder().build().map_err(|error| {
            format!("the Slack HTTP client could not be built: {}", error.without_url())
        })?;
        Ok(Self {
            client,
            base,
            app_token: app_token.to_string(),
            bot_token: bot_token.to_string(),
        })
    }

    /// The Socket Mode address to connect to (`apps.connections.open`).
    pub(crate) async fn open_socket(&self) -> Result<String, SlackError> {
        let reply = self.call_once("apps.connections.open", &Value::Null, Token::App).await?;
        reply.get("url").and_then(Value::as_str).map(str::to_string).ok_or_else(|| {
            SlackError::InvalidResponse {
                method: "apps.connections.open".into(),
                reason: "no url".into(),
            }
        })
    }

    /// Calls `method` with `body` and the bot token, waiting out a short rate limit when
    /// `patient`.
    pub(crate) async fn call(
        &self,
        method: &str,
        body: &Value,
        patient: bool,
    ) -> Result<Value, SlackError> {
        let mut attempt = 1;
        loop {
            match self.call_once(method, body, Token::Bot).await {
                Err(SlackError::RateLimited { retry_after, .. })
                    if patient
                        && attempt < RATE_LIMIT_ATTEMPTS
                        && Duration::from_secs(retry_after) <= MAX_RATE_LIMIT_WAIT =>
                {
                    tracing::debug!(
                        slack.method = method,
                        retry.after_secs = retry_after,
                        "slack rate limit, waiting"
                    );
                    tokio::time::sleep(Duration::from_secs(retry_after.max(1))).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    /// Calls `method` once with `body` and `token`, and returns Slack's answer.
    pub(crate) async fn call_once(
        &self,
        method: &str,
        body: &Value,
        token: Token,
    ) -> Result<Value, SlackError> {
        let unreachable = |error: reqwest::Error| SlackError::Unreachable {
            method: method.to_string(),
            reason: self.redact(&error.without_url().to_string()),
        };
        let token = match token {
            Token::App => &self.app_token,
            Token::Bot => &self.bot_token,
        };
        let mut request = self
            .client
            .post(format!("{}/{method}", self.base))
            .bearer_auth(token)
            .timeout(REQUEST_TIMEOUT);
        if !body.is_null() {
            request = request.json(body);
        }
        let response = request.send().await.map_err(unreachable)?;
        let status = response.status();
        if status.as_u16() == 429 {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(1);
            return Err(SlackError::RateLimited { method: method.to_string(), retry_after });
        }
        let text = response.text().await.map_err(unreachable)?;
        let reply: Value =
            serde_json::from_str(&text).map_err(|error| SlackError::InvalidResponse {
                method: method.to_string(),
                reason: format!("HTTP {status}, {error}"),
            })?;
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(reply);
        }
        let error = self.redact(reply.get("error").and_then(Value::as_str).unwrap_or("unknown"));
        tracing::debug!(slack.method = method, slack.error = %error, "slack refused a call");
        Err(match error.as_str() {
            "not_authed"
            | "invalid_auth"
            | "account_inactive"
            | "token_revoked"
            | "token_expired"
            | "not_allowed_token_type" => SlackError::Unauthorized,
            "ratelimited" => SlackError::RateLimited { method: method.to_string(), retry_after: 1 },
            _ => SlackError::Refused { method: method.to_string(), error },
        })
    }

    /// Downloads `file`, a file a message shared, with the bot token, when Slack serves it:
    /// from `files.slack.com`, or the API address's own host (a test double). The token is
    /// sent nowhere else.
    pub(crate) async fn download(
        &self,
        file: crate::channels::media::FileRef,
    ) -> Option<(String, Vec<u8>)> {
        let url = reqwest::Url::parse(&file.url).ok()?;
        let base = reqwest::Url::parse(&self.base).ok()?;
        let slack = url.scheme() == "https" && url.host_str() == Some("files.slack.com");
        if !slack && url.origin() != base.origin() {
            tracing::debug!("a slack file served from another host was not downloaded");
            return None;
        }
        let request = self.client.get(url).bearer_auth(&self.bot_token);
        crate::channels::media::download(request, file.media_type.as_deref()).await
    }

    fn redact(&self, text: &str) -> String {
        text.replace(&self.app_token, "[token]").replace(&self.bot_token, "[token]")
    }
}
