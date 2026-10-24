//! The Telegram Bot API client: one JSON `POST` per method. The bot token travels in the
//! request path, so it is kept out of every error and log line.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use reqwest::Url;
use serde_json::Value;

use super::TelegramError;

/// The longest wait a rate limit may ask for before the call gives up instead of waiting.
const MAX_RATE_LIMIT_WAIT: u64 = 30;
/// How many times one call is tried while Telegram rate-limits it.
const RATE_LIMIT_ATTEMPTS: u32 = 3;

/// The Bot API of one bot.
pub(crate) struct BotApi {
    client: reqwest::Client,
    /// `{base}/bot{token}/`: never logged or shown.
    endpoint: String,
    token: String,
}

impl fmt::Debug for BotApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotApi").finish_non_exhaustive()
    }
}

impl BotApi {
    /// The API at `base` for the bot with `token`, or why they cannot be used.
    pub(crate) fn new(base: &str, token: &str) -> Result<Self, String> {
        // The token becomes part of the request path, so only the characters of a
        // `@BotFather` token are taken.
        let well_formed = token.contains(':')
            && token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-'));
        if !well_formed {
            return Err(
                "the Telegram bot token is empty or malformed: copy it from @BotFather, as \
                 `123456789:AA…`"
                    .into(),
            );
        }
        let base = base.trim_end_matches('/');
        let url = Url::parse(base)
            .map_err(|error| format!("the Telegram API address `{base}` is not a URL: {error}"))?;
        match url.scheme() {
            "https" => {}
            "http" if is_loopback(&url) => {}
            "http" => {
                return Err(format!(
                    "the Telegram API address `{base}` must use https: the bot token travels in \
                     the request path (plain http is accepted on loopback only)"
                ));
            }
            other => {
                return Err(format!(
                    "the Telegram API address `{base}` uses `{other}`: use https://api.telegram.org"
                ));
            }
        }
        let client = reqwest::Client::builder().build().map_err(|error| {
            format!("the Telegram HTTP client could not be built: {}", error.without_url())
        })?;
        Ok(Self { client, endpoint: format!("{base}/bot{token}/"), token: token.to_string() })
    }

    /// Calls `method` with `body`, waiting out a short rate limit, and returns its `result`.
    pub(crate) async fn call(
        &self,
        method: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<Value, TelegramError> {
        let mut attempt = 1;
        loop {
            match self.call_once(method, body, timeout).await {
                Err(TelegramError::RateLimited { retry_after, .. })
                    if attempt < RATE_LIMIT_ATTEMPTS && retry_after <= MAX_RATE_LIMIT_WAIT =>
                {
                    tracing::debug!(
                        telegram.method = method,
                        retry.after_secs = retry_after,
                        "telegram rate limit, waiting"
                    );
                    tokio::time::sleep(Duration::from_secs(retry_after.max(1))).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn call_once(
        &self,
        method: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<Value, TelegramError> {
        let unreachable = |error: reqwest::Error| TelegramError::Unreachable {
            method: method.to_string(),
            reason: self.redact(&error.without_url().to_string()),
        };
        let response = self
            .client
            .post(format!("{}{method}", self.endpoint))
            .json(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(unreachable)?;
        let status = response.status();
        let text = response.text().await.map_err(unreachable)?;
        let reply: Value =
            serde_json::from_str(&text).map_err(|error| TelegramError::InvalidResponse {
                method: method.to_string(),
                reason: format!("HTTP {status}, {error}"),
            })?;
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
        }
        let code = reply
            .get("error_code")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| i64::from(status.as_u16()));
        let description = self
            .redact(reply.get("description").and_then(Value::as_str).unwrap_or("no description"));
        tracing::debug!(telegram.method = method, telegram.code = code, "telegram refused a call");
        Err(match code {
            // Telegram answers a wrong token with 401, and a malformed one with 404.
            401 | 404 => TelegramError::Unauthorized,
            409 => TelegramError::Conflict,
            429 => TelegramError::RateLimited {
                method: method.to_string(),
                retry_after: reply
                    .pointer("/parameters/retry_after")
                    .and_then(Value::as_u64)
                    .unwrap_or(1),
            },
            _ => TelegramError::Refused { method: method.to_string(), code, description },
        })
    }

    fn redact(&self, text: &str) -> String {
        text.replace(&self.token, "[token]")
    }
}

/// Whether `url` names this machine.
fn is_loopback(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "123456789:AAH-secret_part";

    #[test]
    fn the_token_and_the_address_are_checked() {
        assert!(BotApi::new("https://api.telegram.org", TOKEN).is_ok());
        assert!(BotApi::new("http://127.0.0.1:8081/", TOKEN).is_ok());
        assert!(BotApi::new("http://[::1]:8081", TOKEN).is_ok());
        assert!(BotApi::new("http://localhost:8081", TOKEN).is_ok());
        for base in ["http://api.telegram.org", "ftp://api.telegram.org", "not a url"] {
            let error = BotApi::new(base, TOKEN).unwrap_err();
            assert!(!error.contains(TOKEN), "{error}");
        }
        for token in ["", "no-colon", "123:abc/../../x", "123:abc?x=1", "123:abc def"] {
            assert!(BotApi::new("https://api.telegram.org", token).is_err(), "{token:?}");
        }
    }

    #[tokio::test]
    async fn an_unreachable_api_never_shows_the_token() {
        // Port 9 on loopback (discard) refuses connections on every CI runner.
        let api = BotApi::new("http://127.0.0.1:9", TOKEN).unwrap();
        let error =
            api.call("getMe", &serde_json::json!({}), Duration::from_secs(2)).await.unwrap_err();
        assert!(matches!(error, TelegramError::Unreachable { .. }), "{error:?}");
        assert!(!error.to_string().contains("secret_part"), "{error}");
        assert!(!format!("{api:?}").contains("secret_part"));
    }
}
