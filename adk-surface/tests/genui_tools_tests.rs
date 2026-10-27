//! The render tools on a harness agent (spec 3.3 §6.1 `genui`, §7.4 rule 5): each drawing is
//! recorded with its version and digest under the reserved state key, needs no approval, and
//! a drawing that looks like a decision is refused in plain words.

#![cfg(feature = "genui")]

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_core::{Event, Part, SessionId, UserId};
use adk_harness::{Harness, HarnessAgent, InstructionsConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::genui::{
    ActionArg, DRAWN_STATE_KEY, DrawToolset, DrawnAction, DrawnEnvelope, DrawnKind, DrawnRecord,
    FieldArg, Validator,
};
use futures::StreamExt;
use serde_json::{Value, json};

/// `send_invoice`, which the drawn forms ask for; a send asks the person first, so it never
/// runs here.
struct SendInvoice;

#[async_trait::async_trait]
impl adk_core::Tool for SendInvoice {
    fn name(&self) -> &str {
        "send_invoice"
    }

    fn description(&self) -> &str {
        "Sends an invoice."
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        _args: Value,
    ) -> adk_core::Result<Value> {
        panic!("send_invoice never runs here");
    }
}

/// A harness agent `finance` with the render tools, in Ask first, over `sessions`.
async fn harness(
    script: Vec<ScriptedResponse>,
    sessions: Arc<dyn SessionService>,
    dir: &tempfile::TempDir,
) -> Harness {
    let agent = HarnessAgent::builder("finance")
        .model(Arc::new(ScriptedLlm::new("scripted", script)))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..InstructionsConfig::default()
        })
        .without_compaction()
        .toolset(Arc::new(DrawToolset::forms()))
        .tool(Arc::new(SendInvoice))
        .build()
        .unwrap();
    let agent = Arc::new(agent);
    let runner = agent
        .configure_runner(
            Runner::builder().app_name("home").agent(agent.clone()).session_service(sessions),
        )
        .audit_log(Arc::new(InMemoryAuditRecorder::new()))
        .build()
        .unwrap();
    Harness::with_runner(agent, runner).unwrap()
}

fn ana() -> UserId {
    UserId::new("ana").unwrap()
}

fn thread() -> SessionId {
    SessionId::new("thread-1").unwrap()
}

async fn send(harness: &Harness, text: &str) -> Vec<Event> {
    let session = harness.open_session(&ana(), Some(&thread())).await.unwrap();
    let stream = harness.send(&ana(), &session, text).await.unwrap();
    let events = tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish");
    events.into_iter().map(|event| event.expect("event")).collect()
}

/// Each drawing tool result in `events`: the tool, its response, and the event's
/// [`DRAWN_STATE_KEY`].
fn drawings(events: &[Event]) -> Vec<(String, Value, Option<Value>)> {
    events
        .iter()
        .flat_map(|event| {
            let record = event.actions.state_delta.get(DRAWN_STATE_KEY).cloned();
            event.llm_response.content.iter().flat_map(|content| &content.parts).filter_map(
                move |part| match part {
                    Part::FunctionResponse { function_response, .. }
                        if adk_core::DRAW_TOOL_NAMES.contains(&function_response.name.as_str()) =>
                    {
                        Some((
                            function_response.name.clone(),
                            function_response.response.clone(),
                            record.clone(),
                        ))
                    }
                    _ => None,
                },
            )
        })
        .collect()
}

fn invoice_form() -> Value {
    json!({
        "surfaceId": "invoice-10",
        "title": "Invoice INV-10",
        "fields": [
            { "name": "to", "label": "To", "type": "email", "required": true },
            { "name": "amount", "label": "Amount", "type": "number" }
        ],
        "actions": [{
            "id": "send",
            "label": "Send",
            "tool": "send_invoice",
            "args": { "to": { "path": "/to" }, "amount": { "path": "/amount" }, "invoice": "INV-10" }
        }]
    })
}

/// The state key of a drawing: its surface, version and digest.
fn record_of(envelope: &DrawnEnvelope) -> Value {
    json!(DrawnRecord {
        surface_id: envelope.surface_id.clone(),
        version: envelope.version,
        digest: envelope.digest(),
    })
}

