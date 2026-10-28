//! The WhatsApp Business Cloud API client: `POST /{phone-number-id}/messages` on the Graph
//! API. The access token travels in the `Authorization` header and is kept out of every error
//! and log line.

use std::fmt;
use std::time::Duration;

use serde_json::Value;

use super::WhatsAppError;
use crate::channels::shared::{RATE_LIMIT_ATTEMPTS, REQUEST_TIMEOUT, checked_base, checked_token};

/// Graph API error codes for throughput and spam limits: the send may go through later.
const RATE_LIMITED: [i64; 6] = [4, 80007, 130429, 131048, 131056, 133016];
/// The Graph API error code for a message outside the 24-hour customer-service window.
pub(crate) const OUTSIDE_WINDOW: i64 = 131047;

/// The Cloud API of one business phone number.
pub(crate) struct GraphApi {
    client: reqwest::Client,
    base: String,
    endpoint: String,
    token: String,
}

impl fmt::Debug for GraphApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphApi").field("endpoint", &self.endpoint).finish_non_exhaustive()
    }
}

impl GraphApi {
    /// The API at `base` for `phone_number_id` with `token`, or why they cannot be used.
    pub(crate) fn new(base: &str, phone_number_id: &str, token: &str) -> Result<Self, String> {
        checked_token(token, "the WhatsApp access token (WHATSAPP_ACCESS_TOKEN)")?;
        if phone_number_id.is_empty() || !phone_number_id.bytes().all(|b| b.is_ascii_digit()) {
            return Err("the WhatsApp phone number id (WHATSAPP_PHONE_NUMBER_ID) must be the \
                        number id from the app's WhatsApp settings, digits only"
                .into());
        }
        let base = checked_base(base, "the Graph API address", false)?;
        let client = reqwest::Client::builder().build().map_err(|error| {
            format!("the WhatsApp HTTP client could not be built: {}", error.without_url())
        })?;
        Ok(Self {
            client,
            endpoint: format!("{base}/{phone_number_id}/messages"),
            base,
            token: token.to_string(),
        })
    }

    /// Sends `body`, waiting out a throughput limit a few times when `patient`; returns the
    /// message's id (`wamid.…`).
    pub(crate) async fn send(&self, body: &Value, patient: bool) -> Result<String, WhatsAppError> {
        let mut attempt = 1;
        loop {
            match self.send_once(body).await {
                Err(WhatsAppError::RateLimited { .. })
                    if patient && attempt < RATE_LIMIT_ATTEMPTS =>
                {
                    tracing::debug!(attempt, "whatsapp throughput limit, waiting");
                    tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn send_once(&self, body: &Value) -> Result<String, WhatsAppError> {
        let unreachable = |error: reqwest::Error| WhatsAppError::Unreachable {
            reason: self.redact(&error.without_url().to_string()),
        };
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(body)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        let status = response.status();
        let text = response.text().await.map_err(unreachable)?;
        let reply: Value = serde_json::from_str(&text).map_err(|error| {
            WhatsAppError::InvalidResponse { reason: format!("HTTP {status}, {error}") }
        })?;
        if status.is_success() {
            return Ok(reply["messages"][0]["id"].as_str().unwrap_or_default().to_string());
        }
        let error = &reply["error"];
        let code = error["code"].as_i64().unwrap_or_else(|| i64::from(status.as_u16()));
        let message = self.redact(error["message"].as_str().unwrap_or("no message"));
        tracing::debug!(
            whatsapp.code = code,
            http.status = status.as_u16(),
            "whatsapp refused a send"
        );
        Err(match code {
            190 => WhatsAppError::Unauthorized,
            _ if status.as_u16() == 401 => WhatsAppError::Unauthorized,
            OUTSIDE_WINDOW => WhatsAppError::OutsideWindow,
            code if RATE_LIMITED.contains(&code) || status.as_u16() == 429 => {
                WhatsAppError::RateLimited { code }
            }
            code => WhatsAppError::Refused { code, message },
        })
    }

    /// Downloads the media `id` a message carried, an image within the limits (spec 3.5 §10
    /// X12): its address from the Graph API, then its bytes, both with the token.
    pub(crate) async fn download(
        &self,
        id: &str,
        declared: Option<&str>,
    ) -> Option<adk_kernel::channels::ChannelAttachment> {
        use crate::channels::media;
        if id.is_empty()
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return None;
        }
        let found = self.client.get(format!("{}/{id}", self.base)).bearer_auth(&self.token);
        let found: Value = found.timeout(REQUEST_TIMEOUT).send().await.ok()?.json().await.ok()?;
        if found["file_size"].as_u64().is_some_and(|size| size > media::MAX_BYTES as u64) {
            return None;
        }
        let url = found["url"].as_str()?;
        // The type the Graph API reports for the file wins over the webhook's.
        let declared = found["mime_type"].as_str().or(declared);
        let request = self.client.get(url).bearer_auth(&self.token);
        let (media_type, bytes) = media::download(request, declared).await?;
        let name = media::name(id, &media_type);
        Some(adk_kernel::channels::ChannelAttachment::new(media_type, name, bytes))
    }

    fn redact(&self, text: &str) -> String {
        text.replace(&self.token, "[token]")
    }
}
