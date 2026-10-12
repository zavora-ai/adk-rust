//! Signed inbound webhooks that wake agents: `POST /hooks/{name}` enqueues a durable run.
//!
//! Each [`WebhookHook`] names a [Standard Webhooks](adk_core::webhook) secret and how a
//! verified delivery becomes a [`NewRun`]. [`WebhookIngress`] serves the hooks:
//!
//! | Step | Refusal | Status |
//! |------|---------|--------|
//! | Look up the hook by name | Unknown hook | `404` |
//! | Verify the signature and timestamp | Missing, malformed, expired, or mismatched | `401` (reason logged, never returned) |
//! | Check the kill switch | Frozen, or a replicated control that is stale | `503` with `Retry-After` |
//! | Check the hook's pending-run cap | `max_pending` unfinished runs from this hook | `429` with `Retry-After` |
//! | Parse the body and render the mapping | Not JSON, or a template that renders an empty key | `422` |
//! | Enqueue with idempotency key `hook:{name}:{webhook-id}` | Store unreachable | `503` with `Retry-After` |
//! | | Rejected by `OnConflict::Reject` | `409` with `blockingRunId` |
//!
//! A new run answers `202 Accepted`; a redelivery of a `webhook-id` that already has a
//! run answers `200 OK` with that run, so a sender's retry never creates a second run.
//! Bodies are `{"runId", "state", "duplicate"}` in camelCase.
//!
//! Every run a hook creates acts for its user as a service
//! ([`NewRun::with_service_subject`]), so its audit records name a service, not a person.
//!
//! The run executes when a [`DurableWorker`](crate::background::DurableWorker) (feature
//! `background`) claims it from the same [`DurableRunStore`] — the worker that runs every
//! other durable run, under the runner's budget, policy, and kill switch. With a
//! [`GovernanceControl::mirrored`] control, a freeze written by any replica, or a mirror
//! that has gone stale, refuses deliveries on every replica.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_core::run_store::{InMemoryRunStore, OnConflict};
//! use adk_core::webhook::{WebhookSecret, WebhookVerifier};
//! use adk_server::webhooks::{WebhookHook, WebhookIngress};
//! use serde_json::json;
//!
//! let secret = WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?;
//! let ingress = WebhookIngress::new(Arc::new(InMemoryRunStore::new())).with_hook(
//!     WebhookHook::new("github", WebhookVerifier::new(secret), "triage", "triage_agent")
//!         .with_input_template(json!({"text": "Triage issue #{{/payload/issue/number}}: {{/payload/issue/title}}"}))
//!         .with_concurrency_key("repo:{{/payload/repository/id}}", OnConflict::Enqueue)
//!         .with_max_pending(100),
//! );
//! let router = adk_server::webhooks::webhook_ingress_router(ingress);
//! # let _ = router;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::GovernanceControl;
use adk_core::run_store::{DurableRunStore, Enqueued, NewRun, OnConflict, RunFilter, RunState};
use adk_core::webhook::{VerifiedWebhook, WebhookHeaders, WebhookVerifier};
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Serialize;
use serde_json::{Value, json};

/// Largest accepted webhook body.
pub const DEFAULT_HOOK_BODY_LIMIT: usize = 1024 * 1024;

/// A verified delivery, as a custom mapper sees it.
#[derive(Debug, Clone, Copy)]
pub struct HookDelivery<'a> {
    /// The hook's name.
    pub hook: &'a str,
    /// The verified `webhook-id` and signing time.
    pub webhook: &'a VerifiedWebhook,
    /// The parsed JSON body.
    pub payload: &'a Value,
}

