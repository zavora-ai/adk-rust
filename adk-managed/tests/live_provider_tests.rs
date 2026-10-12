//! The parked custom tool flow against real providers, across simulated restarts.
//!
//! Ignored by default: each test needs its provider's API key and feature.
//!
//! ```bash
//! set -a; source .env; set +a
//! cargo nextest run -p adk-managed --features openai,anthropic \
//!     --test live_provider_tests --run-ignored all
//! ```
//!
//! Every step runs on a new runtime instance over the same SQLite files, so each parked call
//! is resumed by a process that did not park it.

#![cfg(any(feature = "openai", feature = "anthropic"))]

use std::sync::Arc;
use std::time::Duration;

use adk_managed::durable::{DurableConfig, ManagedStores};
use adk_managed::resolver::DefaultModelResolver;
use adk_managed::state_store::{Durability, SessionServiceStateStore};
use adk_managed::types::{
    ConfirmationResult, ContentBlock, ManagedAgentDef, ModelRef, PermissionMode, PermissionPolicy,
    SessionEvent, StopReason, ToolConfig, UserEvent,
};
use adk_managed::{DefaultManagedAgentRuntime, ManagedAgentRuntime, ManagedOwner, SessionHandle};
use futures::StreamExt;
use serde_json::json;

struct Files(std::path::PathBuf);

impl Files {
    fn url(&self, file: &str) -> String {
        format!("sqlite://{}?mode=rwc", self.0.join(file).display())
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A new runtime instance over the stores in `files`, as a restarted process opens them.
async fn instance(files: &Files) -> DefaultManagedAgentRuntime {
    let sessions = adk_session::SqliteSessionService::new(&files.url("sessions.db")).await.unwrap();
    sessions.migrate().await.unwrap();
    let state = adk_session::SqliteSessionService::new(&files.url("state.db")).await.unwrap();
    state.migrate().await.unwrap();
    let runs = adk_session::SqliteRunStore::new(&files.url("runs.db")).await.unwrap();
    runs.migrate().await.unwrap();
    let decisions =
        adk_session::SqliteApprovalStore::new(&files.url("approvals.db")).await.unwrap();
    decisions.migrate().await.unwrap();
    let stores = ManagedStores::new(
        Arc::new(SessionServiceStateStore::new(Arc::new(state), Durability::CrashDurable)),
        Arc::new(runs),
        Arc::new(decisions),
    );
    DefaultManagedAgentRuntime::new(Arc::new(DefaultModelResolver::new()), Arc::new(sessions))
        .with_stores(stores)
        .with_durable_config(
            DurableConfig::default().with_poll_interval(Duration::from_millis(100)),
        )
}

struct Turn {
    events: Vec<SessionEvent>,
    stop: Option<StopReason>,
}

impl Turn {
    fn text(&self) -> String {
        self.events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Message { content, .. } => Some(content),
                _ => None,
            })
            .flatten()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn waiting_on(&self) -> Vec<String> {
        match &self.stop {
            Some(StopReason::RequiresAction { event_ids }) => event_ids.clone(),
            other => panic!("expected requires_action, got {other:?}: {:?}", self.events),
        }
    }

    fn called(&self, tool: &str) -> bool {
        self.events.iter().any(|event| match event {
            SessionEvent::CustomToolUse { name, .. } | SessionEvent::ToolUse { name, .. } => {
                name == tool
            }
            _ => false,
        })
    }
}

/// Sends `event` to a fresh instance and reads the turn it starts.
async fn step(
    files: &Files,
    session: &SessionHandle,
    last_seq: &mut u64,
    event: UserEvent,
) -> Turn {
    let runtime = instance(files).await;
    let mut stream =
        runtime.stream_events(session, Some(*last_seq)).await.expect("session rebuilt");
    runtime.send_event(session, event).await.expect("event accepted");
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(120), stream.next())
            .await
            .expect("the provider answered within 120 s")
            .expect("stream open");
        *last_seq = adk_managed::get_seq(&event);
        if let SessionEvent::Error { message, .. } = &event {
            panic!("session error: {message}");
        }
        if let SessionEvent::StatusIdle { stop_reason, .. } = event {
            return Turn { events, stop: stop_reason };
        }
        events.push(event);
    }
    // `runtime` ends here, before the next step opens another instance.
}

