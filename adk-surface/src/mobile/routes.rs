//! The phone's routes (spec 3.3 §5.4, §8.2), relative to `/api`.

use adk_core::AdkError;
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind};
use adk_kernel::pairing::DeviceRecord;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};
use serde_json::{Value, json};

use super::passkey::REGISTRATION_WINDOW;
use super::push::Subscription;
use super::store::{Attachment, Holder, PASSKEY, PUSH, PUSH_KEY};
use super::{Home, MobileError, Phone, SURFACE_AUDIT_PHONE_PASSKEY, SURFACE_AUDIT_PHONE_PUSH, qr};
use crate::engine::{Command, CommandOutcome};
use crate::error::SurfaceError;
use crate::origin::{Caller, SurfaceKind};
use crate::web::phone::{
    ComputerPasskey, ConfirmOptionsRequest, ConfirmRequest, DeliveriesResponse,
    PasskeyRegistration, PhoneDevice, PhoneDevicesResponse, PhoneInfo, PushReceivedRequest,
    PushSubscriptionRequest, QrRequest,
};
use crate::web::{
    Context, Unauthenticated, caller, device_kind, device_of, failed, paths, problem,
    unauthenticated,
};

/// The phone's routes, relative to `/api` (spec 3.3 §5.4, §8.2): mount them with
/// `ServerBuilder::add_api_routes` beside [`web::router`](crate::web::router) and
/// [`web::devices_router`](crate::web::devices_router) over the same engine. See the
/// [module](super) for the table.
///
/// # Example
///
/// ```rust,no_run
/// use adk_surface::mobile::Phone;
///
/// # fn example(phone: Phone, builder: adk_server::ServerBuilder) {
/// let builder = builder.add_api_routes(adk_surface::mobile::router(phone));
/// # let _ = builder;
/// # }
/// ```
pub fn router(phone: Phone) -> Router {
    Router::new()
        .route(paths::PHONE, get(info))
        .route(paths::PHONE_PUSH, put(subscribe).delete(unsubscribe))
        .route(paths::PHONE_PUSH_RECEIVED, post(received))
        .route(paths::PHONE_PUSH_DELIVERIES, get(deliveries))
        .route(paths::PHONE_PASSKEY_OPTIONS, post(passkey_options))
        .route(paths::PHONE_PASSKEY, post(passkey_register))
        .route(paths::PHONE_PASSKEY_ONE, delete(passkey_remove))
        .route(paths::PHONE_QR, post(qr_code))
        .route(paths::PHONE_DEVICES, get(devices))
        .route(paths::CONFIRM_OPTIONS, post(confirm_options))
        .route(paths::CONFIRM, post(confirm))
        .with_state(phone)
}

/// Who calls: a paired phone, or the person at the web home.
enum Who {
    Phone { caller: Caller, device: String },
    Person { caller: Caller },
}

impl Who {
    fn caller(&self) -> &Caller {
        match self {
            Self::Phone { caller, .. } | Self::Person { caller } => caller,
        }
    }

    fn holder(&self) -> Holder {
        match self {
            Self::Phone { device, .. } => Holder::Device(device.clone()),
            Self::Person { caller } => Holder::User(caller.user_id().to_string()),
        }
    }
}

fn mobile_failed(error: MobileError) -> Response {
    problem(&AdkError::from(error))
}

/// A response that ends a handler early.
type Refused = Box<Response>;

fn who(context: Context) -> Result<Who, Refused> {
    let device = match &context {
        Some(Extension(Some(context))) => device_of(context)
            .map(|device| (device.clone(), device_kind(context).map(str::to_string))),
        _ => None,
    };
    let caller = match caller(context, SurfaceKind::Web, None) {
        Ok(caller) => caller,
        Err(Unauthenticated) => return Err(Box::new(unauthenticated())),
    };
    match device {
        Some((device, Some(kind))) if kind == "phone" => Ok(Who::Phone { caller, device }),
        Some((_, kind)) => Err(Box::new(failed(SurfaceError::NotHere(format!(
            "these are a paired phone's routes, and this device is a {}; pair a phone with \
             adk-rust pair --kind phone",
            kind.as_deref().unwrap_or("terminal")
        ))))),
        None => Ok(Who::Person { caller }),
    }
}

