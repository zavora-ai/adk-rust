//! # CodeAct × Monty agent example — shopping cart
//!
//! Runs the ADK-Rust [`CodeActAgent`] against a real Python interpreter
//! ([`MontyRuntime`](adk_codeact_monty::MontyRuntime)) instead of emitting one
//! tool call at a time. The model writes a single Python script that:
//!
//! 1. reads its inputs from the environment with `os.getenv` (which user, which
//!    tax region) — serviced in-place by the host, never as a tool call,
//! 2. calls the `fetch_cart` tool (via `call_tool`) to load that user's cart,
//! 3. **loops in Python** to sum line items,
//! 4. calls the `tax_rate` tool, applies it with ordinary arithmetic, and
//! 5. stamps the result with `datetime.now()` and returns a tagged
//!    `final_result` value.
//!
//! The environment (`os.getenv` / `os.environ`) and clock (`datetime.now()` /
//! `date.today()`) are **OS functions**: the host services them in place against
//! the policy configured on the [`MontyRuntime`] builder, so they never become
//! tools and never pause the agent loop. The two `call_tool` invocations, by
//! contrast, become two suspend/resume cycles in Monty — the interpreter pauses
//! at each call boundary, the agent runs the tool, and execution resumes exactly
//! where it left off. No container, no subprocess, no API key required: a small
//! deterministic model (`DemoLlm`) emits the script so the example always runs
//! offline. Swap `DemoLlm` for an `adk-model` provider to drive it with a real
//! LLM — the wiring is identical.
//!
//! ## Run
//!
//! Run from inside this directory so its `rust-toolchain.toml` (rustc 1.95+,
//! required by Monty) is picked up automatically:
//!
//! ```bash
//! cd examples/codeact_monty_agent && cargo run
//! ```

use std::sync::Arc;

use adk_agent::codeact::CodeActAgent;
use adk_codeact_monty::MontyRuntime;
use adk_core::{
    Agent, Content, Llm, LlmRequest, LlmResponseStream, Part, SessionId, ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

const APP_NAME: &str = "codeact-monty-agent-example";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    println!("=== ADK-Rust CodeAct × Monty (Python) example ===\n");

    // Grant the script an explicit environment (read by `os.getenv`) and keep
    // the host clock enabled (the builder default) so `datetime.now()` works.
    // Both are OS functions: serviced in place, never tools, never pausing the
    // loop. Everything else stays sandboxed (no filesystem, no network).
    let runtime = MontyRuntime::builder()
        .environ_var("CART_USER", "u-42")
        .environ_var("TAX_REGION", "CA")
        .system_clock(true)
        .build();

    let agent: Arc<dyn Agent> = Arc::new(
        CodeActAgent::builder()
            .name("cart_assistant")
            .model(Arc::new(DemoLlm))
            .runtime(Arc::new(runtime))
            .instruction("Price the user's shopping cart by writing Python.")
            .tool(Arc::new(FetchCartTool))
            .tool(Arc::new(TaxRateTool))
            .build()?,
    );

    let session_id = "session-1";
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await?;
    let runner =
        Runner::builder().app_name(APP_NAME).agent(agent).session_service(sessions).build()?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text("What's the total for cart u-42, including CA tax?"),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            let text: String = content.parts.iter().filter_map(Part::text).collect();
            if !text.is_empty() {
                println!("[{}]\n{text}\n", event.author);
            }
        }
    }

    println!("Done.");
    Ok(())
}

/// A deterministic model that writes one Python script, so the example runs
/// offline. A real deployment would use an `adk-model` provider instead.
///
/// The script exercises the whole point of a Python `CodeRuntime`: it reads the
/// environment with `os.getenv`, stamps the result with `datetime.now()`, calls
/// two tools, *and* does real work between them (a `for` loop, indexing,
/// arithmetic) — none of which a one-tool-call-per-turn agent could express in a
/// single turn. The OS calls are serviced in place by the host policy, so only
/// the two `call_tool` invocations suspend/resume the interpreter.
struct DemoLlm;

#[async_trait]
impl Llm for DemoLlm {
    fn name(&self) -> &str {
        "demo-python"
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        // Build the script line-by-line so Python indentation is preserved
        // exactly (a `\`-continued string literal would eat the leading spaces).
        let script = [
            "```python",
            "import os",
            "from datetime import datetime",
            "# Read inputs from the granted environment (serviced in place).",
            "user = os.getenv(\"CART_USER\", \"u-0\")",
            "region = os.getenv(\"TAX_REGION\", \"US\")",
            "cart = call_tool(\"fetch_cart\", {\"user_id\": user})",
            "subtotal = 0.0",
            "for item in cart[\"items\"]:",
            "    subtotal = subtotal + item[\"price\"] * item[\"qty\"]",
            "rate = call_tool(\"tax_rate\", {\"region\": region})",
            "total = subtotal * (1 + rate)",
            "{",
            "    \"type\": \"final_result\",",
            "    \"value\": {",
            "        \"user\": user,",
            "        \"region\": region,",
            "        \"lines\": len(cart[\"items\"]),",
            "        \"subtotal\": subtotal,",
            "        \"tax_rate\": rate,",
            "        \"total\": total,",
            "        \"priced_at\": datetime.now().isoformat(),",
            "    },",
            "}",
            "```",
        ]
        .join("\n");
        let response = adk_core::model::LlmResponse::new(Content::new("model").with_text(script));
        Ok(Box::pin(futures::stream::once(async move { Ok(response) })))
    }
}

/// Returns the line items in a user's shopping cart.
struct FetchCartTool;

#[async_trait]
impl adk_core::Tool for FetchCartTool {
    fn name(&self) -> &str {
        "fetch_cart"
    }

    fn description(&self) -> &str {
        "Fetch the line items in a user's shopping cart by user id."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "user_id": { "type": "string" } },
            "required": ["user_id"],
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(json!({
            "items": [
                { "name": "keyboard", "price": 80.0, "qty": 1 },
                { "name": "usb-c cable", "price": 12.0, "qty": 2 },
                { "name": "mouse", "price": 28.0, "qty": 1 },
            ],
        }))
    }
}

/// Returns the sales-tax rate for a region.
struct TaxRateTool;

#[async_trait]
impl adk_core::Tool for TaxRateTool {
    fn name(&self) -> &str {
        "tax_rate"
    }

    fn description(&self) -> &str {
        "Return the sales-tax rate (as a fraction) for a region code."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "region": { "type": "string" } },
            "required": ["region"],
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let region = args.get("region").and_then(Value::as_str).unwrap_or("");
        let rate = match region {
            "CA" => 0.0725,
            "NY" => 0.08875,
            _ => 0.0,
        };
        Ok(json!(rate))
    }
}