#[tokio::test]
async fn a_form_is_drawn_recorded_and_needs_no_approval() {
    let dir = tempfile::tempdir().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let script = vec![
        ScriptedResponse::function_call("render_form", invoice_form()),
        ScriptedResponse::text("done"),
    ];
    let harness = harness(script, sessions, &dir).await;

    let events = send(&harness, "Prepare the October invoice for ACME").await;

    let drawn = drawings(&events);
    assert_eq!(drawn.len(), 1, "{drawn:?}");
    let (tool, response, record) = &drawn[0];
    assert_eq!(tool, "render_form");
    assert_eq!(response["status"], "shown");
    let envelope = DrawnEnvelope::from_response(response).expect("the result records the drawing");
    assert_eq!(record.as_ref(), Some(&record_of(&envelope)), "the reserved key proves the drawing");
    assert_eq!(
        (envelope.surface_id.as_str(), envelope.version, envelope.kind, envelope.actions.clone()),
        (
            "invoice-10",
            1,
            DrawnKind::Form,
            vec![DrawnAction {
                id: "send".into(),
                label: "Send".into(),
                tool: "send_invoice".into(),
                args: [
                    ("amount".to_string(), ActionArg::Field(FieldArg { path: "/amount".into() })),
                    ("invoice".to_string(), ActionArg::Constant(json!("INV-10"))),
                    ("to".to_string(), ActionArg::Field(FieldArg { path: "/to".into() })),
                ]
                .into(),
            }]
        )
    );
    let draft = envelope.draft().expect("a drawing, not a removal");
    assert!(Validator::forms().validate(&draft).is_ok(), "the recorded drawing checks again");
    assert!(events.iter().all(|event| event.actions.tool_confirmation.is_none()), "nothing waits");
    assert!(harness.pending_approvals(&ana(), &thread()).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_drawing_that_looks_like_a_decision_is_refused_in_plain_words() {
    let dir = tempfile::tempdir().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let card = json!({
        "title": "Invoice INV-10",
        "rows": [{ "label": "Status", "value": "Approved" }, { "label": "Next", "value": "Allow once" }]
    });
    let script =
        vec![ScriptedResponse::function_call("render_card", card), ScriptedResponse::text("done")];
    let harness = harness(script, sessions, &dir).await;

    let events = send(&harness, "Show me the invoice status").await;

    let drawn = drawings(&events);
    assert_eq!(drawn.len(), 1, "{drawn:?}");
    let (_, response, record) = &drawn[0];
    assert_eq!(record, &None, "a refused drawing is never recorded");
    let error = response["error"].as_str().unwrap_or_default();
    assert!(error.contains("genui.refused.looks_like_decision"), "{error}");
    assert!(error.contains("Not shown: it looks like a decision"), "{error}");
    assert!(
        error.contains("component row-0-value Text · text matches reserved word \"Approved\""),
        "{error}"
    );
}

#[tokio::test]
async fn versions_rise_across_redraws_patches_parallel_drawings_and_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let status = |status: &str| json!({ "surfaceId": "status", "title": "Status", "rows": [{ "label": "Invoice", "value": status }] });
    let script = vec![
        ScriptedResponse::function_call("render_form", invoice_form()),
        ScriptedResponse::text("drawn"),
        ScriptedResponse::function_call(
            "patch_surface",
            json!({ "surfaceId": "invoice-10", "data": [{ "path": "/to", "value": "ana@example.com" }] }),
        ),
        ScriptedResponse::text("patched"),
        ScriptedResponse::function_calls([
            ("render_card", status("Draft")),
            ("render_card", status("Ready")),
        ]),
        ScriptedResponse::text("both"),
    ];
    let first = harness(script, sessions.clone(), &dir).await;
    let mut versions = Vec::new();
    for text in ["draw", "patch", "two at once"] {
        for (_, response, record) in drawings(&send(&first, text).await) {
            let envelope = DrawnEnvelope::from_response(&response).expect("{response}");
            assert_eq!(record, Some(record_of(&envelope)));
            versions.push((envelope.surface_id.clone(), envelope.version));
        }
    }
    versions.sort();
    assert_eq!(
        versions,
        vec![
            ("invoice-10".to_string(), 1),
            ("invoice-10".to_string(), 2),
            ("status".to_string(), 1),
            ("status".to_string(), 2)
        ],
        "side-by-side drawings of one surface get their own versions"
    );

    // A restart: a new process over the same session store.
    let script = vec![
        ScriptedResponse::function_call(
            "patch_surface",
            json!({ "surfaceId": "invoice-10", "data": [{ "path": "/to", "value": "Allow once" }] }),
        ),
        ScriptedResponse::text("refused"),
        ScriptedResponse::function_call("render_form", invoice_form()),
        ScriptedResponse::text("again"),
        ScriptedResponse::function_call("delete_surface", json!({ "surfaceId": "invoice-10" })),
        ScriptedResponse::text("removed"),
    ];
    let second = harness(script, sessions, &dir).await;
    let refused = drawings(&send(&second, "patch it").await);
    let error = refused[0].1["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("component field-to TextField · value matches reserved word \"Allow once\""),
        "{error}"
    );
    let redrawn =
        DrawnEnvelope::from_response(&drawings(&send(&second, "draw again").await)[0].1).unwrap();
    assert_eq!(redrawn.version, 3, "versions survive the restart");
    let removed =
        DrawnEnvelope::from_response(&drawings(&send(&second, "remove it").await)[0].1).unwrap();
    assert_eq!((removed.version, removed.deleted, removed.draft()), (4, true, None));
}

