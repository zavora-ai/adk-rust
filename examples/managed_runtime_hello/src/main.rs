//! Managed Agent Runtime — Hello World Smoke Test
//!
//! This example demonstrates the full managed agent runtime lifecycle:
//! 1. Create a `DefaultManagedAgentRuntime` with a `ScriptedLlm` (no API key needed)
//! 2. Register an agent definition
//! 3. Start a session
//! 4. Send a user message
//! 5. Collect and print the resulting `SessionEvent` stream
//!
//! This is fixture F-1 (hello) running end-to-end. The platform team can
//! clone this and smoke-test integration without any external dependencies.
//!
//! # Running
//!
//! ```bash
//! cargo run --manifest-path examples/managed_runtime_hello/Cargo.toml
//! ```

use std::sync::Arc;

use adk_managed::{
    DefaultManagedAgentRuntime, ManagedAgentRuntime, ManagedOwner, ModelResolver, ScriptedLlm,
    ScriptedTurn,
    resolver::ResolverResult,
    types::{ContentBlock, ManagedAgentDef, ModelRef, SessionEvent, UserEvent},
};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use futures::StreamExt;

/// A simple model resolver that always returns our scripted LLM.
///
/// In production, the `DefaultModelResolver` would resolve `ModelRef` to real
/// provider clients. For this smoke test, we bypass resolution entirely and
/// return a pre-built `ScriptedLlm` instance.
struct MockResolver {
    llm: Arc<dyn adk_core::Llm>,
}

#[async_trait]
impl ModelResolver for MockResolver {
    async fn resolve(&self, _model_ref: &ModelRef) -> ResolverResult<Arc<dyn adk_core::Llm>> {
        Ok(self.llm.clone())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Managed Agent Runtime — Hello World Smoke Test");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // ─── Step 1: Create the scripted LLM ─────────────────────────────────
    // The ScriptedLlm returns pre-defined responses in FIFO order.
    // No API key, no network calls, fully deterministic.
    let scripted_turns = vec![ScriptedTurn {
        text: Some(
            "Hello! I'm a managed agent running on ADK-Rust. How can I help you today?".to_string(),
        ),
        tool_calls: vec![],
    }];
    let scripted_llm = Arc::new(ScriptedLlm::new("scripted-hello-model", scripted_turns));

    println!("✓ Created ScriptedLlm with 1 pre-scripted turn");

    // ─── Step 2: Build the runtime ───────────────────────────────────────
    let resolver = Arc::new(MockResolver { llm: scripted_llm });
    let session_service = Arc::new(InMemorySessionService::new());

    let runtime = DefaultManagedAgentRuntime::new(resolver, session_service);

    println!("✓ Created DefaultManagedAgentRuntime (InMemory sessions)");

    // ─── Step 3: Register an agent definition ────────────────────────────
    let agent_def = ManagedAgentDef::new(
        "hello-agent",
        ModelRef::Shorthand("scripted-hello-model".to_string()),
    )
    .with_system("You are a friendly assistant.")
    .with_description("A simple hello world managed agent");

    let agent_handle = runtime.create(agent_def).await?;
    println!("✓ Registered agent: {:?}", agent_handle);

    // ─── Step 4: Start a session ─────────────────────────────────────────
    let owner = ManagedOwner::new("managed-runtime-hello", "demo-user")?;
    let session_handle = runtime.start_session(&agent_handle, &owner, None).await?;
    println!("✓ Started session: {:?}", session_handle);

    // Check initial status (should be Queued)
    let status = runtime.status(&session_handle).await?;
    println!("  Initial status: {:?}", status);

    // ─── Step 5: Subscribe to event stream ───────────────────────────────
    let mut event_stream = runtime.stream_events(&session_handle, None).await?;

    // ─── Step 6: Send a user message ─────────────────────────────────────
    let user_event = UserEvent::Message {
        content: vec![ContentBlock::Text { text: "Hello, agent!".to_string() }],
    };
    runtime.send_event(&session_handle, user_event).await?;
    println!("✓ Sent user message: \"Hello, agent!\"");
    println!();

    // ─── Step 7: Collect events ──────────────────────────────────────────
    println!("─── Event Stream ───────────────────────────────────────────");
    let mut event_count = 0;
    let timeout = tokio::time::Duration::from_secs(5);

    loop {
        match tokio::time::timeout(timeout, event_stream.next()).await {
            Ok(Some(event)) => {
                event_count += 1;
                print_event(event_count, &event);

                // Stop after status.idle (turn complete)
                if matches!(event, SessionEvent::StatusIdle { .. }) {
                    break;
                }
            }
            Ok(None) => {
                println!("  [stream ended]");
                break;
            }
            Err(_) => {
                println!("  [timeout waiting for events — this is expected after idle]");
                break;
            }
        }
    }

    println!("────────────────────────────────────────────────────────────");
    println!();

    // ─── Step 8: Check final status ──────────────────────────────────────
    let final_status = runtime.status(&session_handle).await?;
    println!("✓ Final session status: {:?}", final_status);
    println!("✓ Total events received: {event_count}");
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Smoke test PASSED — managed runtime is operational!");
    println!("═══════════════════════════════════════════════════════════════");

    Ok(())
}

/// Pretty-print a session event.
fn print_event(index: usize, event: &SessionEvent) {
    match event {
        SessionEvent::StatusRunning { seq } => {
            println!("  [{index}] status.running  (seq={seq})");
        }
        SessionEvent::StatusIdle { seq, stop_reason, .. } => {
            println!("  [{index}] status.idle     (seq={seq}, stop_reason={stop_reason:?})");
        }
        SessionEvent::Message { content, seq } => {
            let text: String = content
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::Text { text } = block { Some(text.as_str()) } else { None }
                })
                .collect::<Vec<_>>()
                .join("");
            println!("  [{index}] agent.message   (seq={seq}) \"{text}\"");
        }
        SessionEvent::ToolUse { name, seq, .. } => {
            println!("  [{index}] agent.tool_use  (seq={seq}) tool={name}");
        }
        SessionEvent::CustomToolUse { name, seq, .. } => {
            println!("  [{index}] agent.custom_tool_use (seq={seq}) tool={name}");
        }
        SessionEvent::McpToolUse { name, seq, .. } => {
            println!("  [{index}] agent.mcp_tool_use (seq={seq}) tool={name}");
        }
        SessionEvent::Error { code, message, seq } => {
            println!("  [{index}] error           (seq={seq}) {code}: {message}");
        }
        _ => {
            println!("  [{index}] <unknown event variant>");
        }
    }
}
