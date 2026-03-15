//! Multi-Turn Tool Conversation Example
//!
//! Demonstrates that tool responses are correctly preserved across conversation
//! turns. In a multi-turn session, Turn 1 may invoke a tool and receive a
//! response. When Turn 2 arrives, the session history must carry the original
//! tool response with its `"function"` role intact — otherwise the LLM provider
//! rejects the request or produces incorrect results.
//!
//! This example uses a simple "inventory" scenario:
//!   Turn 1: "How many widgets are in stock?" → agent calls `check_inventory`
//!   Turn 2: "And how about gadgets?"         → agent calls `check_inventory` again,
//!            but the LLM also sees Turn 1's tool call/response in history.
//!
//! Run with Gemini (default):
//! ```bash
//! export GOOGLE_API_KEY=...
//! cargo run --example multi_turn_tool
//! ```
//!
//! Run with OpenAI:
//! ```bash
//! export OPENAI_API_KEY=...
//! cargo run --example multi_turn_tool --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{AdkError, Result as AdkResult, ToolContext};
use adk_tool::FunctionTool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Tool: check_inventory
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct InventoryArgs {
    /// Name of the product to look up
    product: String,
}

async fn check_inventory(_ctx: Arc<dyn ToolContext>, input: Value) -> AdkResult<Value> {
    let args: InventoryArgs = serde_json::from_value(input)
        .map_err(|e| AdkError::Tool(format!("Invalid arguments: {e}")))?;

    // Simulated inventory database
    let (quantity, warehouse) = match args.product.to_lowercase().as_str() {
        "widgets" | "widget" => (142, "Warehouse A"),
        "gadgets" | "gadget" => (37, "Warehouse B"),
        "gizmos" | "gizmo" => (0, "N/A — out of stock"),
        _ => (0, "Unknown product"),
    };

    Ok(json!({
        "product": args.product,
        "quantity": quantity,
        "warehouse": warehouse,
    }))
}

// ---------------------------------------------------------------------------
// Tool: place_order
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct OrderArgs {
    /// Name of the product to order
    product: String,
    /// Quantity to order
    quantity: u32,
}

async fn place_order(_ctx: Arc<dyn ToolContext>, input: Value) -> AdkResult<Value> {
    let args: OrderArgs = serde_json::from_value(input)
        .map_err(|e| AdkError::Tool(format!("Invalid arguments: {e}")))?;

    // Simulated order placement
    let order_id = format!("ORD-{:04}", args.quantity * 7 + 1000);

    Ok(json!({
        "order_id": order_id,
        "product": args.product,
        "quantity": args.quantity,
        "status": "confirmed",
    }))
}

// ---------------------------------------------------------------------------
// Model setup (Gemini default, OpenAI behind feature flag)
// ---------------------------------------------------------------------------

fn create_model() -> anyhow::Result<Arc<dyn adk_core::Llm>> {
    #[cfg(feature = "openai")]
    {
        let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
        let model = adk_model::openai::OpenAIClient::new(adk_model::openai::OpenAIConfig::new(
            api_key,
            "gpt-4o-mini",
        ))?;
        return Ok(Arc::new(model));
    }

    #[cfg(not(feature = "openai"))]
    {
        let api_key = std::env::var("GOOGLE_API_KEY")
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");
        let model = adk_model::gemini::GeminiModel::new(&api_key, "gemini-2.5-flash")?;
        Ok(Arc::new(model))
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let model = create_model()?;

    let inventory_tool = FunctionTool::new(
        "check_inventory",
        "Check the current stock level and warehouse location for a product",
        check_inventory,
    )
    .with_parameters_schema::<InventoryArgs>();

    let order_tool = FunctionTool::new(
        "place_order",
        "Place an order for a product with a given quantity",
        place_order,
    )
    .with_parameters_schema::<OrderArgs>();

    let agent = LlmAgentBuilder::new("inventory_agent")
        .description("Inventory management assistant")
        .instruction(
            "You are an inventory assistant. Use check_inventory to look up stock levels \
             and place_order to create orders. Always check inventory before placing an order. \
             Be concise in your responses.",
        )
        .model(model)
        .tool(Arc::new(inventory_tool))
        .tool(Arc::new(order_tool))
        .build()?;

    println!("=== Multi-Turn Tool Conversation Example ===");
    println!("This showcases correct tool response role preservation across turns.");
    println!("Try a conversation like:");
    println!("  Turn 1: How many widgets are in stock?");
    println!("  Turn 2: And how about gadgets?");
    println!("  Turn 3: Order 10 of whichever has more stock.");
    println!();

    adk_cli::console::run_console(
        Arc::new(agent),
        "multi_turn_tool_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