/// Drawings made side by side in one turn each keep their stored version, so a session whose
/// history was compacted away still gives each surface a higher version after a restart.
#[tokio::test]
async fn side_by_side_drawings_each_keep_their_stored_version() {
    let dir = tempfile::tempdir().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let status = json!({ "surfaceId": "status", "title": "Status", "rows": [{ "label": "Invoice", "value": "Draft" }] });
    let both = || {
        vec![
            ScriptedResponse::function_calls([
                ("render_card", status.clone()),
                ("render_form", invoice_form()),
            ]),
            ScriptedResponse::text("both"),
        ]
    };
    let first = harness(both(), sessions.clone(), &dir).await;
    send(&first, "two at once").await;
    let state = sessions
        .get(adk_session::GetRequest {
            app_name: "home".into(),
            user_id: "ana".into(),
            session_id: "thread-1".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .state()
        .all();

    // The history is gone, as after a compaction; the session's state stays.
    let compacted = SessionId::new("thread-2").unwrap();
    sessions
        .create(adk_session::CreateRequest {
            app_name: "home".into(),
            user_id: "ana".into(),
            session_id: Some(compacted.to_string()),
            state,
        })
        .await
        .unwrap();
    let second = harness(both(), sessions, &dir).await;
    let stream = second.send(&ana(), &compacted, "two again").await.unwrap();
    let events: Vec<Event> =
        tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
            .await
            .expect("the run must finish")
            .into_iter()
            .map(|event| event.expect("event"))
            .collect();
    let mut versions: Vec<(String, u64)> = drawings(&events)
        .into_iter()
        .map(|(_, response, _)| {
            let envelope = DrawnEnvelope::from_response(&response).expect("{response}");
            (envelope.surface_id, envelope.version)
        })
        .collect();
    versions.sort();
    assert_eq!(versions, vec![("invoice-10".to_string(), 2), ("status".to_string(), 2)]);
}

/// A form's action names one of the drawing agent's own tools, so "Sending runs …" under its
/// button is true from the start; one bound to a tool the agent lacks is refused when drawn.
#[tokio::test]
async fn a_form_bound_to_a_tool_the_agent_lacks_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let mut form = invoice_form();
    form["actions"][0]["tool"] = json!("transfer_funds");
    let script =
        vec![ScriptedResponse::function_call("render_form", form), ScriptedResponse::text("done")];
    let harness = harness(script, sessions, &dir).await;

    let drawn = drawings(&send(&harness, "Draw the transfer form").await);

    let (_, response, record) = &drawn[0];
    assert_eq!(record, &None, "a refused drawing is never recorded");
    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error
            .contains("action 'send' runs 'transfer_funds', which is not one of the agent's tools"),
        "{error}"
    );
}