async fn order_desk(model: &str) {
    let files =
        Files(std::env::temp_dir().join(format!("adk-managed-live-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&files.0).unwrap();

    let def = ManagedAgentDef::new("order-desk", ModelRef::Shorthand(model.into()))
        .with_system(
            "You are an order desk. Look up orders with get_order_status and refund them with \
             issue_refund. Always call the tool; never guess an order's status. Answer in one \
             sentence that quotes the tool's result.",
        )
        .with_tools(vec![
            ToolConfig::Custom {
                name: "get_order_status".into(),
                description: Some("Look up the shipping status of an order".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": { "order_id": { "type": "string" } },
                    "required": ["order_id"]
                }),
            },
            ToolConfig::Custom {
                name: "issue_refund".into(),
                description: Some("Refund an order in full".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": { "order_id": { "type": "string" } },
                    "required": ["order_id"]
                }),
            },
        ])
        .with_permission_policy(PermissionPolicy {
            default: PermissionMode::AutoApprove,
            tools: [("issue_refund".to_string(), PermissionMode::Prompt)].into(),
        });

    let session = {
        let runtime = instance(&files).await;
        let agent = runtime.create(def).await.expect("the model resolves");
        let owner = ManagedOwner::new("live-orders", "customer-1").unwrap();
        runtime.start_session(&agent, &owner, None).await.unwrap()
    };
    let mut seq = 0;
    let message =
        |text: &str| UserEvent::Message { content: vec![ContentBlock::Text { text: text.into() }] };

    // 1. The model calls the custom tool; the turn parks.
    let turn = step(&files, &session, &mut seq, message("What is the status of order 42?")).await;
    assert!(turn.called("get_order_status"), "{:?}", turn.events);
    let call = turn.waiting_on().remove(0);
    println!("[{model}] parked custom tool call {call}");

    // 2. A restarted instance takes the result and the model answers from it.
    let result = UserEvent::CustomToolResult {
        custom_tool_use_id: call,
        content: vec![ContentBlock::Text {
            text: "Order 42 shipped on 2026-10-10 with DHL, tracking DHL-7781.".into(),
        }],
    };
    let turn = step(&files, &session, &mut seq, result).await;
    println!("[{model}] after restart: {}", turn.text());
    assert!(matches!(turn.stop, Some(StopReason::EndTurn)), "{:?}", turn.stop);
    assert!(turn.text().contains("DHL-7781"), "the model used the result: {}", turn.text());

    // 3. A refund needs confirmation; the held call parks.
    let turn = step(&files, &session, &mut seq, message("Please refund order 42.")).await;
    assert!(turn.called("issue_refund"), "{:?}", turn.events);
    let held = turn.waiting_on().remove(0);
    println!("[{model}] held for confirmation: {held}");

    // 4. Another restart allows it; the custom tool then waits on the client.
    let allow = UserEvent::ToolConfirmation {
        tool_use_id: held,
        result: ConfirmationResult::Allow,
        deny_message: None,
    };
    let turn = step(&files, &session, &mut seq, allow).await;
    let call = turn.waiting_on().remove(0);
    println!("[{model}] refund approved, awaiting the client's result for {call}");

    // 5. And a third instance delivers the refund's result.
    let result = UserEvent::CustomToolResult {
        custom_tool_use_id: call,
        content: vec![ContentBlock::Text { text: "Refund R-19 issued for 89.00 EUR.".into() }],
    };
    let turn = step(&files, &session, &mut seq, result).await;
    println!("[{model}] refund: {}", turn.text());
    assert!(matches!(turn.stop, Some(StopReason::EndTurn)), "{:?}", turn.stop);
    assert!(turn.text().contains("R-19"), "the model used the result: {}", turn.text());
}

#[cfg(feature = "openai")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs OPENAI_API_KEY"]
async fn openai_resumes_parked_calls_across_restarts() {
    order_desk("gpt-5.4-mini").await;
}

#[cfg(feature = "anthropic")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ANTHROPIC_API_KEY"]
async fn anthropic_resumes_parked_calls_across_restarts() {
    order_desk("claude-haiku-5-5").await;
}