fn home(phone: &Phone) -> Result<&Home, Refused> {
    phone.inner.home.as_ref().ok_or_else(|| Box::new(mobile_failed(MobileError::NeedsHomeUrl)))
}

/// The operator at the web home: never a device.
fn operator(phone: &Phone, who: &Who) -> Result<(), Refused> {
    match who {
        Who::Phone { .. } => Err(Box::new(failed(SurfaceError::NotHere(
            "this is the operator's, in the home".into(),
        )))),
        Who::Person { caller } if caller.groups.iter().any(|g| g == &phone.inner.operator) => {
            Ok(())
        }
        Who::Person { .. } => Err(Box::new(failed(SurfaceError::OperatorOnly {
            group: phone.inner.operator.clone(),
        }))),
    }
}

macro_rules! or_respond {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(response) => return *response,
        }
    };
}

macro_rules! or_fail {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(error) => return mobile_failed(MobileError::from(error)),
        }
    };
}

/// The active device record of a phone caller.
async fn device_record(phone: &Phone, id: &str) -> Result<DeviceRecord, MobileError> {
    let now = phone.inner.clock.now();
    match phone.inner.pairing.store().get_device(id).await.map_err(AdkError::from)? {
        Some(device) if device.is_active(now) => Ok(device),
        _ => Err(MobileError::Surface(SurfaceError::NotHere(
            "this phone was removed or its pairing ended: pair it again".into(),
        ))),
    }
}

async fn record(phone: &Phone, action: &str, who: &Who, fields: Value) {
    let Some(trail) = &phone.inner.audit else { return };
    let caller = who.caller();
    let mut draft = AuditRecordDraft::new(AuditRecordKind::Other("surface".into()), action)
        .actor(AuditActor::subject(caller.subject.clone()))
        .ext("surface", serde_json::to_value(caller.surface).unwrap_or(Value::Null));
    if let Some(device) = &caller.device {
        draft = draft.ext("device", json!(device));
    }
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            draft = draft.ext(key, value);
        }
    }
    if let Err(error) = trail.record(draft).await {
        tracing::warn!(action = action, error = %error, "a phone record failed");
    }
}

/// Whether `holder` has a subscription this server wakes: one made with its push key. One made
/// with a key since replaced is not, so the phone offers "Allow notifications" again (spec 3.3
/// §5.4).
async fn has_push(phone: &Phone, holder: &Holder) -> Result<bool, MobileError> {
    let key = phone.inner.sender.vapid().key().id();
    Ok(phone.inner.store.list(holder, PUSH).await?.into_iter().any(|row| {
        serde_json::from_value::<Subscription>(row.record)
            .is_ok_and(|subscription| subscription.key_id == key)
    }))
}

async fn info(State(phone): State<Phone>, context: Context) -> Response {
    let who = or_respond!(who(context));
    let home = or_respond!(home(&phone));
    let holder = who.holder();
    let passkey = !or_fail!(phone.inner.passkeys.of(&holder).await).is_empty();
    let (push, can_add_passkey) = match &who {
        Who::Phone { device, .. } => {
            let record = or_fail!(device_record(&phone, device).await);
            let open = phone.inner.clock.now() - record.created_at <= REGISTRATION_WINDOW;
            (or_fail!(has_push(&phone, &holder).await), open && !passkey)
        }
        Who::Person { .. } => (false, true),
    };
    Json(PhoneInfo {
        push_key: phone.inner.sender.vapid().key().public_key().to_string(),
        rp_id: home.rp.id.clone(),
        passkey,
        push,
        can_add_passkey,
    })
    .into_response()
}

fn phone_only(who: &Who) -> Result<String, Refused> {
    match who {
        Who::Phone { device, .. } => Ok(device.clone()),
        Who::Person { .. } => Err(Box::new(failed(SurfaceError::NotHere(
            "wake-ups go to a paired phone; pair one from \"Add a phone or app\"".into(),
        )))),
    }
}