type HookMapper = Arc<dyn Fn(&HookDelivery<'_>) -> adk_core::Result<NewRun> + Send + Sync>;

/// One inbound webhook: its secret and the run each delivery enqueues.
///
/// Templates are JSON values. A string that is exactly `{{/json/pointer}}` becomes the
/// value at that pointer; a string that contains `{{/json/pointer}}` placeholders has
/// each replaced by the value as text. Pointers resolve against
/// `{"payload": <body>, "webhook": {"id", "timestamp"}, "hook": {"name"}}`.
///
/// In the input template a missing value renders as `null` (whole-string placeholder)
/// or empty text. In the user, session, and concurrency-key templates a missing value
/// refuses the delivery with `422`, so deliveries that lack the field never share a key.
#[derive(Clone)]
pub struct WebhookHook {
    name: String,
    verifier: WebhookVerifier,
    queue: String,
    app_name: String,
    target: String,
    user_id: String,
    session_id: Option<String>,
    input: Option<Value>,
    concurrency_key: Option<(String, OnConflict)>,
    max_attempts: u32,
    max_pending: Option<usize>,
    mapper: Option<HookMapper>,
}

impl std::fmt::Debug for WebhookHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookHook")
            .field("name", &self.name)
            .field("queue", &self.queue)
            .field("app_name", &self.app_name)
            .field("target", &self.target)
            .field("max_pending", &self.max_pending)
            .finish_non_exhaustive()
    }
}

impl WebhookHook {
    /// Creates a hook served at `/hooks/{name}` that enqueues runs of `target` in
    /// `app_name` on the `default` queue, as user `webhook:{name}`, with the body as input.
    pub fn new(
        name: impl Into<String>,
        verifier: WebhookVerifier,
        app_name: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        let name = name.into();
        Self {
            user_id: format!("webhook:{name}"),
            name,
            verifier,
            queue: "default".to_string(),
            app_name: app_name.into(),
            target: target.into(),
            session_id: None,
            input: None,
            concurrency_key: None,
            max_attempts: NewRun::DEFAULT_MAX_ATTEMPTS,
            max_pending: None,
            mapper: None,
        }
    }

    /// The hook's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sets the queue runs are enqueued on.
    pub fn with_queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = queue.into();
        self
    }

    /// Sets the user template.
    pub fn with_user_id(mut self, template: impl Into<String>) -> Self {
        self.user_id = template.into();
        self
    }

    /// Continues the session the template names instead of starting a new one per run.
    pub fn with_session_id(mut self, template: impl Into<String>) -> Self {
        self.session_id = Some(template.into());
        self
    }

    /// Sets the input template; without one the body is the input.
    pub fn with_input_template(mut self, template: Value) -> Self {
        self.input = Some(template);
        self
    }

    /// Runs sharing the rendered key run one at a time; `on_conflict` decides what a
    /// delivery does while one is unfinished.
    pub fn with_concurrency_key(
        mut self,
        template: impl Into<String>,
        on_conflict: OnConflict,
    ) -> Self {
        self.concurrency_key = Some((template.into(), on_conflict));
        self
    }

    /// Sets the most attempts each run may take.
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Answers `429` while this many runs from this hook are unfinished.
    pub fn with_max_pending(mut self, max_pending: usize) -> Self {
        self.max_pending = Some(max_pending);
        self
    }

    /// Replaces the templates with code. The idempotency key is still set from the
    /// `webhook-id`, overriding any key the mapper sets.
    pub fn with_mapper(
        mut self,
        mapper: impl Fn(&HookDelivery<'_>) -> adk_core::Result<NewRun> + Send + Sync + 'static,
    ) -> Self {
        self.mapper = Some(Arc::new(mapper));
        self
    }

    /// The idempotency key of the run a delivery creates.
    ///
    /// # Example
    ///
    /// ```rust
    /// assert_eq!(adk_server::webhooks::WebhookHook::idempotency_key("github", "msg_1"), "hook:github:msg_1");
    /// ```
    pub fn idempotency_key(hook: &str, webhook_id: &str) -> String {
        format!("hook:{hook}:{webhook_id}")
    }

    fn map(&self, delivery: &HookDelivery<'_>) -> Result<NewRun, String> {
        let mut run = match &self.mapper {
            Some(mapper) => mapper(delivery).map_err(|error| error.message.clone())?,
            None => {
                let context = json!({
                    "payload": delivery.payload,
                    "webhook": {
                        "id": delivery.webhook.id,
                        "timestamp": delivery.webhook.timestamp.to_rfc3339(),
                    },
                    "hook": { "name": self.name },
                });
                let text = |field: &str, template: &str| {
                    let rendered = render_key(template, &context)
                        .map_err(|reason| format!("{field} template `{template}`: {reason}"))?;
                    if rendered.trim().is_empty() {
                        Err(format!("{field} template `{template}` rendered empty"))
                    } else {
                        Ok(rendered)
                    }
                };
                let input = match &self.input {
                    Some(template) => render(template, &context),
                    None => delivery.payload.clone(),
                };
                let mut run = NewRun::new(
                    self.queue.clone(),
                    self.app_name.clone(),
                    text("userId", &self.user_id)?,
                    self.target.clone(),
                    input,
                )
                .with_max_attempts(self.max_attempts);
                if let Some(template) = &self.session_id {
                    run = run.with_session_id(text("sessionId", template)?);
                }
                if let Some((template, on_conflict)) = &self.concurrency_key {
                    run = run.with_concurrency_key(text("concurrencyKey", template)?, *on_conflict);
                }
                run
            }
        };
        run.idempotency_key = Some(Self::idempotency_key(&self.name, &delivery.webhook.id));
        run.validate().map_err(|error| error.message)?;
        Ok(run.with_service_subject())
    }
}

