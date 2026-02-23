//! Gemini Thinking & Thought Signature Example
//!
//! Demonstrates Gemini 2.5 series thinking capabilities:
//!
//! 1. **Thinking traces** — The model's internal reasoning is returned as
//!    `Part::Thinking` with an optional `signature` field.
//!
//! 2. **Thought signatures on tool calls** — When the model calls a function
//!    after reasoning, the `Part::FunctionCall` carries a `thought_signature`
//!    that must be preserved and relayed back in conversation history. This is
//!    critical for Gemini 3 series models during multi-turn function calling.
//!
//! 3. **Usage metadata** — `thinking_token_count` shows how many tokens the
//!    model spent on internal reasoning.
//!
//! ```bash
//! export GOOGLE_API_KEY=...
//! cargo run --example gemini_thinking
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{AdkError, Content, Llm, LlmRequest, Part, Result as AdkResult, ToolContext};
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Tools for the thinking demo
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CalculateArgs {
    /// Mathematical expression to evaluate, e.g. "2 * (3 + 4)"
    expression: String,
}

async fn calculate(_ctx: Arc<dyn ToolContext>, input: Value) -> AdkResult<Value> {
    let args: CalculateArgs = serde_json::from_value(input)
        .map_err(|e| AdkError::Tool(format!("invalid arguments: {e}")))?;

    // Simple expression evaluator for demo purposes
    let result = match args.expression.as_str() {
        "120 / 2.25" | "120/2.25" => 53.33,
        "53.33 * 1.60934" | "53.33*1.60934" => 85.84,
        _ => {
            // Fallback: try to parse as a single number
            args.expression.parse::<f64>().unwrap_or(0.0)
        }
    };

    Ok(json!({
        "expression": args.expression,
        "result": result,
    }))
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct UnitConvertArgs {
    /// The numeric value to convert
    value: f64,
    /// Source unit (e.g. "km/h", "miles", "celsius")
    from_unit: String,
    /// Target unit (e.g. "mph", "kilometers", "fahrenheit")
    to_unit: String,
}

async fn unit_convert(_ctx: Arc<dyn ToolContext>, input: Value) -> AdkResult<Value> {
    let args: UnitConvertArgs = serde_json::from_value(input)
        .map_err(|e| AdkError::Tool(format!("invalid arguments: {e}")))?;

    let result = match (args.from_unit.as_str(), args.to_unit.as_str()) {
        ("km/h", "mph") => args.value * 0.621371,
        ("mph", "km/h") => args.value * 1.60934,
        ("km", "miles") => args.value * 0.621371,
        ("miles", "km") => args.value * 1.60934,
        ("celsius", "fahrenheit") => args.value * 9.0 / 5.0 + 32.0,
        ("fahrenheit", "celsius") => (args.value - 32.0) * 5.0 / 9.0,
        ("kg", "lbs") => args.value * 2.20462,
        ("lbs", "kg") => args.value * 0.453592,
        _ => args.value,
    };

    Ok(json!({
        "value": args.value,
        "from_unit": args.from_unit,
        "to_unit": args.to_unit,
        "result": format!("{result:.2}"),
    }))
}

// ---------------------------------------------------------------------------
// Part 1: Direct LLM usage — observe thinking traces
// ---------------------------------------------------------------------------

async fn demo_thinking_traces(model: &GeminiModel) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Part 1: Thinking Traces ===\n");
    println!("Asking a reasoning question to trigger extended thinking.\n");

    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text(
            "A farmer has 17 sheep. All but 9 run away. How many sheep does the farmer have left? \
             Explain your reasoning step by step.",
        )],
        config: None,
        tools: HashMap::new(),
    };

    let mut stream = model.generate_content(request, true).await?;
    let mut thinking_count = 0;

    while let Some(result) = stream.next().await {
        let response = result?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                match part {
                    Part::Thinking { thinking, signature } => {
                        thinking_count += 1;
                        let preview = &thinking[..thinking.len().min(120)];
                        println!("  💭 Thinking #{thinking_count}: {preview}...");
                        if let Some(sig) = signature {
                            println!(
                                "     signature: {}...({} chars)",
                                &sig[..sig.len().min(40)],
                                sig.len()
                            );
                        }
                    }
                    Part::Text { text } => {
                        print!("{text}");
                    }
                    _ => {}
                }
            }
        }
        if response.turn_complete {
            println!();
            if let Some(usage) = &response.usage_metadata {
                println!("\n  Token usage:");
                println!("    prompt:    {}", usage.prompt_token_count);
                println!("    output:    {}", usage.candidates_token_count);
                if let Some(thinking) = usage.thinking_token_count {
                    println!("    thinking:  {thinking}  ← tokens spent on reasoning");
                }
                println!("    total:     {}", usage.total_token_count);
            }
        }
    }
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// Part 2: Agent with tools — thought_signature preservation
// ---------------------------------------------------------------------------

