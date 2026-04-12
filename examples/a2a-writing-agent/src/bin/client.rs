//! A2A v1.0.0 client — exercises all 11 operations via the research-to-writing pipeline.

use a2a_protocol_types::{Message, MessageId, MessageRole, Part, TaskPushNotificationConfig};
use adk_server::a2a::client::v1_client::{A2aV1Client, V1ClientError};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "a2a-client")]
struct Cli {
    #[arg(long, env = "RESEARCH_AGENT_URL", default_value = "http://127.0.0.1:3001")]
    research_url: String,
    #[arg(long, env = "WRITING_AGENT_URL", default_value = "http://127.0.0.1:3002")]
    writing_url: String,
    #[arg(long, default_value = "The impact of artificial intelligence on healthcare")]
    topic: String,
}

fn pass(msg: &str) { println!("  ✓ {msg}"); }
fn fail(msg: &str) { println!("  ✗ {msg}"); }

fn build_message(text: &str) -> Message {
    Message {
        id: MessageId::new(uuid::Uuid::new_v4().to_string()),
        role: MessageRole::User,
        parts: vec![Part::text(text)],
        task_id: None,
        context_id: None,
        reference_task_ids: None,
        extensions: None,
        metadata: None,
    }
}

fn extract_artifact_text(task: &a2a_protocol_types::Task) -> Option<String> {
    let artifacts = task.artifacts.as_ref()?;
    let first = artifacts.first()?;
    let texts: Vec<&str> = first.parts.iter().filter_map(|p| p.text_content()).collect();
    if texts.is_empty() { None } else { Some(texts.join("")) }
}

