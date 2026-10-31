//! The Discord REST API client (`/api/v10`): the bot token travels in the `Authorization`
//! header and is kept out of every error and log line.

use std::fmt;
use std::time::Duration;

use reqwest::Method;
use serde_json::Value;

use super::DiscordError;
use crate::channels::shared::{
    MAX_RATE_LIMIT_WAIT, RATE_LIMIT_ATTEMPTS, REQUEST_TIMEOUT, checked_base, checked_token,
};

/// The REST API of one Discord bot.
pub(crate) struct DiscordApi {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl fmt::Debug for DiscordApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordApi").field("base", &self.base).finish_non_exhaustive()
    }
}

impl DiscordApi {
    /// The API at `base` for the bot with `token`, or why they cannot be used.
    pub(crate) fn new(base: &str, token: &str) -> Result<Self, String> {
        checked_token(token, "the Discord bot token (DISCORD_BOT_TOKEN)")?;
        let base = checked_base(base, "the Discord API address", false)?;
        let client = reqwest::Client::builder()
            .user_agent("DiscordBot (https://github.com/zavora-ai/adk-rust, 3.0)")
            .build()
            .map_err(|error| {
                format!("the Discord HTTP client could not be built: {}", error.without_url())
            })?;
        Ok(Self { client, base, token: token.to_string() })
    }

    /// The token, for the Gateway's identify and resume.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// The Gateway's address (`GET /gateway/bot`), which also checks the token.
    pub(crate) async fn gateway_url(&self) -> Result<String, DiscordError> {
        let reply = self.request(Method::GET, "/gateway/bot", None, false).await?;
        reply["url"].as_str().map(str::to_string).ok_or_else(|| DiscordError::InvalidResponse {
            route: "/gateway/bot".into(),
            reason: "no url".into(),
        })
    }

    /// Calls `route` with `body`, waiting out a short rate limit when `patient`.
    pub(crate) async fn request(
        &self,
        method: Method,
        route: &str,
        body: Option<&Value>,
        patient: bool,
    ) -> Result<Value, DiscordError> {
        let mut attempt = 1;
        loop {
            match self.request_once(method.clone(), route, body).await {
                Err(DiscordError::RateLimited { retry_after_ms, .. })
                    if patient
                        && attempt < RATE_LIMIT_ATTEMPTS
                        && Duration::from_millis(retry_after_ms) <= MAX_RATE_LIMIT_WAIT =>
                {
                    tracing::debug!(
                        discord.route = route,
                        retry.after_ms = retry_after_ms,
                        "discord rate limit, waiting"
                    );
                    tokio::time::sleep(Duration::from_millis(retry_after_ms.max(100))).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn request_once(
        &self,
        method: Method,
        route: &str,
        body: Option<&Value>,
    ) -> Result<Value, DiscordError> {
        // Interaction webhooks are named by the interaction's token, which is not shown.
        let shown = route.split("/webhooks/").next().unwrap_or(route).to_string();
        let unreachable = |error: reqwest::Error| DiscordError::Unreachable {
            route: shown.clone(),
            reason: self.redact(&error.without_url().to_string()),
        };
        let mut request = self
            .client
            .request(method, format!("{}{route}", self.base))
            .header("Authorization", format!("Bot {}", self.token))
            .timeout(REQUEST_TIMEOUT);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(unreachable)?;
        let status = response.status();
        let header_wait = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<f64>().ok());
        let text = response.text().await.map_err(unreachable)?;
        if status.is_success() {
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).map_err(|error| DiscordError::InvalidResponse {
                route: shown,
                reason: format!("HTTP {status}, {error}"),
            });
        }
        let reply: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        tracing::debug!(discord.route = %shown, http.status = status.as_u16(), "discord refused a call");
        Err(match status.as_u16() {
            401 => DiscordError::Unauthorized,
            429 => {
                let seconds = reply["retry_after"].as_f64().or(header_wait).unwrap_or(1.0);
                DiscordError::RateLimited {
                    route: shown,
                    retry_after_ms: (seconds.max(0.0) * 1000.0).ceil() as u64,
                }
            }
            code => DiscordError::Refused {
                route: shown,
                status: code,
                code: reply["code"].as_i64().unwrap_or_default(),
                message: self.redact(reply["message"].as_str().unwrap_or("no message")),
            },
        })
    }

    /// Downloads `file`, an attachment of a message, from Discord's CDN — or the API
    /// address's own host, a test double — without the bot token.
    pub(crate) async fn download(
        &self,
        file: crate::channels::media::FileRef,
    ) -> Option<(String, Vec<u8>)> {
        let url = reqwest::Url::parse(&file.url).ok()?;
        let base = reqwest::Url::parse(&self.base).ok()?;
        let cdn = url.scheme() == "https"
            && matches!(url.host_str(), Some("cdn.discordapp.com" | "media.discordapp.net"));
        if !cdn && url.origin() != base.origin() {
            tracing::debug!("a discord attachment served from another host was not downloaded");
            return None;
        }
        crate::channels::media::download(self.client.get(url), file.media_type.as_deref()).await
    }

    fn redact(&self, text: &str) -> String {
        text.replace(&self.token, "[token]")
    }
}
