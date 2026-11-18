//! A fake Microsoft Teams on loopback, served by wiremock: Microsoft Entra ID's token
//! endpoint, the Bot Connector's OpenID metadata and signing keys, and the Bot Connector REST
//! API, which keeps every activity the adapter posts — what left the machine — and can throttle
//! or refuse the next posts. Activities are signed as the Bot Connector signs them: RS256 with
//! a local key the fake's JWKS publishes, endorsed for `msteams`, carrying the bot's app id as
//! the audience and the conversation's service address.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use adk_surface::channels::{TeamsAdapter, TeamsAdapterBuilder};
use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::KeyPair as _;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{body_string_contains, header, method, path, path_regex};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

pub const APP_ID: &str = "11111111-2222-3333-4444-555555555555";
pub const PASSWORD: &str = "fake-client-secret";
pub const ACCESS_TOKEN: &str = "fake-entra-access-token";
pub const TENANT: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";
pub const ISSUER: &str = "https://api.botframework.com";
pub const KEY_ID: &str = "fake-connector-key";

/// A signing key and its public JWK.
pub struct SigningKey {
    pub encoding: EncodingKey,
    pub jwk: Value,
}

fn generate(kid: &str) -> SigningKey {
    let pair = KeyPair::generate(KeySize::Rsa2048).expect("RSA key generation");
    let pkcs8 = STANDARD.encode(pair.as_der().expect("PKCS#8 encoding").as_ref());
    let lines = pkcs8.as_bytes().chunks(64).map(String::from_utf8_lossy).collect::<Vec<_>>();
    let pem =
        format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", lines.join("\n"));
    let public = pair.public_key();
    let jwk = json!({
        "kty": "RSA",
        "use": "sig",
        "kid": kid,
        "n": URL_SAFE_NO_PAD.encode(public.modulus().big_endian_without_leading_zero()),
        "e": URL_SAFE_NO_PAD.encode(public.exponent().big_endian_without_leading_zero()),
        "endorsements": ["msteams", "skype"],
    });
    SigningKey { encoding: EncodingKey::from_rsa_pem(pem.as_bytes()).expect("PKCS#8 PEM"), jwk }
}

/// The Bot Connector's key, published in the fake's JWKS.
pub fn connector_key() -> &'static SigningKey {
    static KEY: OnceLock<SigningKey> = OnceLock::new();
    KEY.get_or_init(|| generate(KEY_ID))
}

/// A key nobody published, with the connector key's id: a forger's.
pub fn forged_key() -> &'static SigningKey {
    static KEY: OnceLock<SigningKey> = OnceLock::new();
    KEY.get_or_init(|| generate(KEY_ID))
}

/// One activity the adapter posted.
#[derive(Debug, Clone)]
pub struct Sent {
    pub conversation: String,
    pub reply_to: Option<String>,
    pub body: Value,
    /// The id the Bot Connector gave it; `None` when it was refused.
    pub id: Option<String>,
}

#[derive(Default)]
struct Shared {
    sent: Mutex<Vec<Sent>>,
    refusals: Mutex<VecDeque<ResponseTemplate>>,
    next_id: AtomicU64,
}

/// The Bot Connector's activities endpoint: keeps each post, answers a ResourceResponse.
struct Activities(Arc<Shared>);

impl Respond for Activities {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let segments: Vec<String> =
            request.url.path_segments().into_iter().flatten().map(percent_decode).collect();
        // teams / v3 / conversations / {id} / activities [/ {reply}]
        let conversation = segments.get(3).cloned().unwrap_or_default();
        let reply_to = segments.get(5).cloned();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let refusal = self.0.refusals.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
        let id = refusal
            .is_none()
            .then(|| format!("1763{:09}", self.0.next_id.fetch_add(1, Ordering::SeqCst)));
        self.0.sent.lock().unwrap_or_else(PoisonError::into_inner).push(Sent {
            conversation,
            reply_to,
            body,
            id: id.clone(),
        });
        refusal.unwrap_or_else(|| ResponseTemplate::new(200).set_body_json(json!({ "id": id })))
    }
}

fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = segment.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// The fake, served until it is dropped.
pub struct FakeTeams {
    pub server: MockServer,
    shared: Arc<Shared>,
    next_activity: AtomicU64,
}

