//! The pairing routes of the control plane (spec 3.3 §10 K1, K2, §11 S-I6, S-I7): issue and
//! claim device codes, list and revoke devices, accept, list and revoke chat accounts, and
//! the admission of device tokens.
//!
//! | Route (under `/api`) | Caller | Effect |
//! |---|---|---|
//! | `POST /kernel/pairing` | operator | Issues a pairing code ([`IssueRequest`] → [`IssuedCode`]) |
//! | `POST /kernel/pairing/claim` | anyone holding a code | Claims it ([`ClaimRequest`] → [`ClaimedDevice`], the token shown once) |
//! | `GET /kernel/devices` | operator | Every paired device ([`DevicesResponse`]) |
//! | `POST /kernel/devices/{id}/revoke` | operator | Removes a device ([`DeviceResponse`]) |
//! | `POST /kernel/channels/pair` (feature `channels`) | operator | Accepts the code a chat account was sent ([`AcceptChannelRequest`] → [`ChannelAccountResponse`]) |
//! | `GET /kernel/channels` (feature `channels`) | operator | Paired chat accounts and the ones waiting ([`ChannelAccountsResponse`]) |
//! | `POST /kernel/channels/{id}/revoke` (feature `channels`) | operator | Removes a chat account and stops its turns that have not finished ([`ChannelAccountResponse`]) |
//! | `POST /kernel/channels/waiting/{id}/dismiss` (feature `channels`) | operator | Dismisses a chat account waiting to be paired: its code stops working and it gets no new one for a day ([`ChannelPairingResponse`]) |
//!
//! The operator routes are mounted on every kernel: one that pairs no devices, or has no
//! chat channels, answers `400` naming what it lacks, and lists no chat accounts.
//!
//! Each route that changes something writes a `kernel.control` record before it takes
//! effect: `pairing.issue`, `pairing.claim` (the device's owner as the subject, since the
//! caller has no credentials but the code), `devices.revoke`, `channels.pair`,
//! `channels.revoke` and `channels.dismiss`. A claim or an acceptance takes its code first
//! and puts it back when the record cannot be written, so nothing is paired unrecorded and
//! the code still works. A device's own actions carry `device` in their record, and a
//! decision a device answers is recorded as `approvals.decide`, with the device's name and
//! its answer, before it reaches the inbox.

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind};
use adk_core::principal::SubjectRef;
use adk_core::{ApprovalRequest, ApprovalRisk, RequestContext};
use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{Method, Request};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::routes::{Caller, ControlBackend, record};
use super::{ControlTransport, paths};
use crate::audit;
use crate::error::{KernelError, Result};
use crate::pairing::{
    ClaimedDevice, DEVICE_ID_METADATA, DEVICE_NAME_METADATA, DeviceAdmission, DeviceScope,
    DeviceView, IssueCode, IssuedCode, Pairing, device_scope_for, device_token,
};

/// `POST /kernel/pairing` body: `adk-rust pair [--name] [--scope] [--expires]` (spec 3.3 §10
/// K2).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::IssueRequest;
/// use adk_kernel::pairing::DeviceScope;
///
/// let body = IssueRequest { name: Some("Ana's phone".into()), scopes: vec![DeviceScope::Decide], expires_in_days: Some(30) };
/// assert_eq!(serde_json::to_value(&body).unwrap()["expiresInDays"], 30);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRequest {
    /// What the person calls the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The scopes; every scope when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<DeviceScope>,
    /// How many days the device stays paired; the kernel's default (90) when absent, at most
    /// 365.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<u32>,
}

/// `POST /kernel/pairing/claim` body (spec 3.3 §10 K2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRequest {
    /// The code, as shown or typed.
    pub code: String,
    /// What the device calls itself, used when the code was issued without a name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `GET /kernel/devices` answer (spec 3.3 §10 K2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicesResponse {
    /// Every paired device, oldest first, revoked and expired ones included.
    pub devices: Vec<DeviceView>,
}

/// The answer of a device action: the device after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceResponse {
    /// The device.
    pub device: DeviceView,
}