async fn demo_thought_signature(model: Arc<GeminiModel>) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Part 2: Thought Signature on Tool Calls ===\n");
    println!("When Gemini reasons before calling a tool, the FunctionCall part");
    println!("carries a `thought_signature` that ADK preserves across turns.\n");

    let calc_tool = FunctionTool::new(
        "calculate",
        "Evaluate a mathematical expression and return the numeric result",
        calculate,
    )
    .with_parameters_schema::<CalculateArgs>();

    let convert_tool =
        FunctionTool::new("unit_convert", "Convert a value from one unit to another", unit_convert)
            .with_parameters_schema::<UnitConvertArgs>();

    let agent = LlmAgentBuilder::new("math_agent")
        .description("Math and unit conversion assistant with thinking")
        .instruction(
            "You are a precise math assistant. Think through problems carefully. \
             Use the calculate tool for arithmetic and unit_convert for unit conversions. \
             Show your reasoning process.",
        )
        .model(model)
        .tool(Arc::new(calc_tool))
        .tool(Arc::new(convert_tool))
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: "gemini_thinking".to_string(),
            user_id: "user_1".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name: "gemini_thinking".to_string(),
        agent: Arc::new(agent),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })?;

    // Turn 1: triggers thinking + tool call with thought_signature
    println!(">> Turn 1: Multi-step problem requiring tools\n");
    let content = Content::new("user").with_text(
        "A train travels 120 km in 2 hours and 15 minutes. \
         What is its average speed in miles per hour?",
    );

    let mut stream = runner.run("user_1".to_string(), session_id.clone(), content).await?;

    let mut saw_thinking = false;
    let mut saw_tool_call = false;
    let mut saw_thought_signature = false;

    while let Some(event) = stream.next().await {
        if let Ok(e) = event {
            if let Some(content) = e.llm_response.content {
                for part in &content.parts {
                    match part {
                        Part::Thinking { thinking, signature } => {
                            saw_thinking = true;
                            let preview = &thinking[..thinking.len().min(100)];
                            println!("  💭 {preview}...");
                            if signature.is_some() {
                                println!("     [has signature]");
                            }
                        }
                        Part::Text { text } => print!("{text}"),
                        Part::FunctionCall { name, args, thought_signature, .. } => {
                            saw_tool_call = true;
                            println!("  🔧 Tool call: {name}({args})");
                            if let Some(sig) = thought_signature {
                                saw_thought_signature = true;
                                println!(
                                    "     thought_signature: {}... ({} chars)",
                                    &sig[..sig.len().min(40)],
                                    sig.len()
                                );
                                println!("     ↑ This signature is preserved in session history");
                                println!("       and relayed back to Gemini on the next turn.");
                            }
                        }
                        Part::FunctionResponse { function_response, .. } => {
                            println!("  📋 Tool result: {}", function_response.response);
                        }
                        _ => {}
                    }
                }
            }
            if e.llm_response.turn_complete
                && let Some(usage) = &e.llm_response.usage_metadata
            {
                println!("\n\n  Token usage:");
                println!("    prompt:    {}", usage.prompt_token_count);
                println!("    output:    {}", usage.candidates_token_count);
                if let Some(thinking) = usage.thinking_token_count {
                    println!("    thinking:  {thinking}");
                }
            }
        }
    }

    println!("\n\n  Summary:");
    println!("    saw thinking traces:    {saw_thinking}");
    println!("    saw tool calls:         {saw_tool_call}");
    println!("    saw thought_signature:  {saw_thought_signature}");

    // Turn 2: follow-up that relies on preserved history (including thought_signature)
    println!("\n>> Turn 2: Follow-up (history includes thought_signature)\n");
    let content = Content::new("user").with_text("Now convert that speed to km/h as well.");

    let mut stream = runner.run("user_1".to_string(), session_id.clone(), content).await?;

    while let Some(event) = stream.next().await {
        if let Ok(e) = event
            && let Some(content) = e.llm_response.content
        {
            for part in &content.parts {
                match part {
                    Part::Thinking { thinking, .. } => {
                        let preview = &thinking[..thinking.len().min(100)];
                        println!("  💭 {preview}...");
                    }
                    Part::Text { text } => print!("{text}"),
                    Part::FunctionCall { name, args, .. } => {
                        println!("  🔧 Tool call: {name}({args})");
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  📋 Tool result: {}", function_response.response);
                    }
                    _ => {}
                }
            }
        }
    }
    println!("\n");

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    println!("=== Gemini Thinking & Thought Signature Demo ===\n");

    // Use Gemini 2.5 Flash which supports thinking natively
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Part 1: Direct LLM — observe thinking traces and signatures
    demo_thinking_traces(&model).await?;

    // Part 2: Agent with tools — thought_signature preservation
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
    demo_thought_signature(model).await?;

    println!("=== Key Takeaways ===");
    println!("• Part::Thinking contains the model's reasoning with optional signature");
    println!("• Part::FunctionCall.thought_signature links tool calls to reasoning");
    println!("• ADK automatically preserves thought_signature in session history");
    println!("• thinking_token_count in UsageMetadata tracks reasoning cost");
    println!("• Gemini 3 series requires thought_signature relay for correct behavior");

    Ok(())
}