/// Renders a template value; see [`WebhookHook`].
fn render(template: &Value, context: &Value) -> Value {
    match template {
        Value::String(text) => {
            let trimmed = text.trim();
            if let Some(pointer) =
                trimmed.strip_prefix("{{").and_then(|rest| rest.strip_suffix("}}"))
                && !pointer.contains("{{")
            {
                return context.pointer(pointer.trim()).cloned().unwrap_or(Value::Null);
            }
            Value::String(render_text(text, context))
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| render(item, context)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter().map(|(key, value)| (key.clone(), render(value, context))).collect(),
        ),
        other => other.clone(),
    }
}

/// Replaces each `{{/pointer}}` in `template` with the pointed value as text; a missing
/// or null value renders empty.
fn render_text(template: &str, context: &Value) -> String {
    render_placeholders(template, context, false).unwrap_or_default()
}

/// Like [`render_text`], but a missing or null value is an error, so deliveries that lack
/// the field never share a key.
fn render_key(template: &str, context: &Value) -> Result<String, String> {
    render_placeholders(template, context, true)
}

fn render_placeholders(template: &str, context: &Value, strict: bool) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else { break };
        out.push_str(&rest[..start]);
        let pointer = rest[start + 2..start + 2 + len].trim();
        match context.pointer(pointer) {
            Some(Value::String(text)) => out.push_str(text),
            Some(Value::Null) | None if strict => {
                return Err(format!("`{pointer}` is missing from the delivery"));
            }
            Some(Value::Null) | None => {}
            Some(other) => out.push_str(&other.to_string()),
        }
        rest = &rest[start + 2 + len + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The webhook endpoints and the run store they enqueue into.
#[derive(Clone)]
pub struct WebhookIngress {
    store: Arc<dyn DurableRunStore>,
    hooks: Arc<HashMap<String, WebhookHook>>,
    governance: Option<GovernanceControl>,
    retry_after: Duration,
    body_limit: usize,
}

impl std::fmt::Debug for WebhookIngress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookIngress")
            .field("hooks", &self.hooks.keys().collect::<Vec<_>>())
            .field("retry_after", &self.retry_after)
            .field("body_limit", &self.body_limit)
            .finish_non_exhaustive()
    }
}

