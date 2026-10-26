//! Web Push from the person's own kernel (spec 3.3 §5.4, §11 S-I5): which push services a
//! phone may subscribe through, the request each wake-up sends (aes128gcm, VAPID, `Urgency:
//! high`), and the one body it carries, which says nothing about any decision.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use axum::http::{HeaderValue, Uri};
use base64ct::{Base64UrlUnpadded, Encoding as _};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use web_push_native::{Auth, WebPushBuilder};

use super::error::MobileError;
use super::vapid::Vapid;
use crate::web::phone::Delivery;

/// The push services of Apple, Google, Mozilla and Microsoft, the only ones a phone's browser
/// subscribes through.
const PUSH_SERVICES: [&str; 4] = [
    "*.push.apple.com",
    "fcm.googleapis.com",
    "*.push.services.mozilla.com",
    "*.notify.windows.com",
];

/// How long a push service keeps a wake-up for a phone that is offline.
pub(crate) const PUSH_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// The longest endpoint accepted.
const MAX_ENDPOINT: usize = 2048;
/// How many deliveries the operator can read back.
const DELIVERIES_KEPT: usize = 256;
/// How many sent wake-ups wait for their acknowledgement.
const SENT_KEPT: usize = 1024;

/// The push services a phone may subscribe through (spec 3.3 §5.4): the browsers' own, over
/// https, never an IP address. A subscription to anything else is refused, so a wake-up never
/// goes to a host the person did not choose by installing a browser.
///
/// # Example
///
/// ```rust
/// use adk_surface::mobile::PushAllowlist;
///
/// let allow = PushAllowlist::default();
/// assert!(allow.allows("https://web.push.apple.com/QGu1…"));
/// assert!(!allow.allows("http://web.push.apple.com/QGu1…"));
/// assert!(!allow.allows("https://192.0.2.7/push"));
/// assert!(!allow.allows("https://push.example.com/push"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushAllowlist {
    hosts: Vec<String>,
    http_localhost: bool,
}

impl Default for PushAllowlist {
    /// Apple's, Google's, Mozilla's and Microsoft's push services.
    fn default() -> Self {
        Self::new(PUSH_SERVICES)
    }
}

impl PushAllowlist {
    /// These hosts, over https: `fcm.googleapis.com` names one host, `*.push.apple.com` every
    /// host under that domain.
    pub fn new(hosts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            hosts: hosts.into_iter().map(|host| host.into().to_ascii_lowercase()).collect(),
            http_localhost: false,
        }
    }

    /// Also allows plain http to `localhost`, for tests against a local fake push service.
    /// Never set it on a server people use.
    #[must_use]
    pub fn with_plain_http_to_localhost(mut self) -> Self {
        self.http_localhost = true;
        self
    }

    /// Whether a subscription may point at `endpoint`.
    pub fn allows(&self, endpoint: &str) -> bool {
        self.origin_of(endpoint).is_ok()
    }

    /// The origin (`scheme://host[:port]`) of an endpoint the list allows, which its VAPID
    /// token names.
    pub(crate) fn origin_of(&self, endpoint: &str) -> Result<String, MobileError> {
        let refused = |reason: &str| MobileError::PushEndpoint { reason: reason.to_string() };
        if endpoint.len() > MAX_ENDPOINT {
            return Err(refused("the endpoint is too long"));
        }
        let uri: Uri = endpoint.parse().map_err(|_| refused("the endpoint is not a URL"))?;
        let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
            return Err(refused("the endpoint is not an absolute URL"));
        };
        if authority.as_str().contains('@') {
            return Err(refused("the endpoint carries credentials"));
        }
        let host = authority.host().to_ascii_lowercase();
        if host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
            return Err(refused("the endpoint is an IP address, not a push service"));
        }
        match scheme {
            "https" => {}
            "http" if self.http_localhost && host == "localhost" => {}
            _ => return Err(refused("the endpoint is not https")),
        }
        if !self.hosts.iter().any(|allowed| host_matches(allowed, &host)) {
            return Err(refused("the host is not a push service this server sends to"));
        }
        Ok(format!("{scheme}://{}", authority.as_str().to_ascii_lowercase()))
    }
}

fn host_matches(allowed: &str, host: &str) -> bool {
    match allowed.strip_prefix("*.") {
        Some(domain) => {
            host.strip_suffix(domain).is_some_and(|label| label.len() > 1 && label.ends_with('.'))
        }
        None => allowed == host,
    }
}

/// A phone's push subscription as the store keeps it: the endpoint, the keys its browser gave,
/// and the id of the push key it subscribed with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Subscription {
    pub(crate) endpoint: String,
    pub(crate) p256dh: String,
    pub(crate) auth: String,
    pub(crate) key_id: String,
}