/// `POST /kernel/channels/pair` body (spec 3.3 §10 K1): the code a chat account was sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptChannelRequest {
    /// The code, as the chat account received it.
    pub code: String,
}

/// Whether a chat account reaches the agents (spec 3.3 §10 K1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ChannelAccountState {
    /// Its messages start turns and its answers decide.
    Active,
    /// It was removed.
    Revoked,
}

/// A paired chat account as the control plane lists it (spec 3.3 §10 K1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChannelAccountView {
    /// The pairing's id.
    pub id: String,
    /// The channel, such as `telegram`.
    pub channel: String,
    /// The account on that channel.
    pub account: String,
    /// The account's display name, as the platform gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The person it acts for.
    pub user_id: String,
    /// When it was paired.
    pub paired_at: DateTime<Utc>,
    /// When it was removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    /// Whether it reaches the agents now.
    pub state: ChannelAccountState,
}

/// A chat account that asked to pair and has not been accepted (spec 3.3 §10 K1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChannelPairingView {
    /// The id the account gets once paired, which dismisses it while it waits.
    pub id: String,
    /// The channel.
    pub channel: String,
    /// The account.
    pub account: String,
    /// The account's display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// When it asked.
    pub requested_at: DateTime<Utc>,
    /// When its code stops working.
    pub expires_at: DateTime<Utc>,
}

/// `GET /kernel/channels` answer (spec 3.3 §10 K1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountsResponse {
    /// Every paired chat account, oldest first, removed ones included.
    pub accounts: Vec<ChannelAccountView>,
    /// The accounts waiting for their code to be accepted.
    pub waiting: Vec<ChannelPairingView>,
}

/// The answer of `POST /kernel/channels/waiting/{id}/dismiss`: the request it dismissed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPairingResponse {
    /// The dismissed request.
    pub waiting: ChannelPairingView,
}

/// The answer of a chat-account action: the account after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountResponse {
    /// The account.
    pub account: ChannelAccountView,
}

type Backend = State<Arc<dyn ControlBackend>>;

/// The operator routes of `backend`'s pairing, relative to `/api`; mounted on every kernel,
/// so one that pairs nothing answers with what to set rather than an empty `404`.
pub(super) fn routes(backend: Arc<dyn ControlBackend>) -> Router {
    Router::new()
        .route(paths::PAIRING, post(issue))
        .route(paths::DEVICES, get(devices))
        .route(paths::DEVICE_REVOKE, post(revoke))
        .route(paths::CHANNELS, get(channels::list))
        .route(paths::CHANNEL_PAIR, post(channels::accept))
        .route(paths::CHANNEL_REVOKE, post(channels::revoke))
        .route(paths::CHANNEL_DISMISS, post(channels::dismiss))
        .with_state(backend)
}

/// The claim route under `/api`, which needs no caller: the code is the credential.
pub(super) fn claim_routes(backend: Arc<dyn ControlBackend>) -> Router {
    Router::new()
        .route(&format!("{}{}", paths::API_PREFIX, paths::PAIRING_CLAIM), post(claim))
        .with_state(backend)
}

/// `backend`'s pairing, or a `Config` error naming why there is none.
fn pairing_of(backend: &dyn ControlBackend) -> Result<Pairing> {
    backend.pairing().ok_or_else(|| KernelError::Config {
        reason: "this kernel pairs no devices: set ControlPlaneConfig::with_pairing, as \
                 `adk-rust up` does"
            .into(),
    })
}