impl WebhookIngress {
    /// Creates an ingress with no hooks over `store`.
    pub fn new(store: Arc<dyn DurableRunStore>) -> Self {
        Self {
            store,
            hooks: Arc::new(HashMap::new()),
            governance: None,
            retry_after: Duration::from_secs(30),
            body_limit: DEFAULT_HOOK_BODY_LIMIT,
        }
    }

    /// Adds a hook, replacing one with the same name.
    pub fn with_hook(mut self, hook: WebhookHook) -> Self {
        Arc::make_mut(&mut self.hooks).insert(hook.name.clone(), hook);
        self
    }

    /// Sets the kill switch deliveries are checked against.
    ///
    /// [`ServerBuilder::with_webhook_ingress`](crate::ServerBuilder::with_webhook_ingress)
    /// installs [`ServerConfig::governance`](crate::ServerConfig::governance) when none
    /// is set here.
    pub fn with_governance(mut self, governance: GovernanceControl) -> Self {
        self.governance = Some(governance);
        self
    }

    /// Sets the `Retry-After` sent with `429` and `503` answers.
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = retry_after;
        self
    }

    /// Sets the largest accepted body.
    pub fn with_body_limit(mut self, bytes: usize) -> Self {
        self.body_limit = bytes;
        self
    }

    pub(crate) fn has_governance(&self) -> bool {
        self.governance.is_some()
    }

    /// The run store deliveries enqueue into.
    pub fn store(&self) -> &Arc<dyn DurableRunStore> {
        &self.store
    }

    /// Unfinished runs this hook created.
    async fn pending(
        &self,
        hook: &WebhookHook,
    ) -> adk_core::Result<Vec<adk_core::run_store::RunRecord>> {
        let prefix = format!("hook:{}:", hook.name);
        let unfinished = self
            .store
            .list(RunFilter::default().queue(hook.queue.clone()).states([
                RunState::Queued,
                RunState::Running,
                RunState::Waiting,
            ]))
            .await?;
        Ok(unfinished
            .into_iter()
            .filter(|run| {
                run.idempotency_key.as_deref().is_some_and(|key| key.starts_with(&prefix))
            })
            .collect())
    }

    fn retry_later(&self, status: StatusCode, code: &str, message: String) -> Response {
        let mut response =
            (status, Json(json!({"error": code, "message": message}))).into_response();
        if let Ok(value) = HeaderValue::from_str(&self.retry_after.as_secs().max(1).to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

/// A run accepted for a delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookAccepted {
    /// The run's identifier.
    pub run_id: String,
    /// The run's state.
    pub state: RunState,
    /// Whether the delivery was a redelivery of one that already has this run.
    pub duplicate: bool,
}

async fn receive(
    State(ingress): State<WebhookIngress>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(hook) = ingress.hooks.get(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "hook.not_found"}))).into_response();
    };
    let parsed =
        WebhookHeaders::from_lookup(|name| headers.get(name).and_then(|v| v.to_str().ok()));
    let verified = match parsed.and_then(|parsed| hook.verifier.verify(&parsed, &body)) {
        Ok(verified) => verified,
        Err(error) => {
            tracing::warn!(hook.name = %name, error.code = error.code(), error = %error, "webhook rejected");
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"})))
                .into_response();
        }
    };
    if let Some(governance) = &ingress.governance
        && let Err(error) = governance.check()
    {
        tracing::info!(hook.name = %name, webhook.id = %verified.id, error.code = %error.code, "webhook refused by governance");
        return ingress.retry_later(StatusCode::SERVICE_UNAVAILABLE, error.code, error.message);
    }
    if let Some(max_pending) = hook.max_pending {
        match ingress.pending(hook).await {
            Ok(pending) if pending.len() >= max_pending => {
                // A redelivery of a message whose run is still pending is answered, not throttled.
                let key = WebhookHook::idempotency_key(&hook.name, &verified.id);
                if let Some(run) =
                    pending.iter().find(|run| run.idempotency_key.as_deref() == Some(key.as_str()))
                {
                    return (
                        StatusCode::OK,
                        Json(HookAccepted {
                            run_id: run.run_id.clone(),
                            state: run.state,
                            duplicate: true,
                        }),
                    )
                        .into_response();
                }
                return ingress.retry_later(
                    StatusCode::TOO_MANY_REQUESTS,
                    "hook.max_pending",
                    format!(
                        "hook '{name}' has {} unfinished runs (max {max_pending})",
                        pending.len()
                    ),
                );
            }
            Ok(_) => {}
            Err(error) => {
                return ingress.retry_later(
                    StatusCode::SERVICE_UNAVAILABLE,
                    error.code,
                    error.message,
                );
            }
        }
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(error) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": "hook.invalid_body", "message": format!("body is not JSON: {error}")})),
            )
                .into_response();
        }
    };
    let delivery = HookDelivery { hook: &hook.name, webhook: &verified, payload: &payload };
    let run = match hook.map(&delivery) {
        Ok(run) => run,
        Err(message) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": "hook.mapping_failed", "message": message})),
            )
                .into_response();
        }
    };
    match ingress.store.enqueue(run).await {
        Ok(Enqueued::Created(record)) => {
            tracing::info!(hook.name = %name, webhook.id = %verified.id, run.id = %record.run_id, "webhook woke a run");
            (
                StatusCode::ACCEPTED,
                Json(HookAccepted { run_id: record.run_id, state: record.state, duplicate: false }),
            )
                .into_response()
        }
        Ok(Enqueued::Existing(record)) => {
            tracing::info!(hook.name = %name, webhook.id = %verified.id, run.id = %record.run_id, "webhook redelivery matched an existing run");
            (
                StatusCode::OK,
                Json(HookAccepted { run_id: record.run_id, state: record.state, duplicate: true }),
            )
                .into_response()
        }
        Ok(Enqueued::Rejected { blocking_run_id }) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "hook.concurrency_conflict", "blockingRunId": blocking_run_id})),
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(hook.name = %name, webhook.id = %verified.id, error = %error, "webhook not enqueued");
            ingress.retry_later(StatusCode::SERVICE_UNAVAILABLE, error.code, error.message)
        }
    }
}