impl Subscription {
    /// Checks the endpoint against `allowlist` and the browser's keys, as subscribed with the
    /// server's key `key_id`.
    pub(crate) fn new(
        allowlist: &PushAllowlist,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
        key_id: &str,
    ) -> Result<Self, MobileError> {
        allowlist.origin_of(endpoint)?;
        let subscription = Self {
            endpoint: endpoint.to_string(),
            p256dh: p256dh.to_string(),
            auth: auth.to_string(),
            key_id: key_id.to_string(),
        };
        subscription.keys()?;
        Ok(subscription)
    }

    fn keys(&self) -> Result<(p256::PublicKey, Auth), MobileError> {
        let refused = |reason: &str| MobileError::PushEndpoint { reason: reason.to_string() };
        let public = Base64UrlUnpadded::decode_vec(self.p256dh.trim_end_matches('='))
            .ok()
            .and_then(|bytes| p256::PublicKey::from_sec1_bytes(&bytes).ok())
            .ok_or_else(|| refused("the subscription's p256dh key is not a P-256 public key"))?;
        let auth = Base64UrlUnpadded::decode_vec(self.auth.trim_end_matches('='))
            .ok()
            .filter(|bytes| bytes.len() == 16)
            .ok_or_else(|| refused("the subscription's auth secret is not 16 bytes"))?;
        Ok((public, Auth::clone_from_slice(&auth)))
    }
}

/// What became of one wake-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sent {
    /// The push service took it.
    Delivered,
    /// The subscription is gone (`404`, `410`): remove it.
    Gone,
    /// It could not be sent and would not be on a later try; why, without secrets.
    Failed(String),
    /// The push service was busy, failing or out of reach on every attempt: a later try may
    /// get through. Why, without secrets.
    Unavailable(String),
}

/// Sends wake-ups (spec 3.3 §5.4): encrypted with aes128gcm for the subscription's keys,
/// signed with the kernel's VAPID key, `Urgency: high` and a TTL, retried with backoff on a
/// busy or failing push service.
#[derive(Debug, Clone)]
pub(crate) struct PushSender {
    client: reqwest::Client,
    vapid: Vapid,
    allowlist: PushAllowlist,
    ttl: Duration,
    backoff: Duration,
    attempts: u32,
}

impl PushSender {
    pub(crate) fn new(vapid: Vapid, allowlist: PushAllowlist, backoff: Duration) -> Self {
        // A push service never redirects; following one could reach a host the list refuses.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self { client, vapid, allowlist, ttl: PUSH_TTL, backoff, attempts: 3 }
    }

    pub(crate) fn vapid(&self) -> &Vapid {
        &self.vapid
    }

    pub(crate) fn allowlist(&self) -> &PushAllowlist {
        &self.allowlist
    }

    /// Sends `body` to `subscription`.
    pub(crate) async fn send(&self, subscription: &Subscription, body: &[u8]) -> Sent {
        if subscription.key_id != self.vapid.key().id() {
            return Sent::Failed(format!(
                "the phone subscribed with push key {}, and this server holds {}: share one key \
                 with --push-key",
                subscription.key_id,
                self.vapid.key().id()
            ));
        }
        let origin = match self.allowlist.origin_of(&subscription.endpoint) {
            Ok(origin) => origin,
            Err(error) => return Sent::Failed(error.to_string()),
        };
        let request = match self.request(subscription, &origin, body) {
            Ok(request) => request,
            Err(reason) => return Sent::Failed(reason),
        };
        let mut wait = self.backoff;
        let mut last = String::new();
        for attempt in 1..=self.attempts {
            let Some(request) = request.try_clone() else {
                return Sent::Failed("the push request could not be repeated".into());
            };
            match self.client.execute(request).await {
                Ok(response) if response.status().is_success() => return Sent::Delivered,
                Ok(response) if matches!(response.status().as_u16(), 404 | 410) => {
                    return Sent::Gone;
                }
                Ok(response)
                    if response.status().as_u16() == 429 || response.status().is_server_error() =>
                {
                    last = format!("the push service answered {}", response.status());
                }
                Ok(response) => {
                    return Sent::Failed(format!(
                        "the push service answered {}",
                        response.status()
                    ));
                }
                Err(error) => last = format!("the push service could not be reached: {error}"),
            }
            if attempt < self.attempts {
                tracing::debug!(push.attempt = attempt, error = %last, "a wake-up will be sent again");
                tokio::time::sleep(wait).await;
                wait = wait.saturating_mul(2);
            }
        }
        Sent::Unavailable(last)
    }

    fn request(
        &self,
        subscription: &Subscription,
        origin: &str,
        body: &[u8],
    ) -> Result<reqwest::Request, String> {
        let (public, auth) = subscription.keys().map_err(|error| error.to_string())?;
        let uri: Uri =
            subscription.endpoint.parse().map_err(|_| "the endpoint is not a URL".to_string())?;
        let mut request = WebPushBuilder::new(uri, public, auth)
            .with_valid_duration(self.ttl)
            .build(body.to_vec())
            .map_err(|error| format!("the wake-up could not be encrypted: {error}"))?;
        let authorization = HeaderValue::from_str(&self.vapid.authorization(origin))
            .map_err(|_| "the VAPID header is not valid".to_string())?;
        request.headers_mut().insert(axum::http::header::AUTHORIZATION, authorization);
        request.headers_mut().insert("urgency", HeaderValue::from_static("high"));
        reqwest::Request::try_from(request).map_err(|error| error.to_string())
    }
}

