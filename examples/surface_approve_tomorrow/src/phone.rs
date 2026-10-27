//! The phone in this scenario (spec 3.3 §5.4, §12 phase 1): a paired phone whose browser
//! subscribed to Web Push through a push service on loopback this program serves, and the
//! notifiers the quiet-time check reads.
//!
//! `adk-rust up --push-to-localhost` lets the phone subscribe at `http://localhost`, so every
//! wake-up the kernel sends arrives here, with the time it arrived. The phone reads its cards
//! with its own device token, as the installed web home does.

use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Bytes;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::stage;

/// The browser's subscription keys: the user agent's public key and authentication secret of
/// the RFC 8291 example (section 5). The kernel encrypts each wake-up to them; this push
/// service only counts what arrives.
const P256DH: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

/// Something that can put a notification in front of the person, as the scenario observes
/// it.
pub trait Notifier {
    /// The notifier in words: "Telegram", "the phone".
    fn name(&self) -> &str;

    /// When it notified anyone between `from` and `to`.
    fn notified_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>>;
}

impl Notifier for crate::telegram::FakeTelegram {
    fn name(&self) -> &str {
        "Telegram"
    }

    fn notified_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
        crate::telegram::FakeTelegram::notified_between(self, from, to)
            .into_iter()
            .map(|call| call.at)
            .collect()
    }
}

/// The wake-ups a push service got: when each arrived and how many bytes it carried.
type WakeUps = Arc<Mutex<Vec<(DateTime<Utc>, usize)>>>;

/// A push service on loopback: every wake-up the kernel sends it, with when it arrived and
/// how many bytes it carried.
#[derive(Clone)]
pub struct PushService {
    port: u16,
    got: WakeUps,
    task: Arc<tokio::task::JoinHandle<()>>,
}

impl Drop for PushService {
    fn drop(&mut self) {
        if Arc::strong_count(&self.task) == 1 {
            self.task.abort();
        }
    }
}

impl PushService {
    /// Starts it on `127.0.0.1`; the phone subscribes at `http://localhost:<port>`.
    pub async fn start() -> Result<Self> {
        let got: WakeUps = Arc::default();
        let seen = Arc::clone(&got);
        let app = Router::new().fallback(move |body: Bytes| {
            let seen = Arc::clone(&seen);
            async move {
                seen.lock().unwrap_or_else(PoisonError::into_inner).push((Utc::now(), body.len()));
                StatusCode::CREATED
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { port, got, task: Arc::new(task) })
    }

    /// The wake-ups that arrived, with their time and size.
    pub fn wake_ups(&self) -> Vec<(DateTime<Utc>, usize)> {
        self.got.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Notifier for PushService {
    fn name(&self) -> &str {
        "the phone"
    }

    fn notified_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
        self.wake_ups().into_iter().map(|(at, _)| at).filter(|at| *at >= from && *at < to).collect()
    }
}

/// Ana's phone: paired with the kernel, its device token, and its browser subscribed to the
/// push service.
pub struct Phone {
    base: String,
    token: String,
}

impl Phone {
    /// Pairs a phone with the kernel the stage serves (`POST /api/kernel/pairing` with the
    /// operator's token, then the claim the installed home makes), and subscribes its browser
    /// to `push` (`PUT /api/surface/phone/push`).
    pub async fn pair(stage: &stage::Stage, push: &PushService) -> Result<Self> {
        let base = stage.base_url();
        let operator = stage.token().context("the stage does not serve the phone")?;
        let (status, issued) = stage::post_json(
            &format!("{base}/api/kernel/pairing"),
            Some(operator),
            &json!({ "name": "Ana's phone", "kind": "phone" }),
        )
        .await?;
        let code = issued["code"]
            .as_str()
            .with_context(|| format!("pairing a phone answered {status}: {issued}"))?;
        let (status, claimed) = stage::post_json(
            &format!("{base}/api/kernel/pairing/claim"),
            None,
            &json!({ "code": code }),
        )
        .await?;
        let token = claimed["token"]
            .as_str()
            .with_context(|| format!("claiming the phone's code answered {status}: {claimed}"))?
            .to_string();
        let subscribed = reqwest::Client::new()
            .put(format!("{base}/api/surface/phone/push"))
            .bearer_auth(&token)
            .json(&json!({
                "endpoint": format!("http://localhost:{}/push/ana", push.port),
                "keys": { "p256dh": P256DH, "auth": AUTH },
            }))
            .send()
            .await?;
        if !subscribed.status().is_success() {
            let status = subscribed.status();
            bail!(
                "the phone could not subscribe to wake-ups: {status} {}",
                subscribed.text().await?
            );
        }
        Ok(Self { base, token })
    }

    /// The decision `id` as the phone shows it: `GET /api/surface/decisions/{id}?surface=phone`
    /// with its device token.
    pub async fn card(&self, id: &str) -> Result<(u16, Value)> {
        stage::get_json(
            &format!("{}/api/surface/decisions/{id}?surface=phone", self.base),
            Some(&self.token),
        )
        .await
    }
}
