//! Agent-drawn surfaces over AG-UI (spec 3.3 §6.1 `genui`, §10 S2): [`GenuiAgUi`], the
//! extension a host sets on adk-server's AG-UI runtime.
//!
//! | What | How |
//! |---|---|
//! | A drawing | An `ACTIVITY_SNAPSHOT` with `activityType: "a2ui-surface"` after the render tool's result, for a drawing the event proves and the validator passes again |
//! | An `a2ui-surface` activity a client sends back | Kept only when it passes the validator again; dropped otherwise |
//! | `forwardedProps.a2uiAction` | The governed submit: checked against the stored surface, then held as a requested call, so the run ends with a `tool_call` interrupt |
//!
//! The activity's shape follows a community convention for A2UI over AG-UI, not a
//! specification; `tests/genui_ag_ui_activity.json` pins it.

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::{DRAW_TOOL_NAMES, Event, Part, RequestedCall, RequestedVia};
use adk_server::rest::controllers::runtime::AgUiInputMessage;
use adk_server::rest::controllers::{AgUiAction, AgUiExtension};
use adk_server::ui_protocol::{A2UI_SURFACE_ACTIVITY_TYPE, AG_UI_A2UI_ACTIVITY_FEATURE};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::a2ui::A2uiMessage;
use super::catalog::FORMS_CATALOG_ID;
use super::draw::DrawnEnvelope;
use super::drawn::DrawnKind;
use super::validate::{DrawnAction, Validator};
use crate::engine::SURFACE_AUDIT_DRAWN_SUBMITTED;
use crate::error::SurfaceError;

/// The `forwardedProps` key of a drawn form's send over AG-UI.
pub const A2UI_ACTION_PROP: &str = "a2uiAction";

/// A drawn form's send as an AG-UI client forwards it: the surface and version the person
/// saw, the action, and what they typed by data path.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForwardedAction {
    surface_id: String,
    version: u64,
    #[serde(alias = "name")]
    action_id: String,
    #[serde(default, alias = "context")]
    values: Map<String, Value>,
}

/// The content of an `a2ui-surface` activity.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityContent {
    #[serde(rename = "a2ui_operations")]
    operations: Vec<A2uiMessage>,
    #[serde(default)]
    kind: Option<DrawnKind>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    actions: Vec<DrawnAction>,
}

/// Agent-drawn surfaces on adk-server's AG-UI runtime (spec 3.3 §6.1 `genui`, §10 S2): set it
/// with `ServerBuilder::with_ag_ui_extension`, and `ag_ui` lists `a2ui_activity`.
///
/// A drawing a render tool provably made, and the validator passes again, follows its result
/// as an `ACTIVITY_SNAPSHOT` of type `a2ui-surface`, whose `content` holds the drawing's A2UI
/// messages as `a2ui_operations` with its surface, version, kind, title and actions. An
/// `a2ui-surface` activity a client sends back is kept only when it passes the validator too.
/// A run whose `forwardedProps.a2uiAction` is `{ surfaceId, version, actionId, values }` sends
/// that form as the web home's Send does: the stored surface at that version, its declared
/// action, the values typed per field and the constants from the stored surface; the run's
/// agent holds the call for the person, so the run ends with a `tool_call` interrupt. A form
/// another agent drew is refused (`403 surface.not_here`): its drawing agent holds it, in its own
/// target (wave 4 decision 13).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_server::rest::controllers::AgUiExtension;
/// use adk_surface::genui::GenuiAgUi;
///
/// let extension = GenuiAgUi::new();
/// assert_eq!(extension.features(), ["a2ui_activity"]);
/// assert!(extension.action_in(&serde_json::json!({ "a2uiAction": {} })));
/// # fn mount(builder: adk_server::ServerBuilder) -> adk_server::ServerBuilder {
/// builder.with_ag_ui_extension(Arc::new(GenuiAgUi::new()))
/// # }
/// ```
#[derive(Clone, Default)]
pub struct GenuiAgUi {
    audit: Option<AuditTrail>,
}

impl std::fmt::Debug for GenuiAgUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenuiAgUi").field("audit", &self.audit.is_some()).finish()
    }
}

impl GenuiAgUi {
    /// The extension, recording nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records every send on `trail` as `surface.drawn.submitted`, with `surface: "ag_ui"`,
    /// before its call is asked for.
    #[must_use]
    pub fn with_audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// The activity of a drawing: its A2UI messages and what checking them again needs.
    fn activity(envelope: &DrawnEnvelope) -> Value {
        json!({
            "type": "ACTIVITY_SNAPSHOT",
            "messageId": format!("a2ui-{}", envelope.surface_id),
            "activityType": A2UI_SURFACE_ACTIVITY_TYPE,
            "content": {
                "a2ui_operations": envelope.messages,
                "surfaceId": envelope.surface_id,
                "version": envelope.version,
                "catalogId": envelope.catalog_id,
                "kind": envelope.kind,
                "title": envelope.title,
                "actions": envelope.actions,
            },
            "replace": true,
        })
    }
}