impl FakeTeams {
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let shared = Arc::new(Shared::default());
        let token = json!({ "token_type": "Bearer", "expires_in": 3600, "ext_expires_in": 3600, "access_token": ACCESS_TOKEN });
        for tenant in ["botframework.com", TENANT] {
            Mock::given(method("POST"))
                .and(path(format!("/{tenant}/oauth2/v2.0/token")))
                .and(body_string_contains("grant_type=client_credentials"))
                .and(body_string_contains(format!("client_id={APP_ID}")))
                .and(body_string_contains(format!("client_secret={PASSWORD}")))
                .and(body_string_contains("scope=https%3A%2F%2Fapi.botframework.com%2F.default"))
                .respond_with(ResponseTemplate::new(200).set_body_json(token.clone()))
                .with_priority(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path_regex(r"^/[^/]+/oauth2/v2\.0/token$"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": "invalid_client",
                "error_description": "AADSTS7000215: Invalid client secret provided.",
            })))
            .with_priority(5)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/.well-known/openidconfiguration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": ISSUER,
                "authorization_endpoint": "https://invalid.botframework.com",
                "jwks_uri": format!("{}/v1/.well-known/keys", server.uri()),
                "id_token_signing_alg_values_supported": ["RS256"],
                "token_endpoint_auth_methods_supported": ["private_key_jwt"],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/.well-known/keys"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "keys": [connector_key().jwk.clone()] })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/teams/v3/conversations/[^/]+/activities(/[^/]+)?$"))
            .and(header("authorization", format!("Bearer {ACCESS_TOKEN}").as_str()))
            .respond_with(Activities(shared.clone()))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/teams/v3/conversations/"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": { "code": "Unauthorized", "message": "Authorization has been denied for this request." },
            })))
            .with_priority(5)
            .mount(&server)
            .await;
        Self { server, shared, next_activity: AtomicU64::new(1) }
    }

    /// The service address activities carry and the adapter posts to.
    pub fn service_url(&self) -> String {
        format!("{}/teams/", self.server.uri())
    }

    pub fn openid_configuration(&self) -> String {
        format!("{}/v1/.well-known/openidconfiguration", self.server.uri())
    }

    /// An adapter builder pointed at the fake.
    pub fn builder(&self) -> TeamsAdapterBuilder {
        TeamsAdapter::builder(APP_ID, PASSWORD)
            .login_base(self.server.uri())
            .openid_configuration(self.openid_configuration())
            .service_url(self.service_url())
    }

    /// Answers the next `times` posts with `status`, and `retry_after` seconds when set.
    pub fn refuse(&self, times: usize, status: u16, retry_after: Option<u64>) {
        let mut refusals = self.shared.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..times {
            let mut response = ResponseTemplate::new(status).set_body_json(json!({
                "error": { "code": "Throttled", "message": "Too many requests" },
            }));
            if let Some(seconds) = retry_after {
                response = response.insert_header("retry-after", seconds.to_string().as_str());
            }
            refusals.push_back(response);
        }
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.shared.sent.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn sent_to(&self, conversation: &str) -> Vec<Sent> {
        self.sent().into_iter().filter(|sent| sent.conversation == conversation).collect()
    }

    /// What each activity to `conversation` shows: a message's text, or a card's runs.
    pub fn texts(&self, conversation: &str) -> Vec<String> {
        self.sent_to(conversation).iter().map(|sent| shown(&sent.body)).collect()
    }

    /// The last decision card posted to `conversation`.
    pub fn last_card(&self, conversation: &str) -> Option<Sent> {
        self.sent_to(conversation).into_iter().rev().find(|sent| {
            sent.id.is_some() && sent.body["attachments"][0]["content"]["type"] == "AdaptiveCard"
        })
    }

    /// How many token requests Entra ID answered.
    pub async fn token_requests(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| request.url.path().ends_with("/oauth2/v2.0/token"))
            .count()
    }

    /// How many times the signing keys were fetched.
    pub async fn key_fetches(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| request.url.path() == "/v1/.well-known/keys")
            .count()
    }

    fn activity_id(&self) -> String {
        format!("f:{}", self.next_activity.fetch_add(1, Ordering::SeqCst))
    }

    /// An activity of `kind` from `from` in the one-on-one chat `conversation`.
    pub fn activity(&self, kind: &str, from: &str, conversation: &str, extra: Value) -> Value {
        let mut activity = json!({
            "type": kind,
            "id": self.activity_id(),
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "serviceUrl": self.service_url(),
            "channelId": "msteams",
            "from": { "id": from, "name": "Ana", "aadObjectId": "7faf8ab2-3d56-4244-b585-20c8a42ed2b8" },
            "conversation": { "conversationType": "personal", "tenantId": TENANT, "id": conversation },
            "recipient": { "id": format!("28:{APP_ID}"), "name": "Agents" },
            "channelData": { "tenant": { "id": TENANT } },
            "locale": "en-US",
        });
        if let (Some(target), Some(extra)) = (activity.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        activity
    }

    /// `from` typing `text`.
    pub fn message(&self, from: &str, conversation: &str, text: &str) -> Value {
        self.activity("message", from, conversation, json!({ "textFormat": "plain", "text": text }))
    }

    /// The data of the button titled `title` on the last card to `conversation`, and the card.
    fn button(&self, conversation: &str, title: &str) -> Option<(Value, String)> {
        let card = self.last_card(conversation)?;
        let body = card.body["attachments"][0]["content"]["body"].as_array()?.clone();
        let actions = body.iter().find(|block| block["type"] == "ActionSet")?["actions"].clone();
        let action = actions
            .as_array()?
            .iter()
            .find(|action| action["type"] == "Action.Execute" && action["title"] == title)?
            .clone();
        Some((action, card.id?))
    }

    /// `from` pressing the `Action.Execute` titled `title` on the last card to
    /// `conversation`: the `adaptiveCard/action` invoke Teams sends, or `None`.
    pub fn press(&self, from: &str, conversation: &str, title: &str) -> Option<Value> {
        let (action, card) = self.button(conversation, title)?;
        Some(self.activity(
            "invoke",
            from,
            conversation,
            json!({
                "name": "adaptiveCard/action",
                "replyToId": card,
                "value": {
                    "action": { "type": "Action.Execute", "id": "", "verb": action["verb"], "data": action["data"] },
                    "trigger": "manual",
                },
            }),
        ))
    }

    /// `from` pressing the `Action.Submit` fallback titled `title`, as an older client does.
    pub fn submit(&self, from: &str, conversation: &str, title: &str) -> Option<Value> {
        let (action, card) = self.button(conversation, title)?;
        Some(self.activity(
            "message",
            from,
            conversation,
            json!({ "replyToId": card, "value": action["fallback"]["data"] }),
        ))
    }

    /// A Bot Connector token for an activity from this fake's service address.
    pub fn token(&self) -> String {
        self.token_with(json!({}), connector_key())
    }

    /// A token with `overrides` on the connector's claims, signed with `key`.
    pub fn token_with(&self, overrides: Value, key: &SigningKey) -> String {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mut claims = json!({
            "iss": ISSUER,
            "aud": APP_ID,
            "nbf": now - 60,
            "exp": now + 3600,
            "serviceurl": self.service_url(),
        });
        if let (Some(target), Some(overrides)) = (claims.as_object_mut(), overrides.as_object()) {
            for (name, value) in overrides {
                if value.is_null() {
                    target.remove(name);
                } else {
                    target.insert(name.clone(), value.clone());
                }
            }
        }
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(key.jwk["kid"].as_str().unwrap().to_string());
        jsonwebtoken::encode(&header, &claims, &key.encoding).expect("token signing")
    }
}

/// What an activity shows: its text, or the text of a card's runs, one line each.
pub fn shown(body: &Value) -> String {
    if let Some(text) = body["text"].as_str() {
        return text.to_string();
    }
    body["attachments"][0]["content"]["body"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "RichTextBlock")
        .map(|block| {
            block["inlines"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|run| run["text"].as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Posts `activity` to the webhook `router` with `token` as the Bot Connector does; returns
/// the status and the body, `null` when empty.
pub async fn post_to(
    router: &Router,
    activity: &Value,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::post("/").header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request.body(Body::from(serde_json::to_vec(activity).unwrap())).unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}