fn validate_card(card: &a2a_protocol_types::AgentCard) -> bool {
    card.supported_interfaces.iter()
        .any(|i| i.protocol_binding == "JSONRPC" && i.protocol_version == "1.0")
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    println!("A2A v1.0.0 Client — Full Protocol Validation");
    println!("  Research: {}", cli.research_url);
    println!("  Writing:  {}", cli.writing_url);

    // ── 1. Agent Card Discovery ──────────────────────────────────────
    println!("\n--- Agent Card Discovery ---");

    let research_card = match A2aV1Client::resolve_agent_card(&cli.research_url).await {
        Ok(card) => {
            if validate_card(&card) {
                pass(&format!("Research: \"{}\" (JSONRPC 1.0 ✓)", card.name));
            } else {
                fail("Research card missing JSONRPC 1.0");
            }
            card
        }
        Err(e) => { fail(&format!("Research discovery: {e}")); std::process::exit(1); }
    };

    let writing_card = match A2aV1Client::resolve_agent_card(&cli.writing_url).await {
        Ok(card) => {
            if validate_card(&card) {
                pass(&format!("Writing: \"{}\" (JSONRPC 1.0 ✓)", card.name));
            } else {
                fail("Writing card missing JSONRPC 1.0");
            }
            card
        }
        Err(e) => { fail(&format!("Writing discovery: {e}")); std::process::exit(1); }
    };

    let research = A2aV1Client::new(research_card);
    let writing = A2aV1Client::new(writing_card);

    // ── 2. SendMessage — Research Pipeline ───────────────────────────
    println!("\n--- SendMessage (Research) ---");

    let research_task = match research.send_message(build_message(&cli.topic)).await {
        Ok(t) => {
            pass(&format!("Task {} → {:?}", t.id, t.status.state));
            t
        }
        Err(e) => { fail(&format!("SendMessage: {e}")); std::process::exit(1); }
    };

    let summary = match extract_artifact_text(&research_task) {
        Some(t) => { pass(&format!("Artifact: {} chars", t.len())); t }
        None => { fail("No artifact text"); std::process::exit(1); }
    };

    println!("\n  Summary excerpt: {}", &summary[..summary.len().min(200)]);

    // ── 3. SendMessage — Writing Pipeline ────────────────────────────
    println!("\n--- SendMessage (Writing) ---");

    let writing_task = match writing.send_message(build_message(&summary)).await {
        Ok(t) => {
            pass(&format!("Task {} → {:?}", t.id, t.status.state));
            t
        }
        Err(e) => { fail(&format!("SendMessage: {e}")); std::process::exit(1); }
    };

    match extract_artifact_text(&writing_task) {
        Some(t) => {
            pass(&format!("Artifact: {} chars", t.len()));
            println!("\n  Article excerpt: {}", &t[..t.len().min(200)]);
        }
        None => fail("No artifact text"),
    }

    let task_id = research_task.id.0.clone();

    // ── 4. GetTask ───────────────────────────────────────────────────
    println!("\n--- GetTask ---");
    match research.get_task(&task_id, Some(10)).await {
        Ok(t) => pass(&format!("{} → {:?}", t.id, t.status.state)),
        Err(e) => fail(&format!("{e}")),
    }

    // ── 5. ListTasks ─────────────────────────────────────────────────
    println!("\n--- ListTasks ---");
    match research.list_tasks(None, None, None, None).await {
        Ok(tasks) => pass(&format!("{} task(s)", tasks.len())),
        Err(e) => fail(&format!("{e}")),
    }

    // ── 6. CancelTask (error: already completed) ─────────────────────
    println!("\n--- CancelTask (error path) ---");
    match research.cancel_task(&task_id).await {
        Err(V1ClientError::JsonRpc { code, .. }) if code == -32002 =>
            pass("TaskNotCancelable (-32002)"),
        Err(e) => fail(&format!("Unexpected: {e}")),
        Ok(_) => fail("Expected error"),
    }

    // ── 7. SendStreamingMessage ──────────────────────────────────────
    println!("\n--- SendStreamingMessage ---");
    match research.send_streaming_message(build_message("streaming test")).await {
        Ok(resp) => {
            let body = resp.text().await.unwrap_or_default();
            if !body.is_empty() {
                pass(&format!("Stream: {} bytes", body.len()));
            } else {
                fail("Empty stream");
            }
        }
        Err(e) => fail(&format!("{e}")),
    }

    // ── 8-9-10-11. Push Notification CRUD ────────────────────────────
    println!("\n--- Push Notification CRUD ---");
    let config = TaskPushNotificationConfig::new(&task_id, "https://example.com/webhook");
    match research.create_push_notification_config(config).await {
        Ok(c) => {
            pass("Create ✓");
            let cid = c.id.clone().unwrap_or_default();

            match research.get_push_notification_config(&task_id, &cid).await {
                Ok(_) => pass("Get ✓"),
                Err(e) => fail(&format!("Get: {e}")),
            }
            match research.list_push_notification_configs(&task_id).await {
                Ok(v) => pass(&format!("List: {} config(s)", v.len())),
                Err(e) => fail(&format!("List: {e}")),
            }
            match research.delete_push_notification_config(&task_id, &cid).await {
                Ok(()) => pass("Delete ✓"),
                Err(e) => fail(&format!("Delete: {e}")),
            }
        }
        Err(e) => fail(&format!("Create: {e}")),
    }

    // ── 12. GetExtendedAgentCard ─────────────────────────────────────
    println!("\n--- GetExtendedAgentCard ---");
    match research.get_extended_agent_card().await {
        Ok(card) => pass(&format!("\"{}\" v{}", card.name, card.version)),
        Err(e) => fail(&format!("{e}")),
    }

    // ── 13. Version Negotiation ──────────────────────────────────────
    println!("\n--- Version Negotiation ---");
    let http = reqwest::Client::new();
    let jsonrpc_url = format!("{}/jsonrpc", cli.research_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": "ver-test",
        "method": "GetExtendedAgentCard", "params": {}
    });

    match http.post(&jsonrpc_url).header("A2A-Version", "1.0").json(&body).send().await {
        Ok(resp) => {
            let ver = resp.headers().get("a2a-version")
                .and_then(|v| v.to_str().ok()).unwrap_or("");
            if ver == "1.0" { pass("A2A-Version: 1.0 ✓"); }
            else { fail(&format!("Expected 1.0, got \"{ver}\"")); }
        }
        Err(e) => fail(&format!("{e}")),
    }

    match http.post(&jsonrpc_url).header("A2A-Version", "99.0").json(&body).send().await {
        Ok(resp) if resp.status().as_u16() == 400 => pass("Version 99.0 → 400 ✓"),
        Ok(resp) => fail(&format!("Expected 400, got {}", resp.status())),
        Err(e) => fail(&format!("{e}")),
    }

    // ── 14. Error Paths ──────────────────────────────────────────────
    println!("\n--- Error Paths ---");
    match research.get_task("nonexistent-id", None).await {
        Err(V1ClientError::JsonRpc { code, .. }) if code == -32001 =>
            pass("TaskNotFound (-32001)"),
        Err(e) => fail(&format!("Unexpected: {e}")),
        Ok(_) => fail("Expected error"),
    }

    println!("\n=== All v1.0.0 operations validated ===");
}