/// Serves `POST /hooks/{name}` for `ingress`, with no authentication layer: the
/// signature is the authentication.
///
/// [`ServerBuilder::with_webhook_ingress`](crate::ServerBuilder::with_webhook_ingress)
/// mounts this router and installs the server's kill switch.
pub fn webhook_ingress_router(ingress: WebhookIngress) -> Router {
    let limit = ingress.body_limit;
    Router::new()
        .route("/hooks/{name}", post(receive))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(ingress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_render_values_and_text() {
        let context = json!({"payload": {"issue": {"number": 42, "title": "Crash"}, "tags": ["a"]}, "hook": {"name": "gh"}});
        let rendered = render(
            &json!({
                "text": "Issue #{{/payload/issue/number}}: {{/payload/issue/title}}{{/payload/missing}}",
                "tags": "{{/payload/tags}}",
                "missing": "{{ /payload/nope }}",
                "fixed": 3
            }),
            &context,
        );
        assert_eq!(
            rendered,
            json!({"text": "Issue #42: Crash", "tags": ["a"], "missing": null, "fixed": 3})
        );
        assert_eq!(render_text("{{/hook/name}}:{{/payload/issue/number}}", &context), "gh:42");
        assert_eq!(render_text("unterminated {{/hook/name", &context), "unterminated {{/hook/name");
        assert_eq!(
            render_key("repo:{{/payload/issue/number}}", &context),
            Ok("repo:42".to_string())
        );
        assert!(render_key("thread:{{/payload/thread}}", &context).is_err());
    }
}