async fn issue(
    State(backend): Backend,
    caller: Caller,
    Json(request): Json<IssueRequest>,
) -> Result<Json<IssuedCode>> {
    caller.require_operator("issue a pairing code")?;
    let pairing = pairing_of(backend.as_ref())?;
    let mut code = IssueCode::new(caller.context.user_id.clone(), caller.context.scopes.clone())
        .with_scopes(request.scopes.iter().copied());
    if let Some(name) = &request.name {
        code = code.with_name(name);
    }
    if let Some(days) = request.expires_in_days {
        code = code.with_device_ttl(std::time::Duration::from_secs(u64::from(days) * 86_400));
    }
    // The record names what the code grants: every scope when none was named, and the
    // name as it is stored; a request the pairing refuses records nothing.
    let code = code.normalized();
    pairing.check(&code)?;
    let extra = [
        ("scopes", json!(code.scopes)),
        ("name", json!(code.name)),
        ("expiresInDays", json!(request.expires_in_days)),
    ];
    record(backend.as_ref(), &caller, "pairing.issue", None, &extra).await?;
    Ok(Json(pairing.issue(code).await?))
}

async fn claim(
    State(backend): Backend,
    Json(request): Json<ClaimRequest>,
) -> Result<Json<ClaimedDevice>> {
    let pairing = pairing_of(backend.as_ref())?;
    // The code is taken, then the claim recorded, then the device stored: a claim nobody
    // recorded pairs nothing and leaves its code to be claimed again.
    let pending = pairing.take_for_claim(&request.code, request.name.as_deref()).await?;
    let device = pending.device();
    let extra = [
        ("device", json!(device.id)),
        ("name", json!(device.name)),
        ("scopes", json!(device.scopes)),
    ];
    let recorded = record_as(
        backend.as_ref(),
        SubjectRef::human(device.owner.clone()),
        ControlTransport::Http,
        "pairing.claim",
        Some(&device.id),
        &extra,
    )
    .await;
    if let Err(error) = recorded {
        pairing.abandon_claim(pending).await;
        return Err(error);
    }
    Ok(Json(pairing.finish_claim(pending).await?))
}

async fn devices(State(backend): Backend, caller: Caller) -> Result<Json<DevicesResponse>> {
    caller.require_operator("list the paired devices")?;
    let pairing = pairing_of(backend.as_ref())?;
    let now = pairing.now();
    let devices = pairing.devices().await?.iter().map(|device| device.view(now)).collect();
    Ok(Json(DevicesResponse { devices }))
}