async fn subscribe(
    State(phone): State<Phone>,
    context: Context,
    Json(request): Json<PushSubscriptionRequest>,
) -> Response {
    let who = or_respond!(who(context));
    or_respond!(home(&phone));
    let device = or_respond!(phone_only(&who));
    let key_id = phone.inner.sender.vapid().key().id().to_string();
    let subscription = match Subscription::new(
        phone.inner.sender.allowlist(),
        &request.endpoint,
        &request.keys.p256dh,
        &request.keys.auth,
        &key_id,
    ) {
        Ok(subscription) => subscription,
        Err(error) => {
            record(&phone, SURFACE_AUDIT_PHONE_PUSH, &who, json!({ "outcome": "refused" })).await;
            return mobile_failed(error);
        }
    };
    let now = phone.inner.clock.now();
    let row = Attachment::new(
        Holder::Device(device),
        PUSH,
        PUSH_KEY,
        serde_json::to_value(&subscription).unwrap_or(Value::Null),
        now,
    );
    or_fail!(phone.inner.store.put(row).await);
    let host = request
        .endpoint
        .parse::<axum::http::Uri>()
        .ok()
        .and_then(|uri| uri.host().map(str::to_string));
    record(
        &phone,
        SURFACE_AUDIT_PHONE_PUSH,
        &who,
        json!({ "outcome": "subscribed", "host": host }),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

async fn unsubscribe(State(phone): State<Phone>, context: Context) -> Response {
    let who = or_respond!(who(context));
    let device = or_respond!(phone_only(&who));
    or_fail!(phone.inner.store.remove(&Holder::Device(device), PUSH, PUSH_KEY).await);
    record(&phone, SURFACE_AUDIT_PHONE_PUSH, &who, json!({ "outcome": "removed" })).await;
    StatusCode::NO_CONTENT.into_response()
}

async fn received(
    State(phone): State<Phone>,
    context: Context,
    Json(request): Json<PushReceivedRequest>,
) -> Response {
    let who = or_respond!(who(context));
    let device = or_respond!(phone_only(&who));
    if let Some(delivery) =
        phone.inner.deliveries.received(&request.id, &device, phone.inner.clock.now())
    {
        let span = tracing::info_span!("surface.phone.push.received", device.id = %device);
        span.in_scope(|| {
            tracing::info!(push.latency_ms = delivery.latency_ms, "wake-up received");
        });
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn deliveries(State(phone): State<Phone>, context: Context) -> Response {
    let who = or_respond!(who(context));
    or_respond!(operator(&phone, &who));
    Json(DeliveriesResponse { deliveries: phone.inner.deliveries.list() }).into_response()
}

/// A phone sets up its passkey within 10 minutes of pairing, once (spec 3.3 §8.2).
async fn may_add_passkey(phone: &Phone, who: &Who) -> Result<String, MobileError> {
    match who {
        Who::Phone { device, .. } => {
            let record = device_record(phone, device).await?;
            if phone.inner.clock.now() - record.created_at > REGISTRATION_WINDOW {
                return Err(MobileError::RegistrationClosed);
            }
            if !phone.inner.passkeys.of(&who.holder()).await?.is_empty() {
                return Err(MobileError::PasskeyExists);
            }
            Ok(record.name)
        }
        Who::Person { caller } => Ok(caller.user_id().to_string()),
    }
}

async fn passkey_options(State(phone): State<Phone>, context: Context) -> Response {
    let who = or_respond!(who(context));
    or_respond!(home(&phone));
    let name = or_fail!(may_add_passkey(&phone, &who).await);
    let now = phone.inner.clock.now();
    let options =
        or_fail!(phone.inner.passkeys.registration_options(who.holder(), &name, now).await);
    Json(options).into_response()
}

async fn passkey_register(
    State(phone): State<Phone>,
    context: Context,
    Json(request): Json<PasskeyRegistration>,
) -> Response {
    let who = or_respond!(who(context));
    or_respond!(home(&phone));
    or_fail!(may_add_passkey(&phone, &who).await);
    let now = phone.inner.clock.now();
    let registered = phone
        .inner
        .passkeys
        .register(&who.holder(), &request.registration_id, &request.credential, now)
        .await;
    match registered {
        Ok(passkey) => {
            record(
                &phone,
                SURFACE_AUDIT_PHONE_PASSKEY,
                &who,
                json!({ "outcome": "added", "credentialId": passkey.credential_id }),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            record(&phone, SURFACE_AUDIT_PHONE_PASSKEY, &who, json!({ "outcome": "refused" }))
                .await;
            mobile_failed(error)
        }
    }
}

async fn passkey_remove(
    State(phone): State<Phone>,
    context: Context,
    Path(id): Path<String>,
) -> Response {
    let who = or_respond!(who(context));
    if let Who::Phone { .. } = who {
        return failed(SurfaceError::NotHere(
            "a phone's passkey goes with the phone: remove the phone in the home".into(),
        ));
    }
    if !or_fail!(phone.inner.passkeys.remove(&who.holder(), &id).await) {
        return failed(SurfaceError::not_found(format!("passkey '{id}'")));
    }
    record(
        &phone,
        SURFACE_AUDIT_PHONE_PASSKEY,
        &who,
        json!({ "outcome": "removed", "credentialId": id }),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

async fn qr_code(
    State(phone): State<Phone>,
    context: Context,
    Json(request): Json<QrRequest>,
) -> Response {
    let who = or_respond!(who(context));
    or_respond!(operator(&phone, &who));
    let home = or_respond!(home(&phone));
    if !request.url.starts_with(&format!("{}/", home.url)) {
        return mobile_failed(MobileError::QrUrl { home: home.url.clone() });
    }
    let drawn = or_fail!(qr::svg(&request.url));
    ([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "no-store")], drawn)
        .into_response()
}

async fn devices(State(phone): State<Phone>, context: Context) -> Response {
    let who = or_respond!(who(context));
    or_respond!(operator(&phone, &who));
    let now = phone.inner.clock.now();
    let paired = or_fail!(phone.inner.pairing.devices().await.map_err(AdkError::from));
    let mut devices = Vec::new();
    for device in paired.into_iter().filter(|device| device.is_active(now)) {
        let holder = Holder::Device(device.id.clone());
        let passkey = !or_fail!(phone.inner.store.list(&holder, PASSKEY).await).is_empty();
        let push = or_fail!(has_push(&phone, &holder).await);
        devices.push(PhoneDevice { device_id: device.id, passkey, push });
    }
    let computer_passkeys = or_fail!(phone.inner.passkeys.of(&who.holder()).await)
        .into_iter()
        .map(|passkey| ComputerPasskey {
            id: passkey.credential_id,
            created_at: passkey.created_at,
        })
        .collect();
    Json(PhoneDevicesResponse { devices, computer_passkeys }).into_response()
}

async fn confirm_options(
    State(phone): State<Phone>,
    context: Context,
    Path(id): Path<String>,
    Json(request): Json<ConfirmOptionsRequest>,
) -> Response {
    let who = or_respond!(who(context));
    or_respond!(home(&phone));
    if let Who::Phone { device, .. } = &who {
        or_fail!(device_record(&phone, device).await);
    }
    // The caller's own decision (S-I8).
    if let Err(error) = phone.inner.engine.decision(who.caller(), &id).await {
        return failed(error);
    }
    let now = phone.inner.clock.now();
    let options =
        or_fail!(phone.inner.passkeys.confirm_options(who.holder(), &id, request, now).await);
    Json(options).into_response()
}

async fn confirm(
    State(phone): State<Phone>,
    context: Context,
    Path(id): Path<String>,
    Json(request): Json<ConfirmRequest>,
) -> Response {
    let who = or_respond!(who(context));
    or_respond!(home(&phone));
    if let Who::Phone { device, .. } = &who {
        or_fail!(device_record(&phone, device).await);
    }
    let now = phone.inner.clock.now();
    let binding = or_fail!(phone.inner.passkeys.confirm(&who.holder(), &id, &request, now).await);
    let command = Command::Decide {
        decision_id: binding.decision_id,
        version: binding.version,
        answer: binding.answer,
        edited_args: binding.edited_args,
        reason: None,
        expanded: binding.expanded,
    };
    let caller = who.caller().clone().with_presence_confirmed();
    match phone.inner.engine.execute(&caller, command).await {
        Ok(outcome) => Json::<CommandOutcome>(outcome).into_response(),
        Err(error) => failed(error),
    }
}