/// The body of every wake-up (spec 3.3 §11 S-I5): a version and a random id the phone
/// acknowledges, never a word about any decision. The notification's words are fixed in the
/// service worker.
pub(crate) fn wake_up(id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "v": 1, "id": id })).unwrap_or_default()
}

/// A new random wake-up id.
pub(crate) fn new_wake_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return uuid::Uuid::new_v4().simple().to_string();
    }
    Base64UrlUnpadded::encode_string(&bytes)
}

/// The wake-ups sent and acknowledged, for "Push delivery" (spec 3.3 §5.4): the operator reads
/// the latest at `GET /surface/phone/push/deliveries`.
#[derive(Debug, Default)]
pub(crate) struct Deliveries {
    inner: Mutex<DeliveriesInner>,
}

#[derive(Debug, Default)]
struct DeliveriesInner {
    sent: HashMap<String, (String, DateTime<Utc>)>,
    order: VecDeque<String>,
    received: VecDeque<Delivery>,
}

impl Deliveries {
    /// Wake-up `id` went to `device` at `at`.
    pub(crate) fn sent(&self, id: &str, device: &str, at: DateTime<Utc>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.order.len() >= SENT_KEPT
            && let Some(oldest) = inner.order.pop_front()
        {
            inner.sent.remove(&oldest);
        }
        inner.order.push_back(id.to_string());
        inner.sent.insert(id.to_string(), (device.to_string(), at));
    }

    /// `device` acknowledged wake-up `id` at `at`; `None` when it was not sent to that device.
    pub(crate) fn received(&self, id: &str, device: &str, at: DateTime<Utc>) -> Option<Delivery> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.sent.get(id) {
            Some((to, _)) if to == device => {}
            _ => return None,
        }
        let (device, sent_at) = inner.sent.remove(id)?;
        inner.order.retain(|kept| kept != id);
        let delivery = Delivery {
            device_id: device,
            sent_at,
            received_at: at,
            latency_ms: u64::try_from((at - sent_at).num_milliseconds()).unwrap_or(0),
        };
        if inner.received.len() >= DELIVERIES_KEPT {
            inner.received.pop_front();
        }
        inner.received.push_back(delivery.clone());
        Some(delivery)
    }

    /// The latest acknowledged deliveries, oldest first.
    pub(crate) fn list(&self) -> Vec<Delivery> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).received.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_browsers_push_services_are_allowed_over_https_only() {
        let allow = PushAllowlist::default();
        for endpoint in [
            "https://web.push.apple.com/QGu1",
            "https://api.push.apple.com/x",
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://updates.push.services.mozilla.com/wpush/v2/x",
            "https://wns2-by3p.notify.windows.com/w/?token=x",
        ] {
            assert!(allow.allows(endpoint), "{endpoint}");
        }
        for endpoint in [
            "http://web.push.apple.com/QGu1",
            "https://push.apple.com/x",
            "https://evilpush.apple.com.example.com/x",
            "https://fcm.googleapis.com.example.com/x",
            "https://user:pw@fcm.googleapis.com/x",
            "https://192.0.2.7/push",
            "https://[2001:db8::1]/push",
            "https://localhost/push",
            "http://localhost:8080/push",
            "web.push.apple.com/x",
            "not a url",
        ] {
            assert!(!allow.allows(endpoint), "{endpoint}");
        }
    }

    #[test]
    fn plain_http_is_for_localhost_in_tests_only() {
        let allow = PushAllowlist::new(["localhost"]).with_plain_http_to_localhost();
        assert_eq!(allow.origin_of("http://localhost:4100/p/1").unwrap(), "http://localhost:4100");
        assert!(!allow.allows("http://127.0.0.1:4100/p/1"));
        assert!(!PushAllowlist::new(["localhost"]).allows("http://localhost:4100/p/1"));
    }

    #[test]
    fn the_wake_up_body_is_a_version_and_an_id() {
        let body: serde_json::Value = serde_json::from_slice(&wake_up("w1")).unwrap();
        assert_eq!(body, serde_json::json!({ "v": 1, "id": "w1" }));
        assert_ne!(new_wake_id(), new_wake_id());
    }

    #[test]
    fn only_the_device_a_wake_up_went_to_acknowledges_it() {
        let deliveries = Deliveries::default();
        let at = Utc::now();
        deliveries.sent("w1", "dev-a", at);
        assert_eq!(deliveries.received("w1", "dev-b", at), None);
        let delivery =
            deliveries.received("w1", "dev-a", at + chrono::Duration::milliseconds(840)).unwrap();
        assert_eq!(delivery.latency_ms, 840);
        assert_eq!(deliveries.received("w1", "dev-a", at), None, "acknowledged once");
        assert_eq!(deliveries.list(), vec![delivery]);
    }
}
