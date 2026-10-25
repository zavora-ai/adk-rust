//! A push service on loopback for the phone's tests (spec 3.3 §5.4): it records each request it
//! is sent and answers a status, and a browser's subscription keys that decrypt what it got.

// Several test binaries include this file, each using part of it.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use base64ct::{Base64UrlUnpadded, Encoding as _};
use p256::elliptic_curve::sec1::ToEncodedPoint as _;
use serde_json::{Value, json};

/// The requests a fake push service got.
type Got = Arc<Mutex<Vec<(HeaderMap, Vec<u8>)>>>;

/// A push service on loopback that records what it is sent and answers `status` (201 unless
/// set).
#[derive(Clone, Default)]
pub struct FakePush {
    pub got: Got,
    pub status: Arc<Mutex<Option<u16>>>,
}

impl FakePush {
    /// Starts it on `127.0.0.1`; its base URL names `localhost`, as WebAuthn and the allowlist
    /// need. The server task stops when the handle is aborted or dropped with the runtime.
    pub async fn start() -> (Self, String, tokio::task::JoinHandle<()>) {
        let fake = Self::default();
        let state = fake.clone();
        let app =
            axum::Router::new().fallback(move |headers: HeaderMap, body: axum::body::Bytes| {
                let state = state.clone();
                async move {
                    state.got.lock().unwrap().push((headers, body.to_vec()));
                    let status = state.status.lock().unwrap().unwrap_or(201);
                    StatusCode::from_u16(status).unwrap()
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (fake, format!("http://localhost:{port}"), server)
    }

    /// How many requests it got.
    pub fn count(&self) -> usize {
        self.got.lock().unwrap().len()
    }

    /// Waits up to four seconds for `n` requests.
    pub async fn wait_for(&self, n: usize) {
        for _ in 0..400 {
            if self.count() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{n} wake-ups expected, {} sent", self.count());
    }
}

/// A browser's push subscription keys.
pub struct Browser {
    secret: p256::SecretKey,
    auth: [u8; 16],
}

impl Browser {
    pub fn new() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).unwrap();
        let mut auth = [0u8; 16];
        getrandom::fill(&mut auth).unwrap();
        Self { secret: p256::SecretKey::from_slice(&bytes).unwrap(), auth }
    }

    /// Its subscription to `endpoint`, as `PushSubscription.toJSON()` gives it.
    pub fn subscription(&self, endpoint: &str) -> Value {
        let public = self.secret.public_key().to_encoded_point(false);
        json!({
            "endpoint": endpoint,
            "keys": {
                "p256dh": Base64UrlUnpadded::encode_string(public.as_bytes()),
                "auth": Base64UrlUnpadded::encode_string(&self.auth),
            },
        })
    }

    /// What a wake-up's body decrypts to, as the phone's push service hands it over.
    pub fn decrypt(&self, body: &[u8]) -> Value {
        let auth = web_push_native::Auth::clone_from_slice(&self.auth);
        let plain = web_push_native::decrypt(body.to_vec(), &self.secret, &auth).unwrap();
        serde_json::from_slice(&plain).unwrap()
    }
}