#[async_trait]
impl AgUiExtension for GenuiAgUi {
    fn features(&self) -> Vec<&'static str> {
        vec![AG_UI_A2UI_ACTIVITY_FEATURE]
    }

    fn events_for(&self, event: &Event) -> Vec<Value> {
        let Some(content) = &event.llm_response.content else {
            return Vec::new();
        };
        let validator = Validator::forms();
        content
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, .. }
                    if DRAW_TOOL_NAMES.contains(&function_response.name.as_str()) =>
                {
                    super::view::proven(event, &function_response.response)
                }
                _ => None,
            })
            .filter(|envelope| {
                envelope.deleted
                    || envelope.draft().is_some_and(|draft| validator.validate(&draft).is_ok())
            })
            .map(|envelope| Self::activity(&envelope))
            .collect()
    }

    fn keeps_activity(&self, message: &AgUiInputMessage) -> bool {
        if message.activity_type.as_deref() != Some(A2UI_SURFACE_ACTIVITY_TYPE) {
            return true;
        }
        let Some(content) = message
            .content
            .clone()
            .and_then(|content| serde_json::from_value::<ActivityContent>(content).ok())
        else {
            return false;
        };
        let Some(surface_id) =
            content.operations.first().map(|message| message.surface_id().to_string())
        else {
            return false;
        };
        let envelope = DrawnEnvelope {
            surface_id,
            version: 0,
            kind: content.kind.unwrap_or(DrawnKind::Screen),
            catalog_id: FORMS_CATALOG_ID.to_string(),
            title: content.title,
            messages: content.operations,
            actions: content.actions,
            deleted: false,
        };
        let kept =
            envelope.draft().is_some_and(|draft| Validator::forms().validate(&draft).is_ok());
        if !kept {
            tracing::debug!(
                ag_ui.message_id = ?message.id,
                "dropped an a2ui-surface activity a client sent back that does not pass the validator"
            );
        }
        kept
    }

    fn action_in(&self, forwarded_props: &Value) -> bool {
        forwarded_props.get(A2UI_ACTION_PROP).is_some()
    }

    async fn requested_call(
        &self,
        action: AgUiAction<'_>,
    ) -> adk_core::Result<Option<RequestedCall>> {
        let Some(pressed) = action.forwarded_props.get(A2UI_ACTION_PROP) else {
            return Ok(None);
        };
        let pressed: ForwardedAction =
            serde_json::from_value(pressed.clone()).map_err(|error| {
                adk_core::AdkError::from(SurfaceError::Invalid(format!(
                    "forwardedProps.{A2UI_ACTION_PROP} is not {{ surfaceId, version, actionId, \
                     values }}: {error}"
                )))
            })?;
        let sent = super::submission(
            action.events,
            &pressed.surface_id,
            pressed.version,
            &pressed.action_id,
            &pressed.values,
        )
        .map_err(adk_core::AdkError::from)?;
        // The drawing agent holds its form's call; another agent never does (decision 13).
        if sent.drawn_by != action.agent_name {
            return Err(adk_core::AdkError::from(SurfaceError::NotHere(format!(
                "this form was drawn by '{}', not '{}', which runs here; send it from the home, \
                 where '{}' holds it",
                sent.drawn_by, action.agent_name, sent.drawn_by
            ))));
        }
        let call = RequestedCall::new(
            sent.tool.clone(),
            sent.args.clone(),
            action.requested_by.clone(),
            RequestedVia::form(pressed.surface_id.clone(), pressed.version, &pressed.action_id),
        );
        // Recorded before it is asked for, as the web home's send is.
        if let Some(trail) = &self.audit {
            let args_digest = trail.digest(&sent.args);
            let draft = AuditRecordDraft::new(
                AuditRecordKind::Other("surface".to_string()),
                SURFACE_AUDIT_DRAWN_SUBMITTED,
            )
            .actor(AuditActor::subject(action.requested_by.clone()))
            .ext("surface", json!("ag_ui"))
            .ext("sessionId", json!(action.session_id))
            .ext("surfaceId", json!(pressed.surface_id))
            .ext("version", json!(pressed.version))
            .ext("actionId", json!(pressed.action_id))
            .ext("tool", json!(sent.tool))
            .ext("argsDigest", json!(args_digest))
            .ext("valuesDigest", json!(sent.values_digest))
            .ext("callId", json!(call.function_call_id))
            .ext("drawnBy", json!(sent.drawn_by));
            trail.record(draft).await?;
        }
        Ok(Some(call))
    }
}
