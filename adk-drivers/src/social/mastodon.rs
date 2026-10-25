//! Social through the Mastodon client API (spec 3.5 §7.13; feature `mastodon`).

use std::cmp::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::{MAX_ITEM_CHARS, SocialDriver, SocialItem, SocialPage, SocialPosted};
use crate::email::mime::strip_html;
use crate::error::{DriverError, Result};
use crate::http::{checked_base, http_client, read_json};
use crate::outbound::bounded;

const SERVICE: &str = "mastodon";

/// A social account on an ActivityPub server through the Mastodon client API (spec 3.5
/// §7.13): Mastodon itself, GoToSocial, Pleroma and Akkoma serve it, so tests run against a
/// server on loopback and owners keep their choice of instance.
///
/// | Contract | API |
/// |---|---|
/// | Mentions and messages | `GET /api/v1/notifications?types[]=mention`, split by the status's visibility; the cursor is the last notification id |
/// | Reply | `POST /api/v1/statuses` with `in_reply_to_id`, the item's visibility and its author mentioned, under an `Idempotency-Key` |
/// | Post | `POST /api/v1/statuses`, public |
///
/// The access token comes from the credential broker's OAuth flow (A-I19).
///
/// # Example
///
/// ```rust,no_run
/// use adk_drivers::social::{MastodonSocial, SocialDriver};
///
/// # async fn example(token: String) -> adk_drivers::Result<()> {
/// let social = MastodonSocial::new("https://social.example", token)?;
/// let mentions = social.mentions(None, 20).await?;
/// # let _ = mentions;
/// # Ok(())
/// # }
/// ```
pub struct MastodonSocial {
    http: reqwest::Client,
    base: String,
    token: String,
    max_chars: usize,
}

impl std::fmt::Debug for MastodonSocial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MastodonSocial").field("base", &self.base).finish_non_exhaustive()
    }
}

impl MastodonSocial {
    /// The account whose access token is `token` on the server at `base`.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an address that is not `https` (or `http` on
    /// loopback) or an empty token.
    pub fn new(base: &str, token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        if token.is_empty() {
            return Err(DriverError::invalid("a Mastodon account needs an access token"));
        }
        Ok(Self {
            http: http_client(SERVICE, Duration::from_secs(20))?,
            base: checked_base(base)?,
            token,
            max_chars: 500,
        })
    }

    /// Sets the longest post the server takes; 500 by default, as Mastodon's.
    #[must_use]
    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        self.max_chars = max_chars;
        self
    }

    async fn notifications(
        &self,
        direct: bool,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<SocialPage> {
        let mut request = self
            .http
            .get(format!("{}api/v1/notifications", self.base))
            .bearer_auth(&self.token)
            .query(&[("types[]", "mention"), ("limit", &limit.clamp(1, 40).to_string())]);
        if let Some(cursor) = cursor {
            request = request.query(&[("min_id", cursor)]);
        }
        let response =
            request.send().await.map_err(|error| DriverError::unavailable(SERVICE, error))?;
        let answer = read_json(SERVICE, response).await?;
        let mut notifications: Vec<&Value> = answer
            .as_array()
            .ok_or_else(|| DriverError::protocol(SERVICE, "notifications are not a list"))?
            .iter()
            .collect();
        notifications.sort_by(|a, b| id_order(id_of(a), id_of(b)));
        let cursor = notifications
            .last()
            .map(|last| id_of(last).to_string())
            .or_else(|| cursor.map(str::to_string));
        let items = notifications
            .into_iter()
            .filter_map(|notification| item_of(notification.get("status")?))
            .filter(|item| item.is_direct() == direct)
            .collect();
        Ok(SocialPage { items, cursor })
    }

    async fn status(&self, body: Value) -> Result<SocialPosted> {
        let response = self
            .http
            .post(format!("{}api/v1/statuses", self.base))
            .bearer_auth(&self.token)
            .header("Idempotency-Key", uuid::Uuid::new_v4().to_string())
            .json(&body)
            .send()
            .await
            .map_err(|error| DriverError::unavailable(SERVICE, error))?;
        let status = read_json(SERVICE, response).await?;
        let id = status
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| DriverError::protocol(SERVICE, "the new status has no id"))?;
        Ok(SocialPosted {
            id: id.to_string(),
            url: status.get("url").and_then(Value::as_str).map(str::to_string),
            at: time_of(&status).unwrap_or_else(Utc::now),
        })
    }
}

fn id_of(value: &Value) -> &str {
    value.get("id").and_then(Value::as_str).unwrap_or_default()
}

/// Orders ids as the numbers Mastodon makes them, falling back to text.
fn id_order(a: &str, b: &str) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn time_of(value: &Value) -> Option<DateTime<Utc>> {
    let text = value.get("created_at")?.as_str()?;
    DateTime::parse_from_rfc3339(text).ok().map(|time| time.with_timezone(&Utc))
}

/// A status as an item.
pub(crate) fn item_of(status: &Value) -> Option<SocialItem> {
    let id = status.get("id")?.as_str()?.to_string();
    let author = status.get("account")?.get("acct")?.as_str()?.to_string();
    let html = status.get("content").and_then(Value::as_str).unwrap_or_default();
    Some(SocialItem {
        id,
        author,
        text: bounded(&strip_html(html), MAX_ITEM_CHARS),
        at: time_of(status).unwrap_or_else(Utc::now),
        visibility: status
            .get("visibility")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string(),
        url: status.get("url").and_then(Value::as_str).map(str::to_string),
    })
}

#[async_trait]
impl SocialDriver for MastodonSocial {
    fn name(&self) -> &str {
        SERVICE
    }

    fn max_chars(&self) -> usize {
        self.max_chars
    }

    async fn mentions(&self, cursor: Option<&str>, limit: usize) -> Result<SocialPage> {
        self.notifications(false, cursor, limit).await
    }

    async fn messages(&self, cursor: Option<&str>, limit: usize) -> Result<SocialPage> {
        self.notifications(true, cursor, limit).await
    }

    async fn reply(&self, item: &SocialItem, text: &str) -> Result<SocialPosted> {
        let visibility = match item.visibility.as_str() {
            "direct" | "private" | "unlisted" => item.visibility.as_str(),
            _ => "unlisted",
        };
        self.status(json!({
            "status": format!("@{} {text}", item.author),
            "in_reply_to_id": item.id,
            "visibility": visibility,
        }))
        .await
    }

    async fn post(&self, text: &str) -> Result<SocialPosted> {
        self.status(json!({ "status": text, "visibility": "public" })).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_becomes_an_item_without_markup() {
        let status = json!({
            "id": "109", "visibility": "direct", "created_at": "2026-10-20T08:00:00.000Z",
            "account": {"acct": "ana@social.example"},
            "content": "<p><span class=\"h-card\"><a href=\"https://site.example/@site\" class=\"u-url mention\">@<span>site</span></a></span> open on Sunday? &amp; parking?</p>"
        });
        let item = item_of(&status).unwrap();
        assert_eq!(item.text, "@site open on Sunday? & parking?");
        assert!(item.is_direct());
        assert_eq!(id_order("99", "100"), Ordering::Less);
    }
}