async fn revoke(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<DeviceResponse>> {
    caller.require_operator("remove a paired device")?;
    let pairing = pairing_of(backend.as_ref())?;
    record(backend.as_ref(), &caller, "devices.revoke", Some(&id), &[]).await?;
    let device = pairing.revoke(&id).await?;
    Ok(Json(DeviceResponse { device: device.view(pairing.now()) }))
}

/// Writes a `kernel.control` record of `action` by `subject`, for a caller that carries no
/// request context (spec 3.2 §9.3).
pub(super) async fn record_as(
    backend: &dyn ControlBackend,
    subject: SubjectRef,
    transport: ControlTransport,
    action: &str,
    target: Option<&str>,
    extra: &[(&str, serde_json::Value)],
) -> Result<()> {
    tracing::info!(
        control.action = action,
        control.subject = %subject.id,
        control.transport = ?transport,
        control.target = target.unwrap_or_default(),
        "kernel control action"
    );
    let Some(trail) = backend.audit_trail() else {
        return Ok(());
    };
    let draft = AuditRecordDraft::new(
        AuditRecordKind::Other(audit::KERNEL_AUDIT_KIND.into()),
        audit::CONTROL,
    )
    .actor(AuditActor::subject(subject.clone()))
    .ext("subject", json!(subject))
    .ext("transport", json!(transport))
    .ext("action", json!(action))
    .ext("target", json!(target));
    let draft = extra.iter().fold(draft, |draft, (key, value)| draft.ext(*key, value.clone()));
    trail.record(draft).await.map_err(KernelError::from)
}

/// The largest decision a device sends: a decision's edited arguments included.
const MAX_DECIDE_BODY: usize = 2 * 1024 * 1024;

/// Whether `kernel` rates `request`'s call high risk whatever the request says: a tool a
/// mounted driver declares of a class nothing may allow without a person, `publish_public`
/// or `financial` (spec 3.2 K-I9). Chat and paired devices, which decide no high-risk call,
/// send it to the home (spec 3.3 §7.2, S-I6).
pub(crate) fn rated_high(
    kernel: Option<&crate::kernel::Kernel>,
    request: &ApprovalRequest,
) -> bool {
    #[cfg(feature = "drivers")]
    if let Some(drivers) = kernel.and_then(crate::kernel::Kernel::drivers) {
        return drivers
            .mounted()
            .iter()
            .filter_map(|id| drivers.record(id))
            .any(|record| record.manifest.class_of(&request.tool_name).is_never_auto_allowed());
    }
    let _ = (kernel, request);
    false
}

/// Admits a request carrying device `token` (spec 3.3 §10 K2): the device's context when
/// its scopes open the route, a decision it answers (`answer`, the body's `action`)
/// recorded and limited to low and medium risk, never a yes to a take-over, or the refusal
/// as a response.
pub(super) async fn admit_device(
    backend: &dyn ControlBackend,
    pairing: &Pairing,
    token: &str,
    method: &Method,
    path: &str,
    answer: Option<&str>,
) -> std::result::Result<RequestContext, Response> {
    let context =
        pairing.authorize(token, method, path).await.map_err(IntoResponse::into_response)?;
    if device_scope_for(method, path) != Some(DeviceScope::Decide) {
        return Ok(context);
    }
    let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    let id = percent_decode(segments.iter().rev().nth(1).copied().unwrap_or_default());
    // §7.2: a phone approves a high-risk call only after confirming it is the person, which
    // a device token alone does not show; a risk the kernel cannot read is refused too.
    let Some(approvals) = backend.approvals() else {
        return Err(crate::pairing::forbidden(
            "this kernel cannot check a decision's risk for a device: decide it in the home".into(),
        )
        .into_response());
    };
    match approvals.get(&id).await {
        Ok(Some(request))
            if request.risk >= ApprovalRisk::High || rated_high(backend.kernel(), &request) =>
        {
            return Err(crate::pairing::forbidden(
                "this decision is high risk: confirm it's you and approve it in the home".into(),
            )
            .into_response());
        }
        // A take-over is done by typing the secret; a yes delivers nothing (spec 3.3 §8.2).
        Ok(Some(request))
            if request.tool_name == crate::pairing::TAKE_OVER_TOOL && answer != Some("reject") =>
        {
            return Err(crate::pairing::forbidden(
                "this decision asks you to type a secret: take over in the home".into(),
            )
            .into_response());
        }
        Ok(_) => {}
        Err(error) => return Err(KernelError::Core(error).into_response()),
    }
    let device = context.metadata.get(DEVICE_ID_METADATA).cloned();
    let name = context.metadata.get(DEVICE_NAME_METADATA).cloned();
    let extra = [
        ("surface", json!("device")),
        ("device", json!(device)),
        ("deviceName", json!(name)),
        ("answer", json!(answer)),
    ];
    record_as(
        backend,
        SubjectRef::human(context.user_id.clone()),
        ControlTransport::Http,
        "approvals.decide",
        Some(&id),
        &extra,
    )
    .await
    .map_err(IntoResponse::into_response)?;
    Ok(context)
}

/// `request` with its body read when it is a device's decision, and the `action` the body
/// carries: the decision's record says what the device answered.
pub(super) async fn read_answer(
    request: Request<Body>,
) -> std::result::Result<(Request<Body>, Option<String>), Response> {
    if device_scope_for(request.method(), request.uri().path()) != Some(DeviceScope::Decide) {
        return Ok((request, None));
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_DECIDE_BODY).await else {
        return Err(axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response());
    };
    let answer = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|body| body.get("action")?.as_str().map(str::to_string));
    Ok((Request::from_parts(parts, Body::from(bytes)), answer))
}

