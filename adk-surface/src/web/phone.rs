//! The wire types of the device routes and the phone's routes (spec 3.3 §5.4, §8.2, §10 K2).
//!
//! They compile under `web`, not `mobile`, so the OpenAPI document and the TypeScript types
//! generated from it are the same in every `web` build (see [`openapi`](fn@super::openapi)).
//! The routes themselves are served by [`devices_router`](super::devices_router) and, with
//! feature `mobile`, `adk_surface::mobile::router`.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rules::Answer;

/// `POST /surface/decisions/{id}/answer` body: an answer from a paired device, at the version
/// it saw (spec 3.3 §7.2, S-I2).
///
/// # Example
///
/// ```rust
/// use adk_surface::web::AnswerRequest;
///
/// let request: AnswerRequest = serde_json::from_value(serde_json::json!({
///     "version": 3,
///     "answer": "deny",
///     "expanded": true,
/// }))
/// .unwrap();
/// assert_eq!(request.answer, adk_surface::Answer::Deny);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnswerRequest {
    /// The card's `version`.
    pub version: u64,
    /// The answer.
    pub answer: Answer,
    /// The changed arguments of an `edit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_args: Option<Value>,
    /// Why, for a `deny`; reported to the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The device showed the call in full (§7.1).
    #[serde(default)]
    pub expanded: bool,
}

/// `POST /surface/stop` body: **Stop all** from a paired device (spec 3.3 §11 S-I10).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StopRequest {
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `GET /surface/phone` answer: what the phone needs to subscribe and to confirm it is you
/// (spec 3.3 §5.4, §8.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhoneInfo {
    /// The kernel's push key, which the phone subscribes with (`applicationServerKey`).
    pub push_key: String,
    /// The passkeys' relying party: the home's host.
    pub rp_id: String,
    /// The caller has a passkey here: this phone's, or for the web home, the person's.
    pub passkey: bool,
    /// This phone is subscribed to wake-ups.
    pub push: bool,
    /// The caller may set up a passkey now: a phone within 10 minutes of pairing without one,
    /// or the person at the web home.
    pub can_add_passkey: bool,
}

/// The keys a browser gives with its push subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PushKeys {
    /// The subscription's P-256 public key, base64url.
    pub p256dh: String,
    /// The subscription's authentication secret, base64url.
    pub auth: String,
}

/// `PUT /surface/phone/push` body: the phone's push subscription, as `PushSubscription.toJSON()`
/// gives it (spec 3.3 §5.4).
///
/// # Example
///
/// ```rust
/// use adk_surface::web::PushSubscriptionRequest;
///
/// let request: PushSubscriptionRequest = serde_json::from_value(serde_json::json!({
///     "endpoint": "https://web.push.apple.com/QGu1",
///     "keys": { "p256dh": "BLn9…", "auth": "_ord…" },
/// }))
/// .unwrap();
/// assert_eq!(request.keys.auth, "_ord…");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PushSubscriptionRequest {
    /// The push service URL: https, on Apple's, Google's, Mozilla's or Microsoft's service.
    pub endpoint: String,
    /// The browser's keys.
    pub keys: PushKeys,
}

/// `POST /surface/phone/push/received` body: the phone's acknowledgement of a wake-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PushReceivedRequest {
    /// The wake-up's `id`.
    pub id: String,
}

/// One wake-up a phone acknowledged, for "Push delivery" (spec 3.3 §5.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    /// The phone.
    pub device_id: String,
    /// When the kernel sent it.
    pub sent_at: DateTime<Utc>,
    /// When the phone acknowledged it.
    pub received_at: DateTime<Utc>,
    /// The time between, in milliseconds.
    pub latency_ms: u64,
}

/// `GET /surface/phone/push/deliveries` answer, for the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeliveriesResponse {
    /// The latest acknowledged wake-ups, oldest first.
    pub deliveries: Vec<Delivery>,
}

/// `POST /surface/phone/passkey/options` answer: the options for
/// `navigator.credentials.create`, as WebAuthn's JSON form gives them (spec 3.3 §8.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyOptions {
    /// What the registration answers, with the credential.
    pub registration_id: String,
    /// `PublicKeyCredentialCreationOptionsJSON`: a platform authenticator, user verification
    /// required, attestation none.
    pub public_key: Value,
}

/// `POST /surface/phone/passkey` body: the new passkey.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyRegistration {
    /// The options' `registrationId`.
    pub registration_id: String,
    /// The `PublicKeyCredential` as JSON (`credential.toJSON()`).
    pub credential: Value,
}

/// `POST /surface/decisions/{id}/confirm/options` body: the answer the person is about to
/// confirm, bound on the server to this decision, its version and these arguments (spec 3.3
/// §8.2).
///
/// # Example
///
/// ```rust
/// use adk_surface::web::ConfirmOptionsRequest;
///
/// let request: ConfirmOptionsRequest = serde_json::from_value(serde_json::json!({
///     "version": 2,
///     "answer": "allowOnce",
///     "expanded": true,
/// }))
/// .unwrap();
/// assert_eq!(request.answer, adk_surface::Answer::AllowOnce);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmOptionsRequest {
    /// The card's `version`.
    pub version: u64,
    /// `allowOnce` or `edit`.
    pub answer: Answer,
    /// The changed arguments of an `edit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_args: Option<Value>,
    /// The surface showed the call in full (§7.1).
    #[serde(default)]
    pub expanded: bool,
}

/// `POST /surface/decisions/{id}/confirm/options` answer: the options for
/// `navigator.credentials.get`, single use, for two minutes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmOptions {
    /// What the confirmation answers, with the assertion.
    pub challenge_id: String,
    /// `PublicKeyCredentialRequestOptionsJSON`: user verification required.
    pub public_key: Value,
}

/// `POST /surface/decisions/{id}/confirm` body: the passkey's assertion for the bound answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmRequest {
    /// The options' `challengeId`.
    pub challenge_id: String,
    /// The `PublicKeyCredential` as JSON (`credential.toJSON()`).
    pub credential: Value,
    /// The answer, when the page sends it again: it must be the bound one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// The changed arguments, when the page sends them again: they must be the bound ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_args: Option<Value>,
}

/// `POST /surface/phone/qr` body: the pairing link to draw, for the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QrRequest {
    /// The link: it starts with the home's URL.
    pub url: String,
}

/// A paired device's phone attachments, as "Where you can say yes from" lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhoneDevice {
    /// The device.
    pub device_id: String,
    /// It asks for the person's face or fingerprint.
    pub passkey: bool,
    /// It is woken by push.
    pub push: bool,
}

/// A passkey the person set up on a computer for the web home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputerPasskey {
    /// Its id, which **Remove** names.
    pub id: String,
    /// When it was set up.
    pub created_at: DateTime<Utc>,
}

/// `GET /surface/phone/devices` answer, for the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhoneDevicesResponse {
    /// Every paired device with a passkey or a push subscription.
    pub devices: Vec<PhoneDevice>,
    /// The caller's computer passkeys.
    pub computer_passkeys: Vec<ComputerPasskey>,
}