/// `segment` with its `%XX` escapes decoded, as the router decodes a path parameter.
fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|byte| (*byte as char).to_digit(16));
        match (bytes[index], hex(index + 1), hex(index + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push(u8::try_from(high * 16 + low).unwrap_or_default());
                index += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `method` and `path` are the claim route, which needs no caller.
fn is_claim(method: &Method, path: &str) -> bool {
    *method == Method::POST
        && path.strip_prefix(paths::API_PREFIX).unwrap_or(path) == paths::PAIRING_CLAIM
}

/// Checks every request that carries a device token against `backend`'s pairing before it
/// reaches `router` (spec 3.3 §10 K2), so a device reaches only the routes its scopes open:
/// `401` for an unknown, expired or revoked token, `403` for a route it may not call. The
/// claim route passes untouched, since it needs no caller.
pub(super) fn guard(backend: Arc<dyn ControlBackend>, router: Router) -> Router {
    router.layer(middleware::from_fn(move |request: Request<Body>, next: Next| {
        let backend = backend.clone();
        async move {
            let Some(pairing) = backend.pairing() else {
                return next.run(request).await;
            };
            let Some(token) = device_token(request.headers()).map(str::to_string) else {
                return next.run(request).await;
            };
            let method = request.method().clone();
            let path = request.uri().path().to_string();
            // The claim needs no caller: a client still holding an old token re-pairs.
            if is_claim(&method, &path) {
                return next.run(request).await;
            }
            let (request, answer) = match read_answer(request).await {
                Ok(read) => read,
                Err(response) => return response,
            };
            match admit_device(
                backend.as_ref(),
                &pairing,
                &token,
                &method,
                &path,
                answer.as_deref(),
            )
            .await
            {
                Ok(context) => {
                    // The extractor behind the guard admits only the decisions it checked.
                    let mut request = request;
                    if let Some(device) = context.metadata.get(DEVICE_ID_METADATA) {
                        request.extensions_mut().insert(DeviceAdmission { device: device.clone() });
                    }
                    next.run(request).await
                }
                Err(response) => response,
            }
        }
    }))
}

/// The chat-account routes (spec 3.3 §10 K1), mounted on every kernel: one without chat
/// channels lists none and answers the others with what it lacks.
mod channels {
    use axum::Json;
    use axum::extract::{Path as UrlPath, State};

    use super::{
        AcceptChannelRequest, Backend, Caller, ChannelAccountResponse, ChannelAccountsResponse,
        ChannelPairingResponse,
    };
    use crate::error::{KernelError, Result};

    /// A chat account or waiting request `id` this kernel does not have.
    fn no_account(id: &str) -> KernelError {
        KernelError::Core(adk_core::AdkError::new(
            adk_core::ErrorComponent::Server,
            adk_core::ErrorCategory::NotFound,
            crate::error::NOT_FOUND_CODE,
            format!("no chat account {id}: list them with `adk-rust pair list`"),
        ))
    }

    #[cfg(feature = "channels")]
    fn bindings(
        backend: &dyn super::ControlBackend,
    ) -> Option<(crate::pairing::Pairing, std::sync::Arc<dyn crate::channels::ChannelBindingStore>)>
    {
        let pairing = backend.pairing()?;
        let store = pairing.channel_bindings().cloned()?;
        Some((pairing, store))
    }

    pub(super) async fn list(
        State(backend): Backend,
        caller: Caller,
    ) -> Result<Json<ChannelAccountsResponse>> {
        caller.require_operator("list the paired chat accounts")?;
        let empty = ChannelAccountsResponse { accounts: Vec::new(), waiting: Vec::new() };
        #[cfg(feature = "channels")]
        if let Some((pairing, store)) = bindings(backend.as_ref()) {
            let now = pairing.now();
            let accounts =
                store.list_bindings().await?.iter().map(|binding| binding.view()).collect();
            let waiting = store
                .list_requests()
                .await?
                .iter()
                .filter(|request| !request.dismissed && request.expires_at > now)
                .map(|request| request.view())
                .collect();
            return Ok(Json(ChannelAccountsResponse { accounts, waiting }));
        }
        let _ = backend;
        // A kernel without chat channels has none to list.
        Ok(Json(empty))
    }

    pub(super) async fn accept(
        State(backend): Backend,
        caller: Caller,
        Json(request): Json<AcceptChannelRequest>,
    ) -> Result<Json<ChannelAccountResponse>> {
        caller.require_operator("pair a chat account")?;
        #[cfg(feature = "channels")]
        {
            let (pairing, store) =
                bindings(backend.as_ref()).ok_or_else(|| KernelError::Config {
                    reason: "this kernel pairs no chat accounts: no chat channel is connected to \
                         it (Pairing::with_channel_bindings)"
                        .into(),
                })?;
            let now = pairing.now();
            // The code is taken, then the pairing recorded, then the account stored: an
            // account nobody recorded pairing is not paired, and its code still works.
            let waiting = crate::channels::take_pairing(store.as_ref(), &request.code, now).await?;
            let binding = crate::channels::binding_for(
                &waiting,
                &caller.context.user_id,
                &caller.context.scopes,
                now,
            );
            let extra = [
                ("channel", serde_json::json!(binding.channel)),
                ("account", serde_json::json!(binding.account)),
            ];
            let recorded = super::record(
                backend.as_ref(),
                &caller,
                "channels.pair",
                Some(&binding.id),
                &extra,
            )
            .await;
            if let Err(error) = recorded {
                crate::channels::restore_pairing(store.as_ref(), waiting).await;
                return Err(error);
            }
            if let Err(error) = store.put_binding(binding.clone()).await {
                crate::channels::restore_pairing(store.as_ref(), waiting).await;
                return Err(error);
            }
            Ok(Json(ChannelAccountResponse { account: binding.view() }))
        }
        #[cfg(not(feature = "channels"))]
        {
            let _ = (backend, request);
            Err(KernelError::Config {
                reason: "this kernel has no chat channels: it was built without adk-kernel's \
                         `channels` feature, so no chat account can be paired with it"
                    .into(),
            })
        }
    }

    pub(super) async fn revoke(
        State(backend): Backend,
        caller: Caller,
        UrlPath(id): UrlPath<String>,
    ) -> Result<Json<ChannelAccountResponse>> {
        caller.require_operator("remove a chat account")?;
        #[cfg(feature = "channels")]
        if let Some((pairing, store)) = bindings(backend.as_ref())
            && store.get_binding(&id).await?.is_some()
        {
            super::record(backend.as_ref(), &caller, "channels.revoke", Some(&id), &[]).await?;
            let binding =
                crate::channels::revoke_binding(store.as_ref(), &id, pairing.now()).await?;
            // Its turns that have not finished stop too: they would run as the person.
            if let Some(kernel) = backend.kernel() {
                crate::channels::stop_account_turns(kernel, &binding.channel, &binding.account)
                    .await;
            }
            return Ok(Json(ChannelAccountResponse { account: binding.view() }));
        }
        let _ = backend;
        Err(no_account(&id))
    }

    /// Dismisses the waiting chat account `id`: its code stops working, its place in the
    /// queue is freed, and it gets no new code for a day.
    pub(super) async fn dismiss(
        State(backend): Backend,
        caller: Caller,
        UrlPath(id): UrlPath<String>,
    ) -> Result<Json<ChannelPairingResponse>> {
        caller.require_operator("dismiss a chat account waiting to be paired")?;
        #[cfg(feature = "channels")]
        if let Some((pairing, store)) = bindings(backend.as_ref())
            && let Some(waiting) = crate::channels::waiting_request(store.as_ref(), &id).await?
        {
            super::record(backend.as_ref(), &caller, "channels.dismiss", Some(&id), &[]).await?;
            crate::channels::dismiss_request(store.as_ref(), &waiting, pairing.now()).await?;
            return Ok(Json(ChannelPairingResponse { waiting: waiting.view() }));
        }
        let _ = backend;
        Err(no_account(&id))
    }
}

#[cfg(test)]
mod tests {
    use super::percent_decode;

    #[test]
    fn path_segments_decode_as_the_router_decodes_them() {
        assert_eq!(percent_decode("apr_1f"), "apr_1f");
        assert_eq!(percent_decode("a%2Fb%20c"), "a/b c");
        assert_eq!(percent_decode("bad%2"), "bad%2");
        assert_eq!(percent_decode("%E6%97%A5"), "日");
    }
}
